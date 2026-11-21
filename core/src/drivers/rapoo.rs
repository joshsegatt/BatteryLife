//! Rapoo VT gaming mice (VID 24AE). The battery is pushed periodically on FF00:0002; nothing is
//! ever sent to the device:
//!  * Nordic 54L firmware: report 07 [07][mode][..][7] state [8] percent .. [13..14] mouse pid;
//!    connection = low nibble of byte 1 (0 receiver, 1 Bluetooth, 2 cable).
//!  * VT3 Pro / VT1 Pro Max / VT3 Pro Max firmware (known ids only): 7-byte [BB][B0][mode][dpi]
//!    [dpi][state][percent]; 7F instead of a level while charging means unknown.
//!
//! State 1 = discharging, 2 = charging, 0 = invalid / transition.
//! Handles stay open between polls so Windows buffers the pushed reports; a fresh handle waits for
//! one, capped so the driver stays inside its time budget (BatteryHub waits up to 5 s). Every
//! product of 24AE with the status collection is read (BatteryHub scans the whole vendor).
//!
//! Ported from BatteryHub (src/drivers/rapoo.cpp and rapoo_protocol.h, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0, from the official Rapoo
//! Hub (hub.rapoo.cn), github.com/PyFlat/Device-Battery-Info (RapooProtocol.cs) and
//! github.com/zbndev/rapoo_battery.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::hid::{self, BUDGET, Coll, Dev, Hid, find, hex4, left, pids};
use crate::registry::Match;

const VID: u16 = 0x24AE;

pub static MATCHES: &[Match] = &[Match {
    vid: VID,
    pids: &[],
}];

/// (cable, receiver, name), from Rapoo's manifests. Support is decided by the collection and a
/// valid status frame, so other models on the same protocol work without this table.
const MODELS: [(u16, u16, &str); 17] = [
    (0x4610, 0x1410, "Rapoo VT0"),
    (0x4620, 0x1420, "Rapoo VT0 Max"),
    (0x4615, 0x1415, "Rapoo VT1"),
    (0x4644, 0x1444, "Rapoo VT1 Max"),
    (0x4622, 0x1422, "Rapoo VT2"),
    (0x4623, 0x1423, "Rapoo VT2 Max"),
    (0x4612, 0x1412, "Rapoo VT3"),
    (0x4617, 0x1417, "Rapoo VT3 Max (Gen 2)"),
    (0x4B06, 0x1B06, "Rapoo VT3 Max V2"),
    (0x4611, 0x1411, "Rapoo VT3S"),
    (0x4621, 0x1421, "Rapoo VT3S Max"),
    (0x4613, 0x1413, "Rapoo VT7"),
    (0x4616, 0x1416, "Rapoo VT7 Max"),
    (0x4614, 0x1414, "Rapoo VT9 (Gen 2)"),
    (0x4618, 0x1418, "Rapoo VT9 Max"),
    (0x4415, 0x1215, "Rapoo VT3 Pro"),
    (0x4433, 0x1233, "Rapoo VT1 Pro Max"),
];

fn find_model(pid: u16) -> Option<(u16, u16, &'static str)> {
    MODELS.into_iter().find(|m| pid == m.0 || pid == m.1)
}

/// Mice confirmed to send the 7-byte BB B0 status report.
fn is_legacy(pid: u16) -> bool {
    matches!(pid, 0x1215 | 0x4415 | 0x1233 | 0x4433 | 0x1244)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Connection {
    Unknown,
    Receiver,
    Bluetooth,
    Cable,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Profile {
    Nordic54L,
    LegacyVt3Pro,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Battery {
    pub percent: Option<u8>,
    pub charging: bool,
    pub connection: Connection,
    pub mouse_pid: u16,
}

/// Decodes one passive status report (report id included).
pub fn decode(data: &[u8], profile: Profile) -> Option<Battery> {
    let mut b = Battery {
        percent: None,
        charging: false,
        connection: Connection::Unknown,
        mouse_pid: 0,
    };
    let (state, percent);
    match (profile, data.first()) {
        (Profile::Nordic54L, Some(0x07)) => {
            if data.len() < 9 {
                return None;
            }
            b.connection = match data[1] & 0x0F {
                0 => Connection::Receiver,
                1 => Connection::Bluetooth,
                2 => Connection::Cable,
                _ => return None,
            };
            state = data[7];
            percent = data[8];
            if data.len() >= 15 {
                b.mouse_pid = u16::from_le_bytes([data[13], data[14]]);
            }
        }
        (Profile::LegacyVt3Pro, Some(0xBB)) => {
            if data.len() < 7 || data[1] != 0xB0 {
                return None;
            }
            state = data[5];
            percent = data[6];
        }
        _ => return None,
    }
    if state != 1 && state != 2 {
        return None;
    }
    if percent > 100 && !(profile == Profile::LegacyVt3Pro && state == 2 && percent == 0x7F) {
        return None;
    }
    b.charging = state == 2;
    b.percent = (percent <= 100).then_some(percent);
    Some(b)
}

/// Open status collections between polls, by path.
pub type Handles = HashMap<String, Box<dyn Dev>>;
static HANDLES: Mutex<Option<Handles>> = Mutex::new(None);

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    let mut handles = HANDLES.lock().unwrap_or_else(|e| e.into_inner());
    hid::run(api, infos, |hid, colls| {
        poll_with(colls, hid, handles.get_or_insert_with(HashMap::new))
    })
}

/// Reports queued since the last poll; the newest valid one wins.
fn drain_queued(d: &mut dyn Dev, profile: Profile) -> Option<Battery> {
    let mut got = None;
    for _ in 0..64 {
        let Some(r) = d.read(Duration::from_millis(30)) else {
            break;
        };
        if let Some(b) = decode(&r, profile) {
            got = Some(b);
        }
    }
    got
}

fn wait_battery(d: &mut dyn Dev, profile: Profile, end: Instant) -> Option<Battery> {
    loop {
        let left = left(end);
        if left.is_zero() {
            return None;
        }
        if let Some(b) = decode(&d.read(left)?, profile) {
            return Some(b);
        }
    }
}

fn read_battery(
    handles: &mut Handles,
    path: &str,
    c: &Coll,
    io: &mut dyn Hid,
    profile: Profile,
    fresh: bool,
    end: Instant,
) -> Option<Battery> {
    let d = handles.get_mut(path)?;
    if !fresh && let Some(b) = drain_queued(d.as_mut(), profile) {
        return Some(b);
    }
    // A pushed report can take several seconds (VT1 Pro Max ~4 s; VT9 Gen 2 5-10 s on first open).
    let start = Instant::now();
    let end = end.min(start + Duration::from_millis(5000));
    if let Some(b) = wait_battery(d.as_mut(), profile, end) {
        return Some(b);
    }
    // A persistent handle that failed at once (receiver reset between polls): reopen once.
    if !fresh && start.elapsed() < Duration::from_millis(50) {
        handles.remove(path);
        let mut d = io.open(c)?;
        let b = wait_battery(d.as_mut(), profile, end);
        handles.insert(path.to_string(), d);
        return b;
    }
    None
}

fn poll_with(colls: &[Coll], io: &mut dyn Hid, handles: &mut Handles) -> Vec<Device> {
    let end = Instant::now() + BUDGET;
    let mut present: Vec<String> = Vec::new();
    let mut out: Vec<Device> = Vec::new();
    for pid in pids(colls, VID) {
        for c in find(colls, VID, pid, |c| {
            // The other vendor collections are for configuration / firmware, not battery.
            c.page == 0xFF00
                && c.usage == 2
                && (c.in_len == 16
                    || c.in_len == 19
                    || (c.in_len == 13 && pid == 0x1417)
                    || (c.in_len == 7 && is_legacy(pid)))
        }) {
            let path = c.path_str();
            present.push(path.clone());
            let fresh = !handles.contains_key(&path);
            if fresh {
                let Some(d) = io.open(c) else { continue };
                handles.insert(path.clone(), d);
            }
            let profile = if c.in_len == 7 {
                Profile::LegacyVt3Pro
            } else {
                Profile::Nordic54L
            };
            let Some(b) = read_battery(handles, &path, c, io, profile, fresh, end) else {
                continue;
            };
            let model = find_model(if b.mouse_pid != 0 { b.mouse_pid } else { pid });
            let identity = model.map_or(if b.mouse_pid != 0 { b.mouse_pid } else { pid }, |m| m.0);
            let mut name = model.map_or(c.product.clone(), |m| m.2.to_string());
            // No confirmed cable id for the 1244 receiver; it keeps its own identity.
            if pid == 0x1244 {
                name = "Rapoo VT3 Pro Max".into();
            }
            if name.is_empty() || name == "Rapoo Gaming Device" {
                name = "Rapoo wireless mouse".into();
            }
            let via = if b.connection == Connection::Cable
                || (b.connection == Connection::Unknown && model.is_some_and(|m| pid == m.0))
            {
                "USB cable"
            } else if b.connection == Connection::Bluetooth {
                "Bluetooth"
            } else {
                "2.4 GHz receiver"
            };
            let mut s = Device::new(format!("rapoo:{}", hex4(identity)), name, Kind::Mouse, via);
            s.percent = b.percent;
            s.charging = b.charging;
            s.grace = Duration::from_secs(1800);
            // Cable and receiver while charging: one entry, the cable's state, any known level.
            match out.iter_mut().find(|o| o.key == s.key) {
                None => out.push(s),
                Some(existing) if s.via == "USB cable" => {
                    let receiver_percent = existing.percent;
                    *existing = s;
                    if existing.percent.is_none() {
                        existing.percent = receiver_percent;
                    }
                }
                Some(existing) => {
                    if existing.percent.is_none() {
                        existing.percent = s.percent;
                    }
                }
            }
            break;
        }
    }
    handles.retain(|p, _| present.contains(p));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::hid::fake::{Fake, iface_coll as fcoll};

    fn with_path(mut c: Coll, path: &str) -> Coll {
        c.path = path.into();
        c
    }

    // ---- rapoo_protocol_test.cpp ----

    const CAPTURED: [u8; 19] = [
        0x07, 0x20, 0x03, 0x20, 0x03, 0x20, 0x03, 0x01, 0x19, 0x00, 0x04, 0x84, 0x00, 0x17, 0x46,
        0x00, 0x02, 0x03, 0xBC,
    ];
    const CAPTURED_CABLE: [u8; 19] = [
        0x07, 0x22, 0x03, 0x20, 0x03, 0x20, 0x03, 0x02, 0x19, 0x00, 0x04, 0x84, 0x00, 0x17, 0x46,
        0x00, 0x02, 0x03, 0x00,
    ];
    const VT9: [u8; 16] = [
        0x07, 0x10, 0x03, 0x40, 0x06, 0x40, 0x06, 0x01, 0x64, 0x00, 0x03, 0x01, 0x00, 0x14, 0x46,
        0x01,
    ];
    const VT9_CABLE: [u8; 19] = [
        0x07, 0x12, 0x04, 0x80, 0x0C, 0x80, 0x0C, 0x02, 0x64, 0x00, 0x03, 0x01, 0x00, 0x14, 0x46,
        0x01, 0x02, 0x02, 0x00,
    ];
    const N: Profile = Profile::Nordic54L;
    const L: Profile = Profile::LegacyVt3Pro;

    #[test]
    fn captured_nordic_frames() {
        let b = decode(&CAPTURED, N).unwrap();
        assert_eq!(
            (b.percent, b.charging, b.mouse_pid),
            (Some(25), false, 0x4617)
        );
        assert_eq!(b.connection, Connection::Receiver);
        let b = decode(&CAPTURED_CABLE, N).unwrap();
        assert_eq!(
            (b.percent, b.charging, b.connection),
            (Some(25), true, Connection::Cable)
        );
        let b = decode(&VT9, N).unwrap();
        assert_eq!(
            (b.percent, b.charging, b.mouse_pid),
            (Some(100), false, 0x4614)
        );
        let b = decode(&CAPTURED[..13], N).unwrap();
        assert_eq!(
            (b.percent, b.mouse_pid),
            (Some(25), 0),
            "13-byte collection has no pid"
        );
        let b = decode(&VT9_CABLE, N).unwrap();
        assert_eq!(
            (b.percent, b.charging, b.connection),
            (Some(100), true, Connection::Cable)
        );

        let mut modern = CAPTURED;
        modern[1] = 0x22; // sensor bits must not affect the connection type
        modern[7] = 2;
        modern[8] = 100;
        let b = decode(&modern, N).unwrap();
        assert_eq!(
            (b.charging, b.percent, b.connection),
            (true, Some(100), Connection::Cable)
        );
        modern[1] = 0x21;
        modern[7] = 1;
        modern[8] = 0;
        let b = decode(&modern, N).unwrap();
        assert_eq!(
            (b.percent, b.charging, b.connection),
            (Some(0), false, Connection::Bluetooth)
        );
        modern[1] = 0x23;
        assert!(decode(&modern, N).is_none());
    }

    #[test]
    fn legacy_frames() {
        let mut legacy = vec![0xBB, 0xB0, 0x81, 0x20, 0x03, 1, 37];
        assert!(decode(&legacy, N).is_none());
        assert!(decode(&CAPTURED, L).is_none());
        let b = decode(&legacy, L).unwrap();
        assert_eq!(
            (b.percent, b.charging, b.mouse_pid, b.connection),
            (Some(37), false, 0, Connection::Unknown)
        );
        legacy[5] = 2;
        legacy[6] = 0x7F;
        let b = decode(&legacy, L).unwrap();
        assert!(
            b.charging && b.percent.is_none(),
            "unknown must never become 100%"
        );
        legacy[6] = 100;
        assert_eq!(decode(&legacy, L).unwrap().percent, Some(100));
        legacy[5] = 1;
        assert!(!decode(&legacy, L).unwrap().charging);
        let b = decode(&[0xBB, 0xB0, 0x71, 0x84, 0x03, 0x01, 0x64], L).unwrap();
        assert_eq!((b.percent, b.charging), (Some(100), false));
        let b = decode(&[0xBB, 0xB0, 0x71, 0x84, 0x03, 0x02, 0x5F], L).unwrap();
        assert_eq!((b.percent, b.charging), (Some(95), true));
    }

    #[test]
    fn malformed_frames() {
        for size in 0..9 {
            assert!(decode(&CAPTURED[..size], N).is_none());
        }
        let legacy = [0xBB, 0xB0, 0x81, 0x20, 0x03, 1, 37];
        for size in 0..7 {
            assert!(decode(&legacy[..size], L).is_none());
        }
        assert_eq!(decode(&CAPTURED[..9], N).unwrap().mouse_pid, 0);
        for state in [0u8, 3, 255] {
            let mut m = CAPTURED;
            m[7] = state;
            assert!(decode(&m, N).is_none());
            let mut l = legacy;
            l[5] = state;
            assert!(decode(&l, L).is_none());
        }
        for percent in [101u8, 126, 127, 128, 255] {
            let mut m = CAPTURED;
            m[8] = percent;
            assert!(decode(&m, N).is_none());
            let mut l = legacy;
            l[5] = 1;
            l[6] = percent;
            assert!(decode(&l, L).is_none());
        }
        assert!(decode(&[0xBC, 0xB0, 0x81, 0x20, 0x03, 1, 50, 0, 0, 0], L).is_none());
        assert!(decode(&[0xBB, 0, 0x81, 0x20, 0x03, 1, 50, 0, 0, 0], L).is_none());
        let mut m = CAPTURED_CABLE;
        m[8] = 0x7F;
        assert!(decode(&m, N).is_none(), "sentinel is legacy-only");
        let mut m = CAPTURED;
        m[0] = 0x08; // CH584 has a separate protocol
        assert!(decode(&m, N).is_none());
    }

    // ---- rapoo_driver_test.cpp ----

    fn status(cable: bool, percent: u8) -> Vec<u8> {
        vec![
            7,
            if cable { 0x22 } else { 0x20 },
            3,
            0x20,
            3,
            0x20,
            3,
            if cable { 2 } else { 1 },
            percent,
            0,
            4,
            0x84,
            0,
            0x17,
            0x46,
            0,
            2,
            3,
            0xBC,
        ]
    }

    fn coll(pid: u16, path: &str, len: u16) -> Coll {
        let mut c = with_path(fcoll(VID, pid, 1, 0xFF00, 2, len, 0, 0), path);
        c.product = "Rapoo Gaming Device".into();
        c
    }

    fn reports(f: &Fake, list: &[(&str, Vec<Vec<u8>>)]) {
        for (path, rs) in list {
            for r in rs {
                f.s().push_input(path, r.clone());
            }
        }
    }

    fn poll1(cs: &[Coll], list: &[(&str, Vec<Vec<u8>>)]) -> (Vec<Device>, Fake) {
        let mut f = Fake::new();
        reports(&f, list);
        let out = poll_with(cs, &mut f, &mut HashMap::new());
        (out, f)
    }

    #[test]
    fn cable_wins_over_receiver() {
        let (out, _) = poll1(
            &[coll(0x1417, "receiver", 19), coll(0x4617, "cable", 19)],
            &[
                ("receiver", vec![vec![0xBC, 0], status(false, 25)]),
                ("cable", vec![status(true, 27)]),
            ],
        );
        assert_eq!(out.len(), 1);
        assert_eq!(
            (out[0].key.as_str(), out[0].percent, out[0].charging),
            ("rapoo:4617", Some(27), true)
        );
        assert_eq!(
            (out[0].via.as_str(), out[0].kind),
            ("USB cable", Kind::Mouse)
        );
        assert_eq!(out[0].grace, Duration::from_secs(1800));

        let (out, _) = poll1(
            &[coll(0x1417, "receiver", 19)],
            &[("receiver", vec![status(false, 25)])],
        );
        assert_eq!(
            (out[0].key.as_str(), out[0].percent, out[0].charging),
            ("rapoo:4617", Some(25), false)
        );
        assert_eq!(out[0].via, "2.4 GHz receiver");

        let (out, _) = poll1(&[coll(0x1417, "receiver", 19)], &[]);
        assert!(out.is_empty(), "a timeout invents no level");
    }

    #[test]
    fn legacy_cable_keeps_the_receiver_level() {
        let (out, _) = poll1(
            &[coll(0x1215, "legacy", 7), coll(0x4415, "legacy-cable", 7)],
            &[
                ("legacy", vec![vec![0xBB, 0xB0, 0x81, 0x20, 3, 1, 37]]),
                (
                    "legacy-cable",
                    vec![vec![0xBB, 0xB0, 0x81, 0x20, 3, 2, 0x7F]],
                ),
            ],
        );
        assert_eq!(out.len(), 1);
        assert_eq!(
            (out[0].key.as_str(), out[0].charging, out[0].percent),
            ("rapoo:4415", true, Some(37))
        );
        assert_eq!(out[0].via, "USB cable");

        let (out, _) = poll1(
            &[coll(0x4415, "legacy-cable", 7)],
            &[(
                "legacy-cable",
                vec![vec![0xBB, 0xB0, 0x81, 0x20, 3, 2, 0x7F]],
            )],
        );
        assert!(out[0].charging && out[0].percent.is_none());
    }

    #[test]
    fn legacy_models() {
        let (out, _) = poll1(
            &[coll(0x1233, "vt1pm", 7)],
            &[("vt1pm", vec![vec![0xBB, 0xB0, 0x71, 0x84, 3, 1, 0x64]])],
        );
        assert_eq!(
            (out[0].key.as_str(), out[0].name.as_str()),
            ("rapoo:4433", "Rapoo VT1 Pro Max")
        );
        assert_eq!(
            (out[0].percent, out[0].charging, out[0].via.as_str()),
            (Some(100), false, "2.4 GHz receiver")
        );
        let (out, _) = poll1(
            &[coll(0x4433, "vt1pm-cable", 7)],
            &[(
                "vt1pm-cable",
                vec![vec![0xBB, 0xB0, 0x71, 0x84, 3, 2, 0x5F]],
            )],
        );
        assert_eq!(
            (out[0].key.as_str(), out[0].percent, out[0].via.as_str()),
            ("rapoo:4433", Some(95), "USB cable")
        );

        let (out, f) = poll1(
            &[
                coll(0x1244, "vt3-pro-max", 7),
                coll(0x1244, "vt3-pro-max-10", 10),
            ],
            &[
                (
                    "vt3-pro-max",
                    vec![vec![0xBB, 0xB0, 0x81, 0x20, 0x03, 0x01, 45]],
                ),
                (
                    "vt3-pro-max-10",
                    vec![vec![0xBC, 0xB0, 0, 0, 0, 0, 0, 0, 0, 0]],
                ),
            ],
        );
        assert_eq!(
            f.s().opened.len(),
            1,
            "the 10-byte sibling is not a battery endpoint"
        );
        assert_eq!(
            (out[0].key.as_str(), out[0].name.as_str()),
            ("rapoo:1244", "Rapoo VT3 Pro Max")
        );
        assert_eq!((out[0].percent, out[0].charging), (Some(45), false));

        let (out, _) = poll1(
            &[coll(0x1244, "vt3-pro-max", 7)],
            &[(
                "vt3-pro-max",
                vec![vec![0xBB, 0xB0, 0x81, 0x20, 0x03, 0x02, 0x7F]],
            )],
        );
        assert!(
            out[0].charging && out[0].percent.is_none(),
            "7F is unknown, never 127%"
        );
    }

    #[test]
    fn legacy_decoder_is_restricted() {
        let (out, f) = poll1(
            &[coll(0x1245, "unknown-1245", 7)],
            &[(
                "unknown-1245",
                vec![vec![0xBB, 0xB0, 0x81, 0x20, 0x03, 0x01, 45]],
            )],
        );
        assert!(out.is_empty() && f.s().opened.is_empty());
        let (out, _) = poll1(
            &[coll(0x1417, "receiver", 19)],
            &[("receiver", vec![vec![0xBB, 0xB0, 0x81, 0x20, 3, 2, 50]])],
        );
        assert!(out.is_empty());
        let (out, f) = poll1(&[coll(0x1414, "other-13", 13)], &[]);
        assert!(
            out.is_empty() && f.s().opened.is_empty(),
            "13 bytes only on 1417"
        );
    }

    #[test]
    fn vt9_and_vt3_max_identities() {
        let (out, _) = poll1(
            &[coll(0x1414, "vt9-gen2", 16)],
            &[("vt9-gen2", vec![VT9.to_vec()])],
        );
        assert_eq!(
            (out[0].key.as_str(), out[0].name.as_str()),
            ("rapoo:4614", "Rapoo VT9 (Gen 2)")
        );
        assert_eq!(
            (out[0].percent, out[0].via.as_str()),
            (Some(100), "2.4 GHz receiver")
        );
        let (out, _) = poll1(
            &[
                coll(0x1414, "vt9-gen2", 16),
                coll(0x4614, "vt9-gen2-cable", 19),
            ],
            &[
                ("vt9-gen2", vec![VT9.to_vec()]),
                ("vt9-gen2-cable", vec![VT9_CABLE.to_vec()]),
            ],
        );
        assert_eq!(out.len(), 1);
        assert_eq!((out[0].charging, out[0].via.as_str()), (true, "USB cable"));
        let (out, _) = poll1(
            &[coll(0x1417, "vt3max-13", 13)],
            &[("vt3max-13", vec![CAPTURED[..13].to_vec()])],
        );
        assert_eq!(
            (out[0].key.as_str(), out[0].name.as_str()),
            ("rapoo:4617", "Rapoo VT3 Max (Gen 2)")
        );
        assert_eq!(out[0].percent, Some(25));
    }

    #[test]
    fn persistent_handle_drains_queued_reports() {
        let cs = [coll(0x1414, "vt9-queued", 16)];
        let frame = |p: u8| {
            let mut r = VT9.to_vec();
            r[8] = p;
            r
        };
        let mut handles = HashMap::new();
        let mut f = Fake::new();
        reports(&f, &[("vt9-queued", vec![frame(98)])]);
        assert_eq!(poll_with(&cs, &mut f, &mut handles)[0].percent, Some(98));
        reports(&f, &[("vt9-queued", vec![frame(98), frame(97)])]);
        let out = poll_with(&cs, &mut f, &mut handles);
        assert_eq!(
            (f.s().opened.len(), out[0].percent),
            (1, Some(97)),
            "newest wins, no reopen"
        );
        poll_with(&[], &mut f, &mut handles); // unplugged: the handle is dropped
        reports(&f, &[("vt9-queued", vec![frame(96)])]);
        let out = poll_with(&cs, &mut f, &mut handles);
        assert_eq!((f.s().opened.len(), out[0].percent), (2, Some(96)));
    }

    #[test]
    fn unlisted_model_and_strict_matching() {
        let mut unknown = status(false, 62);
        unknown[13] = 0;
        unknown[14] = 0;
        let (out, _) = poll1(
            &[coll(0x7ABC, "new-model", 19)],
            &[("new-model", vec![unknown])],
        );
        assert_eq!(
            (out[0].name.as_str(), out[0].key.as_str()),
            ("Rapoo wireless mouse", "rapoo:7ABC")
        );
        assert_eq!(out[0].percent, Some(62));

        let mut other_vendor = coll(0x1417, "receiver", 19);
        other_vendor.vid = 0x1234;
        let mut config = coll(0x1417, "receiver", 19);
        config.usage = 14;
        let mut wrong_page = coll(0x1417, "receiver", 19);
        wrong_page.page = 1;
        let (out, f) = poll1(
            &[
                other_vendor,
                config,
                wrong_page,
                coll(0x1215, "silent-sibling", 10),
                coll(0x1C00, "unrecognised-telink", 7),
            ],
            &[],
        );
        assert!(out.is_empty() && f.s().opened.is_empty());
    }
}

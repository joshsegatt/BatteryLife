//! Audeze Maxwell / Maxwell 2 headsets (VID 3329) on the USB dongle or the USB-C cable. Vendor
//! collection FF13:0001: the battery query (attribute 0x0CD6) goes out as 62-byte output report 06;
//! the answer is input report 07, fetched with GetInputReport, a rolling buffer of recent replies
//! scanned for the marker D6 0C 00 00 <percent>. With no headset linked the dongle names itself
//! "... Dongle" and keeps a stale value, so it is not asked then. No charging flag: on its cable a
//! Maxwell always charges.
//!
//! Ported from BatteryHub (src/drivers/audeze.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0, which credits HaloBattery
//! (providers/audeze.py) and HeadsetControl (audeze_maxwell.hpp).

use std::sync::Mutex;
use std::time::{Duration, Instant};

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::hid_io::{self, Coll, Io, Open, find};

pub const VID: u16 = 0x3329;
/// (pid, cable, name); cables first: the direct connection wins.
const MODELS: &[(u16, bool, &str)] = &[
    (0x4B1A, true, "Audeze Maxwell"),
    (0x4B1E, true, "Audeze Maxwell"),
    (0x4B19, false, "Audeze Maxwell"),
    (0x4B18, false, "Audeze Maxwell"),
    (0x4B29, false, "Audeze Maxwell 2"),
    (0x4B28, false, "Audeze Maxwell 2"),
];
pub const PIDS: &[u16] = &[0x4B1A, 0x4B1E, 0x4B19, 0x4B18, 0x4B29, 0x4B28];

/// A headset reports 0% for a moment right after power-on.
const POWER_ON_ZERO: Duration = Duration::from_secs(90);

/// When each product was first seen answering (BatteryHub's `firstSeen`).
#[derive(Default)]
pub struct State {
    first_seen: Vec<(u16, Instant)>,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

/// Battery percent from a report 07 buffer: the marker D6 0C 00 00 <percent>.
pub fn find_level(b: &[u8]) -> Option<u8> {
    b.windows(5)
        .find(|w| w[..4] == [0xD6, 0x0C, 0x00, 0x00] && w[4] <= 100)
        .map(|w| w[4])
}

fn request() -> [u8; 62] {
    let mut req = [0u8; 62];
    req[..9].copy_from_slice(&[0x06, 0x07, 0x80, 0x05, 0x5A, 0x03, 0x00, 0xD6, 0x0C]);
    req
}

pub fn read_level(io: &mut dyn Io) -> Option<u8> {
    let mut level = None;
    for _ in 0..2 {
        if !io.write(&request()) {
            return None;
        }
        // The answer can arrive a little later; the newest frame counts.
        for i in 0..4 {
            io.sleep(60);
            let Some(r) = io.get_input_report(0x07) else {
                continue;
            };
            if let Some(l) = find_level(&r) {
                level = Some(l);
            }
            if i >= 2 && level.is_some() {
                return level;
            }
        }
        if level.is_some() {
            return level;
        }
    }
    level
}

pub fn poll_colls(colls: &[Coll], open: &mut Open<'_>, st: &mut State) -> Vec<Device> {
    for &(pid, cable, name) in MODELS {
        let found = find(colls, VID, pid, |c| {
            c.page == 0xFF13 && c.usage == 0x0001 && c.in_len > 0
        });
        let Some(c) = found.first() else {
            st.first_seen.retain(|(p, _)| *p != pid);
            continue;
        };
        if !cable && c.product.to_lowercase().ends_with("dongle") {
            // No headset linked: nothing to read (and the stored value is stale).
            st.first_seen.retain(|(p, _)| *p != pid);
            continue;
        }
        let level = open(c).and_then(|mut io| read_level(io.as_mut()));
        let now = Instant::now();
        if !st.first_seen.iter().any(|(p, _)| *p == pid) {
            st.first_seen.push((pid, now));
        }
        let first = st
            .first_seen
            .iter()
            .find(|(p, _)| *p == pid)
            .map_or(now, |f| f.1);
        let via = if cable { "USB cable" } else { "2.4 GHz dongle" };
        // Dongle and cable are one headset.
        let mut d = Device::new("audeze:maxwell", name, Kind::Headset, via);
        d.grace = Duration::from_secs(300);
        match level {
            Some(l) if l > 0 || now.duration_since(first) >= POWER_ON_ZERO => {
                d.percent = Some(l);
                d.charging = cable;
            }
            // No reading yet (or the power-on 0%): keep the last value.
            _ => d.asleep = true,
        }
        return vec![d];
    }
    Vec::new()
}

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    let mut guard = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let st = guard.get_or_insert_with(State::default);
    hid_io::run(api, infos, |colls, open| poll_colls(colls, open, st))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::hid_io::fake::Fake;
    use std::cell::RefCell;

    fn level(n: u8) -> Vec<u8> {
        vec![7, 8, 0x80, 0xD6, 0x0C, 0, 0, n]
    }
    fn frames(f: Vec<Vec<u8>>) -> Fake {
        let fake = Fake::new();
        fake.s().inputs = f.into();
        fake
    }
    fn counts(f: &Fake) -> (usize, usize) {
        let s = f.s();
        (s.writes.len(), s.input_reads)
    }

    // tests/audeze_retry_test.cpp
    #[test]
    fn newest_frame_and_retry() {
        let f = frames(vec![level(80), level(81), level(82)]);
        assert_eq!(read_level(&mut f.clone()), Some(82));
        assert_eq!(counts(&f), (1, 3));
        let mut expected = vec![0u8; 62];
        expected[..9].copy_from_slice(&[6, 7, 0x80, 5, 0x5A, 3, 0, 0xD6, 0x0C]);
        assert_eq!(f.s().writes[0], expected);

        let f = frames(vec![vec![], vec![], vec![], level(73)]);
        assert_eq!(read_level(&mut f.clone()), Some(73));
        assert_eq!(counts(&f), (1, 4));

        let f = frames(vec![
            vec![],
            vec![],
            vec![],
            vec![],
            level(65),
            vec![],
            vec![],
        ]);
        assert_eq!(read_level(&mut f.clone()), Some(65));
        assert_eq!(counts(&f), (2, 8));

        let f = frames(vec![vec![7], vec![7, 0xD6, 0x0C, 0, 0, 101]]);
        assert_eq!(read_level(&mut f.clone()), None);
        assert_eq!(counts(&f), (2, 8));

        for l in [0, 100] {
            let f = frames(vec![level(l), level(l), level(l)]);
            assert_eq!(read_level(&mut f.clone()), Some(l));
        }
        let f = Fake::new();
        f.s().write_ok = false;
        assert_eq!(read_level(&mut f.clone()), None);
        assert_eq!(counts(&f), (1, 0));
    }

    fn maxwell(pid: u16, product: &str) -> Coll {
        let mut c = Coll::new(VID, pid, 0xFF13, 1, 62, 62);
        c.product = product.into();
        c
    }

    #[test]
    fn identity_per_product() {
        for pid in [0x4B29, 0x4B28, 0x4B19, 0x4B1A] {
            let f = frames(vec![level(86), level(86), level(86)]);
            let opened = RefCell::new(Vec::new());
            let out = poll_colls(
                &[maxwell(pid, "Audeze Maxwell")],
                &mut *f.opener(&opened),
                &mut State::default(),
            );
            assert_eq!(out.len(), 1);
            assert_eq!(out[0].percent, Some(86));
            let two = pid == 0x4B29 || pid == 0x4B28;
            assert_eq!(
                out[0].name,
                if two {
                    "Audeze Maxwell 2"
                } else {
                    "Audeze Maxwell"
                }
            );
            assert_eq!(out[0].charging, pid == 0x4B1A);
            assert_eq!(out[0].key, "audeze:maxwell");
        }
    }

    #[test]
    fn foreign_collections_and_lone_dongle_are_not_asked() {
        let mut wrong = vec![maxwell(0xFFFF, "Audeze Maxwell")];
        let mut c = maxwell(0x4B29, "Audeze Maxwell");
        c.page += 1;
        wrong.push(c);
        let mut c = maxwell(0x4B29, "Audeze Maxwell");
        c.usage += 1;
        wrong.push(c);
        wrong.push(maxwell(0x4B29, "Audeze Maxwell 2 Dongle"));
        for c in wrong {
            let f = frames(vec![level(86)]);
            let opened = RefCell::new(Vec::new());
            let out = poll_colls(&[c], &mut *f.opener(&opened), &mut State::default());
            assert!(out.is_empty() && opened.borrow().is_empty());
        }
    }

    #[test]
    fn power_on_zero_is_not_trusted() {
        let f = frames(vec![level(0), level(0), level(0)]);
        let opened = RefCell::new(Vec::new());
        let out = poll_colls(
            &[maxwell(0x4B19, "Audeze Maxwell")],
            &mut *f.opener(&opened),
            &mut State::default(),
        );
        assert_eq!((out[0].percent, out[0].asleep), (None, true));
    }
}

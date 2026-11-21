//! SteelSeries wireless headsets and mice (VID 1038): Arctis 1 / 7 / 9 / Pro / Nova family, Aerox and
//! Prime Wireless, Rival 3 Wireless / Rival 650 / Rival Pro. Only the status reads listed per model
//! are sent. Headset protocols follow HeadsetControl, mouse protocols rivalcfg.
//!
//! Ported from BatteryHub (src/drivers/steelseries.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0, which credits
//! HeadsetControl, rivalcfg and steel-mouse.

use std::time::Duration;

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::hid::map_to_100;
use crate::drivers::hid::{self, Coll, Dev, Hid, MAX_WAIT, find, products};

pub const VID: u16 = 0x1038;
/// Wireless devices drop off their receiver while asleep; keep the last level this long.
const RECEIVER_GRACE: Duration = Duration::from_secs(30 * 60);

/// One reading: None = no valid reply. `connected == false` is a base station whose headset is off.
#[derive(Debug, Default, PartialEq)]
pub struct Status {
    pub percent: Option<u8>,
    pub charging: bool,
    pub connected: bool,
}

fn status(percent: Option<u8>, charging: bool, connected: bool) -> Option<Status> {
    Some(Status {
        percent: if connected { percent } else { None },
        charging,
        connected,
    })
}

fn clamp(v: u8) -> u8 {
    v.min(100)
}

fn in_list(pid: u16, list: &[u16]) -> bool {
    list.contains(&pid)
}

// ---- headset reply parsers ------------------------------------------------------------------

/// Arctis 1 / 1 Xbox / 7X: 06 12 -> [2] == 01 offline, [3] %.
pub fn parse_arctis1(r: &[u8]) -> Option<Status> {
    (r.len() >= 4).then_some(())?;
    status(Some(clamp(r[3])), false, r[2] != 0x01)
}

/// Arctis 7 / 7 2019 / Pro 2019: 06 18 -> [2] %.
pub fn parse_arctis7(r: &[u8]) -> Option<Status> {
    (r.len() >= 3).then_some(())?;
    Some(Status {
        percent: Some(clamp(r[2])),
        charging: false,
        connected: r[2] != 0,
    })
}

/// Arctis 9: 00 20 -> [3] raw (0x64..0x9A), [4] == 01 charging.
pub fn parse_arctis9(r: &[u8]) -> Option<Status> {
    (r.len() >= 5).then_some(())?;
    status(
        Some(map_to_100(r[3].into(), 0x64, 0x9A)),
        r[4] == 0x01,
        true,
    )
}

/// Arctis Nova Pro Wireless: 06 B0 -> [15] status (01 offline, 02 charging), [6] level 0..8.
pub fn parse_nova_pro(r: &[u8]) -> Option<Status> {
    (r.len() >= 16).then_some(())?;
    status(
        Some(map_to_100(r[6].into(), 0, 8)),
        r[15] == 0x02,
        r[15] != 0x01,
    )
}

/// Nova family: 00 B0 -> status block; the layout depends on the model.
pub fn parse_nova_family(pid: u16, r: &[u8]) -> Option<Status> {
    (r.len() >= 7).then_some(())?;
    if in_list(pid, &[0x220E, 0x2212, 0x2216, 0x2236]) {
        // Arctis 7+
        status(
            Some(map_to_100(r[2].into(), 0, 4)),
            r[3] == 0x01,
            r[1] != 0x01,
        )
    } else if in_list(pid, &[0x2269, 0x226D, 0x2232, 0x2253]) {
        // Nova 3P Wireless, Nova 5
        status(Some(clamp(r[3])), r[4] == 0x01, r[1] != 0x02)
    } else if pid == 0x230A {
        // Arctis GameBuds: lowest of the buds that are out of the case.
        let mut lvl: Option<u8> = None;
        if r[3] == 0x03 {
            lvl = Some(r[5]);
        }
        if r[4] == 0x03 {
            lvl = Some(lvl.map_or(r[6], |l| l.min(r[6])));
        }
        status(lvl.map(clamp), false, lvl.is_some())
    } else {
        // Arctis Nova 7 / 7X / 7P and variants
        let discrete = in_list(pid, &[0x2202, 0x2206, 0x220A, 0x223A, 0x227A, 0x22A4]);
        let pct = if discrete {
            map_to_100(r[2].into(), 0, 4)
        } else {
            clamp(r[2])
        };
        status(Some(pct), r[3] == 0x01 || r[3] == 0x02, r[3] != 0x00)
    }
}

fn headset_name(pid: u16) -> &'static str {
    match pid {
        0x12B3 | 0x12B6 | 0x12D7 => "SteelSeries Arctis 1 Wireless",
        0x1252 => "SteelSeries Arctis Pro",
        0x1260 | 0x12AD => "SteelSeries Arctis 7",
        0x12C2 => "SteelSeries Arctis 9",
        0x1290 => "SteelSeries Arctis Pro Wireless",
        0x12E0 | 0x225D => "SteelSeries Arctis Nova Pro Wireless",
        0x220E | 0x2212 | 0x2216 | 0x2236 => "SteelSeries Arctis 7+",
        0x2232 | 0x2253 => "SteelSeries Arctis Nova 5",
        0x2269 | 0x226D => "SteelSeries Arctis Nova 3P Wireless",
        0x230A => "SteelSeries Arctis GameBuds",
        _ => "SteelSeries Arctis Nova 7",
    }
}

// ---- mouse reply parsers ----------------------------------------------------------------------

/// Aerox / Prime Wireless family: 92 on the cable, D2 through the receiver.
pub struct AeroxModel {
    pub pid: u16,
    /// Stable identity on both the receiver and the cable.
    pub receiver_pid: u16,
    pub name: &'static str,
    pub wired: bool,
    /// Gen 2 layout (percent) instead of steps.
    pub percent: bool,
}

const fn aerox(pid: u16, receiver_pid: u16, name: &'static str, wired: bool) -> AeroxModel {
    AeroxModel {
        pid,
        receiver_pid,
        name,
        wired,
        percent: false,
    }
}

pub const AEROX: &[AeroxModel] = &[
    aerox(0x1838, 0x1838, "SteelSeries Aerox 3 Wireless", false),
    aerox(0x183A, 0x1838, "SteelSeries Aerox 3 Wireless", true),
    aerox(
        0x1878,
        0x1878,
        "SteelSeries Aerox 3 Wireless CS2 Dragon Lore Edition",
        false,
    ),
    aerox(
        0x187A,
        0x1878,
        "SteelSeries Aerox 3 Wireless CS2 Dragon Lore Edition",
        true,
    ),
    AeroxModel {
        percent: true,
        ..aerox(0x1890, 0x1890, "SteelSeries Aerox 3 Wireless Gen 2", false)
    },
    AeroxModel {
        percent: true,
        ..aerox(0x1892, 0x1890, "SteelSeries Aerox 3 Wireless Gen 2", true)
    },
    aerox(0x1852, 0x1852, "SteelSeries Aerox 5 Wireless", false),
    aerox(0x1854, 0x1852, "SteelSeries Aerox 5 Wireless", true),
    aerox(0x1858, 0x1858, "SteelSeries Aerox 9 Wireless", false),
    aerox(0x185A, 0x1858, "SteelSeries Aerox 9 Wireless", true),
    aerox(0x1840, 0x1840, "SteelSeries Prime Wireless", false),
    aerox(0x1842, 0x1840, "SteelSeries Prime Wireless", true),
    aerox(
        0x1860,
        0x1860,
        "SteelSeries Aerox 5 Wireless Diablo IV Edition",
        false,
    ), // untested hardware
];

/// The echo of the Aerox query: None = mouse off / asleep (left out, the tracker keeps the last
/// level), or not a reply of this layout.
///  * steps: [1] = (level/5 + 1) | charging<<7; 00 = off or asleep.
///  * percent (Gen 2): [1] 00 battery / 01 charging / 02 charged on the cable, [2] 0..100 or FF
///    (not measured yet); 00 00 through the receiver = off.
pub fn parse_aerox(m: &AeroxModel, r: &[u8]) -> Option<Status> {
    if m.percent {
        if r.len() < 3 || r[1] > 0x02 {
            return None;
        }
        if !m.wired && r[1] == 0x00 && r[2] == 0x00 {
            return None;
        }
        return status((r[2] <= 100).then_some(r[2]), r[1] != 0x00, true);
    }
    let level = r.get(1)? & 0x7F;
    if level == 0 {
        return None;
    }
    if level <= 21 {
        status(Some((level - 1) * 5), r[1] & 0x80 != 0, true)
    } else {
        // Invalid levels stay unknown and never invent a charging state.
        status(None, false, true)
    }
}

// ---- I/O --------------------------------------------------------------------------------------

fn write_read(d: &mut dyn Dev, req: &[u8], timeout: Duration) -> Option<Vec<u8>> {
    d.flush();
    if !d.write(req) {
        return None;
    }
    hid::read_until(d, timeout, |_| true)
}

/// Sends `cmd` and returns the first input report that echoes it. SteelSeries GG keeps its own
/// commands going on the same interface and the receiver only answers the latest one, so the query is
/// resent every 100 ms until the echo arrives. 40 FF (the receiver cannot reach the mouse) ends the
/// wait at once.
fn query_echo(hid: &dyn Hid, d: &mut dyn Dev, cmd: u8, min_len: usize) -> Option<Vec<u8>> {
    d.flush();
    let start = hid.now();
    let end = start + MAX_WAIT;
    let mut resend = start;
    loop {
        let now = hid.now();
        if now >= end {
            return None;
        }
        if now >= resend {
            if !d.write(&[0x00, cmd]) {
                return None;
            }
            resend = now + Duration::from_millis(100);
        }
        let wait = resend.min(end).saturating_sub(now);
        if wait.is_zero() {
            continue;
        }
        let Some(r) = d.read(wait) else { continue };
        if r.len() >= min_len && r[0] == cmd {
            return Some(r);
        }
        if r.len() >= 2 && r[0] == 0x40 && r[1] == 0xFF {
            return None;
        }
    }
}

#[derive(Clone, Copy)]
enum Model {
    Arctis1,
    Arctis7,
    Arctis9,
    ArctisProWireless,
    NovaProWireless,
    NovaFamily,
    Aerox,
    Rival,
    RivalPro,
}

const AEROX_PIDS: &[u16] = &[
    0x1838, 0x183A, 0x1878, 0x187A, 0x1890, 0x1892, 0x1852, 0x1854, 0x1858, 0x185A, 0x1840, 0x1842,
    0x1860,
];
const NOVA_FAMILY_PIDS: &[u16] = &[
    0x220E, 0x2212, 0x2216, 0x2236, 0x2269, 0x226D, 0x2232, 0x2253, 0x230A, 0x2202, 0x22A1, 0x227E,
    0x2206, 0x2258, 0x229E, 0x22AD, 0x223A, 0x22A9, 0x227A, 0x22A4, 0x22A5, 0x220A, 0x22A7,
];

/// (pids, which collection, protocol), in BatteryHub's order.
type ModelRow = (&'static [u16], fn(&Coll) -> bool, Model);
const MODELS: &[ModelRow] = &[
    (&[0x12B3, 0x12B6, 0x12D7], page_ff43, Model::Arctis1),
    (&[0x1260, 0x12AD, 0x1252], page_ff43, Model::Arctis7),
    (&[0x12C2], iface0, Model::Arctis9),
    (&[0x1290], iface0, Model::ArctisProWireless),
    (&[0x12E0, 0x225D], iface4, Model::NovaProWireless),
    (NOVA_FAMILY_PIDS, page_ffc0, Model::NovaFamily),
    (AEROX_PIDS, iface3, Model::Aerox),
    (&[0x1830, 0x1872], iface3, Model::Rival),
    (&[0x172B, 0x1726], iface0, Model::Rival), // Rival 650: cable, 2.4 GHz receiver
    (&[0x1880], iface2, Model::RivalPro),
];

/// Every product id this driver reads.
pub const PIDS: &[u16] = &[
    0x12B3, 0x12B6, 0x12D7, 0x1260, 0x12AD, 0x1252, 0x12C2, 0x1290, 0x12E0, 0x225D, 0x220E, 0x2212,
    0x2216, 0x2236, 0x2269, 0x226D, 0x2232, 0x2253, 0x230A, 0x2202, 0x22A1, 0x227E, 0x2206, 0x2258,
    0x229E, 0x22AD, 0x223A, 0x22A9, 0x227A, 0x22A4, 0x22A5, 0x220A, 0x22A7, 0x1838, 0x183A, 0x1878,
    0x187A, 0x1890, 0x1892, 0x1852, 0x1854, 0x1858, 0x185A, 0x1840, 0x1842, 0x1860, 0x1830, 0x1872,
    0x172B, 0x1726, 0x1880,
];

fn page_ff43(c: &Coll) -> bool {
    c.page == 0xFF43 && c.usage == 0x0202
}
fn page_ffc0(c: &Coll) -> bool {
    c.page == 0xFFC0 && c.usage == 0x0001
}
fn iface0(c: &Coll) -> bool {
    c.iface == 0 && c.out_len > 0
}
fn iface2(c: &Coll) -> bool {
    c.iface == 2 && c.out_len > 0
}
fn iface3(c: &Coll) -> bool {
    c.iface == 3 && c.out_len > 0
}
fn iface4(c: &Coll) -> bool {
    c.iface == 4 && c.out_len > 0
}

fn headset(pid: u16, s: Status) -> (Device, bool) {
    let mut d = Device::new(
        format!("steelseries:{pid:04X}"),
        headset_name(pid),
        Kind::Headset,
        "2.4 GHz receiver",
    );
    d.percent = s.percent;
    d.charging = s.charging;
    d.grace = RECEIVER_GRACE;
    (d, s.connected)
}

fn mouse(pid: u16, name: &str, wired: bool) -> Device {
    Device::new(
        format!("steelseries:{pid:04X}"),
        name,
        Kind::Mouse,
        if wired {
            "USB cable"
        } else {
            "2.4 GHz receiver"
        },
    )
}

/// Arctis Pro Wireless: the command goes behind a 0 report id on Windows (interface 0 does not
/// number its reports); the numbered form is tried in case a firmware numbers them.
fn arctis_pro_cmd(d: &mut dyn Dev, cmd: u8) -> Option<Vec<u8>> {
    let t = MAX_WAIT;
    write_read(d, &[0x00, cmd, 0xAA], t).or_else(|| write_read(d, &[cmd, 0xAA], t))
}

/// One device and whether it is connected, or None when the collection gave no valid reply.
fn read(hid: &dyn Hid, d: &mut dyn Dev, model: Model, pid: u16) -> Option<(Device, bool)> {
    match model {
        Model::Arctis1 => {
            let r = write_read(d, &[0x06, 0x12], MAX_WAIT)?;
            Some(headset(pid, parse_arctis1(&r)?))
        }
        Model::Arctis7 => {
            let r = write_read(d, &[0x06, 0x18], MAX_WAIT)?;
            Some(headset(pid, parse_arctis7(&r)?))
        }
        Model::Arctis9 => {
            let r = write_read(d, &[0x00, 0x20], MAX_WAIT)?;
            Some(headset(pid, parse_arctis9(&r)?))
        }
        Model::ArctisProWireless => {
            // 41 AA -> [0] == 02 offline (04 online); 40 AA -> [0] level 0..4.
            let r = arctis_pro_cmd(d, 0x41)?;
            let connected = r[0] != 0x02;
            let mut s = Status {
                connected,
                ..Default::default()
            };
            if connected && let Some(l) = arctis_pro_cmd(d, 0x40).filter(|l| l[0] <= 4) {
                s.percent = Some(map_to_100(l[0].into(), 0, 4));
            }
            Some(headset(pid, s))
        }
        Model::NovaProWireless => {
            let r = write_read(d, &[0x06, 0xB0], MAX_WAIT)?;
            Some(headset(pid, parse_nova_pro(&r)?))
        }
        Model::NovaFamily => {
            let r = write_read(d, &[0x00, 0xB0], MAX_WAIT)?;
            Some(headset(pid, parse_nova_family(pid, &r)?))
        }
        Model::Aerox => {
            let m = AEROX.iter().find(|m| m.pid == pid)?;
            let cmd = if m.wired { 0x92 } else { 0xD2 };
            let r = query_echo(hid, d, cmd, if m.percent { 3 } else { 2 })?;
            let s = parse_aerox(m, &r)?;
            let mut dev = mouse(pid, m.name, m.wired);
            dev.key = format!("steelseries:{:04X}", m.receiver_pid);
            dev.grace = RECEIVER_GRACE;
            dev.percent = s.percent;
            dev.charging = s.charging;
            Some((dev, true))
        }
        Model::Rival => {
            // Rival 3 Wireless (+ Gen 2), Rival 650: AA 01 -> [0] %, [2] charging.
            let r = write_read(d, &[0x00, 0xAA, 0x01], Duration::from_millis(300))
                .filter(|r| r.len() >= 3)?;
            let rival650 = pid == 0x172B || pid == 0x1726;
            let name = if rival650 {
                "SteelSeries Rival 650 Wireless"
            } else if pid == 0x1872 {
                "SteelSeries Rival 3 Wireless Gen 2"
            } else {
                "SteelSeries Rival 3 Wireless"
            };
            let mut dev = mouse(pid, name, pid == 0x172B);
            if rival650 {
                // One entry whether it answers on the cable or the receiver.
                dev.key = "steelseries:172B".into();
            }
            if pid != 0x172B {
                dev.grace = RECEIVER_GRACE;
            }
            dev.percent = Some(clamp(r[0]));
            dev.charging = r[2] != 0;
            Some((dev, true))
        }
        Model::RivalPro => {
            // Rival Pro (UWB, hot-swappable batteries): 92 -> 92 00 <mouse %> <flag> <spare %>.
            d.flush();
            if !d.write(&[0x00, 0x92]) {
                return None;
            }
            let end = hid.now() + MAX_WAIT;
            let r = loop {
                let left = end.saturating_sub(hid.now());
                if left.is_zero() {
                    return None;
                }
                let r = d.read(left).filter(|r| r.len() >= 3)?;
                if r[0] == 0x92 && r[1] == 0x00 {
                    break r;
                }
            };
            if r[2] > 100 {
                return None;
            }
            let mut dev = mouse(pid, "SteelSeries Rival Pro", false);
            dev.via = "UWB base station".into();
            dev.grace = RECEIVER_GRACE;
            dev.percent = Some(r[2]);
            Some((dev, true))
        }
    }
}

fn poll_with(hid: &mut dyn Hid, colls: &[Coll]) -> Vec<Device> {
    let mut out: Vec<(Device, bool)> = Vec::new();
    for (vid, pid) in products(colls) {
        let Some(&(_, accept, model)) = MODELS.iter().find(|(pids, _, _)| pids.contains(&pid))
        else {
            continue;
        };
        for c in find(colls, vid, pid, accept) {
            let Some(mut d) = hid.open(c) else { continue };
            let Some(s) = read(hid, d.as_mut(), model, pid) else {
                continue;
            };
            // Aerox / Prime and Rival 650 can answer on both products; keep one entry, preferring the
            // cable.
            match out.iter_mut().find(|(o, _)| o.key == s.0.key) {
                None => out.push(s),
                Some(dup) => {
                    if s.0.via == "USB cable" {
                        *dup = s;
                    }
                }
            }
            break;
        }
    }
    // A base station whose headset is off lists nothing; the tracker keeps the last level.
    out.into_iter()
        .filter(|(_, c)| *c)
        .map(|(d, _)| d)
        .collect()
}

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    hid::run(api, infos, poll_with)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::hid::fake::{Fake, Op};
    use std::collections::{HashMap, VecDeque};
    use std::sync::{Arc, Mutex};

    #[test]
    fn pid_table_matches_models() {
        let mut from_models: Vec<u16> = MODELS.iter().flat_map(|m| m.0.iter().copied()).collect();
        let mut pids = PIDS.to_vec();
        from_models.sort_unstable();
        pids.sort_unstable();
        assert_eq!(from_models, pids);
    }

    // ---- steelseries_driver_test.cpp (Nova Pro Wireless) ----

    fn nova_coll(pid: u16) -> Coll {
        Coll {
            vid: VID,
            pid,
            iface: 4,
            page: 0xFFC0,
            usage: 1,
            in_len: 64,
            out_len: 64,
            feat_len: 1024,
            path: "ffc0".into(),
            ..Default::default()
        }
    }

    fn nova_reply() -> Vec<u8> {
        let mut r = vec![0u8; 64];
        r[0] = 0x06;
        r[1] = 0xB0;
        r[6] = 4;
        r[15] = 0x08;
        r
    }

    /// The base station: writes 06 B0 (rejected on the FF00 collection), each read returns `reply`.
    fn nova(reply: Vec<u8>, write_ok: bool) -> Fake {
        Fake::with(move |_, c, op| match op.clone() {
            Op::Write(w) => {
                assert_eq!(w, [0x06, 0xB0]);
                (write_ok && c.page == 0xFFC0).then(Vec::new)
            }
            Op::Read(_) => (!reply.is_empty()).then(|| reply.clone()),
            Op::Flush => Some(vec![]),
            _ => None,
        })
    }

    fn count(f: &Fake) -> (usize, usize, usize) {
        let opened = f.s().opened.len();
        (
            opened,
            f.count(|o| matches!(o, Op::Write(_))),
            f.count(|o| matches!(o, Op::Read(_))),
        )
    }

    #[test]
    fn nova_pro_wireless_battery() {
        let mut f = nova(nova_reply(), true);
        let out = poll_with(&mut f, &[nova_coll(0x225D)]);
        assert_eq!(out.len(), 1);
        let s = &out[0];
        assert_eq!(s.name, "SteelSeries Arctis Nova Pro Wireless");
        assert_eq!(s.key, "steelseries:225D");
        assert_eq!(
            (s.kind, s.via.as_str()),
            (Kind::Headset, "2.4 GHz receiver")
        );
        assert_eq!((s.percent, s.charging), (Some(50), false));
        assert_eq!(count(&f), (1, 1, 1));

        let out = poll_with(&mut nova(nova_reply(), true), &[nova_coll(0x12E0)]);
        assert_eq!(out[0].key, "steelseries:12E0");
        assert_eq!(out[0].percent, Some(50));
    }

    #[test]
    fn nova_pro_skips_the_ff00_collection() {
        let mut other = nova_coll(0x225D);
        other.page = 0xFF00;
        other.feat_len = 64;
        other.path = "ff00".into();
        // FFC0 first: one open. FF00 first: its write is rejected, then FFC0 answers.
        let mut f = nova(nova_reply(), true);
        assert_eq!(
            poll_with(&mut f, &[nova_coll(0x225D), other.clone()]).len(),
            1
        );
        assert_eq!(count(&f), (1, 1, 1));
        let mut f = nova(nova_reply(), true);
        let out = poll_with(&mut f, &[other, nova_coll(0x225D)]);
        assert_eq!(out[0].percent, Some(50));
        assert_eq!(count(&f), (2, 2, 1));
    }

    #[test]
    fn nova_pro_charging_and_offline() {
        for level in [0u8, 8] {
            let mut r = nova_reply();
            r[6] = level;
            r[15] = 0x02;
            let out = poll_with(&mut nova(r, true), &[nova_coll(0x225D)]);
            assert_eq!(out[0].percent, Some(if level == 0 { 0 } else { 100 }));
            assert!(out[0].charging);
        }
        let mut r = nova_reply();
        r[15] = 0x01;
        assert!(poll_with(&mut nova(r, true), &[nova_coll(0x225D)]).is_empty());
    }

    #[test]
    fn nova_pro_failures_and_filtering() {
        for len in [0, 15] {
            let mut r = nova_reply();
            r.truncate(len);
            let mut f = nova(r, true);
            assert!(poll_with(&mut f, &[nova_coll(0x225D)]).is_empty());
            assert_eq!(f.count(|o| matches!(o, Op::Read(_))), 1);
        }
        let mut f = nova(nova_reply(), false);
        assert!(poll_with(&mut f, &[nova_coll(0x225D)]).is_empty());
        assert_eq!(count(&f), (1, 1, 0));
        let mut f = nova(nova_reply(), true);
        f.s().refuse_open = true;
        assert!(poll_with(&mut f, &[nova_coll(0x225D)]).is_empty());
        assert_eq!(count(&f), (1, 0, 0));
        for invalid in 0..3 {
            let mut c = nova_coll(0x225D);
            match invalid {
                0 => c.pid = 0x225E,
                1 => c.iface = 3,
                _ => c.out_len = 0,
            }
            let mut f = nova(nova_reply(), true);
            assert!(poll_with(&mut f, &[c]).is_empty());
            assert_eq!(count(&f), (0, 0, 0));
        }
    }

    // ---- steelseries_mouse_test.cpp (Aerox / Prime) ----

    struct Mouse {
        queued: VecDeque<Vec<u8>>,
        expected: u8,
        write_ok: bool,
        writes: usize,
        reply_on_write: usize,
        reply: Vec<u8>,
        noise: Vec<u8>,
        by_pid: HashMap<u16, Vec<u8>>,
    }

    fn mouse_fake(m: Arc<Mutex<Mouse>>) -> Fake {
        Fake::with(move |_, c, op| {
            let mut m = m.lock().unwrap();
            match op.clone() {
                Op::Flush => {
                    m.queued.clear();
                    Some(vec![])
                }
                Op::Write(w) => {
                    if !m.by_pid.is_empty() {
                        let r = m.by_pid[&c.pid].clone();
                        assert_eq!(w, [0x00, r[0]]);
                        m.queued.push_back(r);
                        return Some(vec![]);
                    }
                    assert_eq!(w, [0x00, m.expected]);
                    if !m.write_ok {
                        return None;
                    }
                    m.writes += 1;
                    if !m.noise.is_empty() {
                        let n = m.noise.clone();
                        m.queued.push_back(n);
                    }
                    if m.writes == m.reply_on_write && !m.reply.is_empty() {
                        let r = m.reply.clone();
                        m.queued.push_back(r);
                    }
                    Some(vec![])
                }
                Op::Read(t) => {
                    assert!(t <= Duration::from_millis(100));
                    m.queued.pop_front()
                }
                _ => None,
            }
        })
    }

    fn mouse_coll(pid: u16) -> Coll {
        Coll {
            vid: VID,
            pid,
            iface: 3,
            page: 0xFFC0,
            usage: 1,
            in_len: 65,
            out_len: 65,
            path: format!("if3-{pid:04X}"),
            ..Default::default()
        }
    }

    fn new_mouse() -> Arc<Mutex<Mouse>> {
        Arc::new(Mutex::new(Mouse {
            queued: VecDeque::new(),
            expected: 0xD2,
            write_ok: true,
            writes: 0,
            reply_on_write: 1,
            reply: vec![],
            noise: vec![],
            by_pid: HashMap::new(),
        }))
    }

    fn poll_mouse(m: &Arc<Mutex<Mouse>>, pid: u16, cmd: u8, payload: &[u8]) -> Vec<Device> {
        {
            let mut s = m.lock().unwrap();
            s.expected = cmd;
            s.writes = 0;
            s.reply = if payload.is_empty() {
                vec![]
            } else {
                [&[cmd][..], payload].concat()
            };
        }
        poll_with(&mut mouse_fake(m.clone()), &[mouse_coll(pid)])
    }

    struct M {
        pid: u16,
        wired: bool,
        name: &'static str,
        key: &'static str,
    }

    fn expect(m: &Arc<Mutex<Mouse>>, model: &M, payload: &[u8], pct: Option<u8>, charging: bool) {
        let cmd = if model.wired { 0x92 } else { 0xD2 };
        let out = poll_mouse(m, model.pid, cmd, payload);
        assert_eq!(out.len(), 1, "{:04X} {payload:02X?}", model.pid);
        let s = &out[0];
        assert_eq!(
            (s.percent, s.charging),
            (pct, charging),
            "{:04X} {payload:02X?}",
            model.pid
        );
        assert_eq!(s.name, model.name);
        assert_eq!(s.grace, Duration::from_secs(1800));
        assert_eq!(s.key, model.key);
        assert_eq!(s.kind, Kind::Mouse);
        assert_eq!(
            s.via,
            if model.wired {
                "USB cable"
            } else {
                "2.4 GHz receiver"
            }
        );
    }

    fn expect_nothing(m: &Arc<Mutex<Mouse>>, pid: u16, cmd: u8, payload: &[u8]) {
        assert!(poll_mouse(m, pid, cmd, payload).is_empty());
    }

    fn expect_one_mouse(
        m: &Arc<Mutex<Mouse>>,
        receiver: &M,
        cable: &M,
        wireless: &[u8],
        wired: &[u8],
    ) {
        m.lock().unwrap().by_pid = HashMap::from([
            (receiver.pid, wireless.to_vec()),
            (cable.pid, wired.to_vec()),
        ]);
        let out = poll_with(
            &mut mouse_fake(m.clone()),
            &[mouse_coll(cable.pid), mouse_coll(receiver.pid)],
        );
        assert_eq!(out.len(), 1);
        let s = &out[0];
        assert_eq!(
            (s.key.as_str(), s.name.as_str()),
            (receiver.key, receiver.name)
        );
        assert_eq!(
            (s.via.as_str(), s.percent, s.charging),
            ("USB cable", Some(95), true)
        );
        m.lock().unwrap().by_pid.clear();
        // Unplugging the cable keeps the key and returns to the wireless level.
        expect(m, receiver, &wireless[1..], Some(45), false);
    }

    const STEP_MODELS: &[M] = &[
        M {
            pid: 0x1838,
            wired: false,
            name: "SteelSeries Aerox 3 Wireless",
            key: "steelseries:1838",
        },
        M {
            pid: 0x183A,
            wired: true,
            name: "SteelSeries Aerox 3 Wireless",
            key: "steelseries:1838",
        },
        M {
            pid: 0x1878,
            wired: false,
            name: "SteelSeries Aerox 3 Wireless CS2 Dragon Lore Edition",
            key: "steelseries:1878",
        },
        M {
            pid: 0x187A,
            wired: true,
            name: "SteelSeries Aerox 3 Wireless CS2 Dragon Lore Edition",
            key: "steelseries:1878",
        },
        M {
            pid: 0x1852,
            wired: false,
            name: "SteelSeries Aerox 5 Wireless",
            key: "steelseries:1852",
        },
        M {
            pid: 0x1854,
            wired: true,
            name: "SteelSeries Aerox 5 Wireless",
            key: "steelseries:1852",
        },
        M {
            pid: 0x1858,
            wired: false,
            name: "SteelSeries Aerox 9 Wireless",
            key: "steelseries:1858",
        },
        M {
            pid: 0x185A,
            wired: true,
            name: "SteelSeries Aerox 9 Wireless",
            key: "steelseries:1858",
        },
        M {
            pid: 0x1840,
            wired: false,
            name: "SteelSeries Prime Wireless",
            key: "steelseries:1840",
        },
        M {
            pid: 0x1842,
            wired: true,
            name: "SteelSeries Prime Wireless",
            key: "steelseries:1840",
        },
        M {
            pid: 0x1860,
            wired: false,
            name: "SteelSeries Aerox 5 Wireless Diablo IV Edition",
            key: "steelseries:1860",
        },
    ];

    const GEN2: &[M] = &[
        M {
            pid: 0x1890,
            wired: false,
            name: "SteelSeries Aerox 3 Wireless Gen 2",
            key: "steelseries:1890",
        },
        M {
            pid: 0x1892,
            wired: true,
            name: "SteelSeries Aerox 3 Wireless Gen 2",
            key: "steelseries:1890",
        },
    ];

    #[test]
    fn aerox_step_layout() {
        let samples: &[(&[u8], Option<u8>, bool)] = &[
            (&[0x01], Some(0), false),
            (&[0x13], Some(90), false),
            (&[0x15], Some(100), false),
            (&[0x81], Some(0), true),
            (&[0x93], Some(90), true),
            (&[0x95], Some(100), true),
            // Captured on a Dragon Lore Edition: receiver on battery, then cable while charging.
            (&[0x0A, 0x00, 0x34, 0x2E, 0x31], Some(45), false),
            (&[0x8B, 0x00, 0x34, 0x2E, 0x31], Some(50), true),
            (&[0x8C, 0x00], Some(55), true),
            // Out-of-range levels stay unknown and never invent a charging state.
            (&[0xFF], None, false),
            (&[0x7F], None, false),
            (&[0x16], None, false),
            (&[0x96], None, false),
        ];
        let m = new_mouse();
        for model in STEP_MODELS {
            for (payload, pct, charging) in samples {
                expect(&m, model, payload, *pct, *charging);
            }
            // Zero means off or asleep, even with the charging bit: left out.
            let cmd = if model.wired { 0x92 } else { 0xD2 };
            expect_nothing(&m, model.pid, cmd, &[0x00]);
            expect_nothing(&m, model.pid, cmd, &[0x80]);
        }
        for pair in STEP_MODELS.as_chunks::<2>().0 {
            expect_one_mouse(&m, &pair[0], &pair[1], &[0xD2, 0x0A], &[0x92, 0x94]);
        }
    }

    #[test]
    fn aerox_gen2_percent_layout() {
        let m = new_mouse();
        for model in GEN2 {
            // Captured on hardware in rivalcfg issue #276.
            expect(&m, model, &[0x00, 0x61, 0x00], Some(97), false);
            expect(&m, model, &[0x01, 0x56, 0x00], Some(86), true);
            expect(&m, model, &[0x02, 0x64, 0x00], Some(100), true);
            expect(&m, model, &[0x00, 0xFF, 0x00], None, false); // not measured yet
            expect(&m, model, &[0x01, 0xFF, 0x00], None, true);
            expect(&m, model, &[0x01, 0x00, 0x00], Some(0), true);
            expect(&m, model, &[0x00, 0x65, 0x00], None, false); // above 100: unknown
            let cmd = if model.wired { 0x92 } else { 0xD2 };
            // 00 00 through the receiver = off; on the cable it is a real 0%.
            if model.wired {
                expect(&m, model, &[0x00, 0x00, 0x00], Some(0), false);
            } else {
                expect_nothing(&m, model.pid, cmd, &[0x00, 0x00, 0x00]);
            }
            expect_nothing(&m, model.pid, cmd, &[0x12, 0x00, 0x00]); // step layout, not Gen 2
            expect_nothing(&m, model.pid, cmd, &[0x01]); // too short for the level byte
        }
        expect_one_mouse(
            &m,
            &GEN2[0],
            &GEN2[1],
            &[0xD2, 0x00, 0x2D],
            &[0x92, 0x01, 0x5F],
        );
    }

    #[test]
    fn aerox_echo_filtering_resend_and_nack() {
        let m = new_mouse();
        // Reports that do not echo the command are skipped.
        m.lock().unwrap().noise = vec![0x11, 0x00];
        expect(&m, &STEP_MODELS[2], &[0x0A], Some(45), false);
        // GG talking over the query: the receiver answers the third write.
        m.lock().unwrap().noise = vec![0x61, 0x00, 0x31, 0x31, 0x2E, 0x34, 0x00, 0x00];
        expect(
            &m,
            &STEP_MODELS[0],
            &[0x0A, 0x31, 0x31, 0x2E, 0x34, 0x00, 0x00],
            Some(45),
            false,
        );
        m.lock().unwrap().reply_on_write = 3;
        expect(&m, &STEP_MODELS[2], &[0x0A], Some(45), false);
        assert_eq!(m.lock().unwrap().writes, 3);
        expect(&m, &GEN2[0], &[0x00, 0x61, 0x00], Some(97), false);
        // No echo before the deadline: nothing reported, and the query was resent.
        expect_nothing(&m, 0x1878, 0xD2, &[]);
        let w = m.lock().unwrap().writes;
        assert!((5..=11).contains(&w), "{w}");
        m.lock().unwrap().reply_on_write = 1;
        // Mouse switched off: the receiver answers 40 FF, which ends the wait at once.
        m.lock().unwrap().noise = vec![0x40, 0xFF, 0x31, 0x34, 0x2E, 0x31];
        expect_nothing(&m, 0x1878, 0xD2, &[]);
        assert_eq!(m.lock().unwrap().writes, 1);
        m.lock().unwrap().noise.clear();
        // I/O failures.
        m.lock().unwrap().write_ok = false;
        expect_nothing(&m, 0x1838, 0xD2, &[0x13]);
        assert_eq!(m.lock().unwrap().writes, 0);
        m.lock().unwrap().write_ok = true;
        expect_nothing(&m, 0x1838, 0xD2, &[]); // receiver never answers
    }

    #[test]
    fn headset_parsers() {
        assert_eq!(
            parse_arctis1(&[0x06, 0x12, 0x00, 77]),
            status(Some(77), false, true)
        );
        assert_eq!(
            parse_arctis1(&[0x06, 0x12, 0x01, 77]).map(|s| s.connected),
            Some(false)
        );
        assert_eq!(
            parse_arctis9(&[0, 0x20, 0, 0x9A, 1]),
            status(Some(100), true, true)
        );
        // Nova 7: percent at [2], [3] 01 / 02 charging, 00 offline.
        let r = [0x00, 0xB0, 64, 0x01, 0, 0, 0];
        assert_eq!(parse_nova_family(0x227E, &r), status(Some(64), true, true));
        // GameBuds: lowest of the buds out of the case.
        let r = [0, 0xB0, 0, 0x03, 0x03, 80, 60];
        assert_eq!(parse_nova_family(0x230A, &r).unwrap().percent, Some(60));
        assert!(
            parse_nova_family(0x230A, &[0, 0xB0, 0, 1, 1, 80, 60]).is_some_and(|s| !s.connected)
        );
    }
}

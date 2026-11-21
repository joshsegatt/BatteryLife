//! ATK / VXE mice that do not speak the Kysona protocol, on VID 373B:
//!  * NK mouse NANO dongle 1216 (VXE R1 NK and other NearLink mice): ATK's BITMOUSE protocol on
//!    FF05:0001. Output report 08, 64 bytes: [08][checksum][72][02][00][01][cmd][01][00 ...],
//!    checksum = sum(bytes 2..63); cmd 07 = battery level, 0F = charging. Reply
//!    [08][72][status][seq][00][cmd][01][value]; status FF = error.
//!  * ATK Zero: same framing with checksum byte 7D, target 00 on the cable 1154 or 01 on the 8K
//!    receivers 124F / 1155; only status 00 is fresh; command 08 reports charging (02) on the cable.
//!  * ATK 8K Nano Dongle 1178 (Holtek): feature report 0 [82] (GetConfigData), answered as an input
//!    report [82][rate][dpi][light][brightness][speed][color][reserved][battery][link]; battery
//!    bit 7 = charging, bits 0..6 = percent; link 0 = no mouse.
//!
//! 373B is shared with Kysona and other ATK drivers, so only these ids are declared.
//!
//! Ported from BatteryHub (src/drivers/atk_bitmouse.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0, which cites
//! github.com/HeyOkay/HaloBattery PR #350, github.com/qwr79/mouse-battery,
//! github.com/Fan4Metal/mouse_tray (nordic54.py) and ATK HUB (v3-hub.atkgear.com). Replies are
//! awaited 500 ms (BatteryHub: 700 ms for the NK dongle and the 8K Nano).

use std::time::{Duration, Instant};

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::hid_dev::{BUDGET, Coll, Dev, HidIo, Io, colls, find, ms_left, read_until};
use crate::registry::Match;

const VID: u16 = 0x373B;
const NANO_DONGLE: u16 = 0x1216;
const NANO_8K_DONGLE: u16 = 0x1178;
const ZERO_CABLE: u16 = 0x1154;
const ZERO_RECEIVERS: [u16; 2] = [0x124F, 0x1155];
const REPLY_MS: u64 = 500;

pub static MATCHES: &[Match] = &[Match {
    vid: VID,
    pids: &[NANO_DONGLE, NANO_8K_DONGLE, ZERO_CABLE, 0x124F, 0x1155],
}];

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    poll_with(&colls(infos), &mut HidIo(api))
}

#[derive(Clone, Copy, PartialEq)]
pub enum Link {
    NearLink,
    ZeroCable,
    ZeroReceiver,
}

pub fn request(cmd: u8, link: Link) -> [u8; 64] {
    let mut q = [0u8; 64];
    q[0] = 0x08;
    q[2] = 0x72;
    q[3] = 0x02; // request length
    q[5] = if link == Link::ZeroCable { 0x00 } else { 0x01 };
    q[6] = cmd;
    q[7] = 0x01; // expected reply length
    q[1] = if link == Link::NearLink {
        q[2..].iter().fold(0u8, |a, b| a.wrapping_add(*b))
    } else {
        0x7D
    };
    q
}

/// The reply's value: Some(Ok(v)), Some(Err(())) for an error status, None for another report.
pub fn parse(r: &[u8], cmd: u8, link: Link) -> Option<Result<u8, ()>> {
    let zero = link != Link::NearLink;
    if r.len() < 8 || r[0] != 0x08 || r[1] != 0x72 || r[5] != cmd || (zero && r[6] != 0x01) {
        return None;
    }
    let error = if zero { r[2] != 0x00 } else { r[2] == 0xFF };
    Some(if error { Err(()) } else { Ok(r[7]) })
}

fn query(d: &mut dyn Dev, cmd: u8, link: Link, end: Instant) -> Option<u8> {
    d.flush();
    if !d.write(&request(cmd, link)) {
        return None;
    }
    let end = end.min(Instant::now() + Duration::from_millis(REPLY_MS));
    let r = read_until(d, end, |r| parse(r, cmd, link).is_some())?;
    parse(&r, cmd, link)?.ok()
}

fn mouse(key: &str, name: &str, via: &str, pct: u8, charging: bool) -> Device {
    let mut s = Device::new(key, name, Kind::Mouse, via);
    s.percent = Some(pct);
    s.charging = charging;
    // The dongle stops answering while the mouse sleeps: keep the last value.
    s.grace = Duration::from_secs(1800);
    s
}

fn poll_zero(colls: &[Coll], io: &mut dyn Io, end: Instant) -> Option<Device> {
    // The cable first, for a fresh reading while charging; one key on either link.
    for pid in [ZERO_CABLE, ZERO_RECEIVERS[0], ZERO_RECEIVERS[1]] {
        for c in find(colls, VID, pid, |c| {
            c.page == 0xFF05 && c.usage == 1 && c.out_len == 64 && c.in_len == 64
        }) {
            if ms_left(end) == 0 {
                return None;
            }
            let Some(mut d) = io.open(c) else { continue };
            let wired = pid == ZERO_CABLE;
            let link = if wired {
                Link::ZeroCable
            } else {
                Link::ZeroReceiver
            };
            let Some(level) = query(d.as_mut(), 0x07, link, end).filter(|l| *l <= 100) else {
                continue;
            };
            let charging = wired && query(d.as_mut(), 0x08, link, end) == Some(0x02);
            return Some(mouse(
                "atkzero:1154",
                "ATK Zero",
                if wired {
                    "USB cable"
                } else {
                    "2.4 GHz receiver"
                },
                level,
                charging,
            ));
        }
    }
    None
}

/// 8K Nano GetConfigData reply: (percent, charging); None without a linked mouse or valid level.
pub fn parse_nano_8k(r: &[u8]) -> Option<(u8, bool)> {
    if r.len() < 10 || r[0] != 0x82 || r[9] == 0 {
        return None;
    }
    let level = r[8] & 0x7F;
    (1..=100)
        .contains(&level)
        .then_some((level, r[8] & 0x80 != 0))
}

fn poll_nano_8k(colls: &[Coll], io: &mut dyn Io, end: Instant) -> Option<Device> {
    for c in find(colls, VID, NANO_8K_DONGLE, |c| {
        c.page == 0xFF10 && c.usage == 1 && c.feat_len >= 9 && c.in_len >= 11
    }) {
        if ms_left(end) == 0 {
            return None;
        }
        let Some(mut d) = io.open(c) else { continue };
        d.flush();
        if !d.set_feature(&[0x00, 0x82]) {
            continue;
        }
        let until = end.min(Instant::now() + Duration::from_millis(REPLY_MS));
        // Other input reports (e.g. 12 DPI / rate notifications) are skipped; short ones end the wait.
        let mut short = false;
        let Some(r) = read_until(d.as_mut(), until, |r| {
            short |= r.len() < 10;
            short || r[0] == 0x82
        }) else {
            continue;
        };
        if short {
            continue;
        }
        // The dongle answered: no linked mouse (or a bad level) ends the search for it.
        let (level, charging) = parse_nano_8k(&r)?;
        return Some(mouse(
            "atknano8k:1178",
            "ATK 8K Nano mouse",
            "2.4 GHz receiver",
            level,
            charging,
        ));
    }
    None
}

fn poll_nano(colls: &[Coll], io: &mut dyn Io, end: Instant) -> Option<Device> {
    for c in find(colls, VID, NANO_DONGLE, |c| {
        c.page == 0xFF05 && c.usage == 1 && c.out_len >= 64 && c.in_len >= 8
    }) {
        if ms_left(end) == 0 {
            return None;
        }
        let Some(mut d) = io.open(c) else { continue };
        // Asleep / off, or no valid reading.
        let Some(level) =
            query(d.as_mut(), 0x07, Link::NearLink, end).filter(|l| (1..=100).contains(l))
        else {
            continue;
        };
        let charging = query(d.as_mut(), 0x0F, Link::NearLink, end) == Some(1);
        return Some(mouse(
            "atknk:1216",
            "ATK / VXE NK mouse",
            "2.4 GHz receiver",
            level,
            charging,
        ));
    }
    None
}

fn poll_with(colls: &[Coll], io: &mut dyn Io) -> Vec<Device> {
    let end = Instant::now() + BUDGET;
    [
        poll_zero(colls, io, end),
        poll_nano_8k(colls, io, end),
        poll_nano(colls, io, end),
    ]
    .into_iter()
    .flatten()
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::hid_dev::fake::{Fake, Sent, State, coll};
    use std::sync::{Arc, Mutex};

    fn atk(pid: u16) -> Coll {
        coll(VID, pid, 2, 0xFF05, 1, 64, 64, 0)
    }

    #[derive(Clone, Copy)]
    struct Cfg {
        level: u8,
        status: u8,
        bad_header: bool,
    }

    /// The fake of atk_zero_driver_test: checks every request, then queues a short report, a reply
    /// to another command and the real reply.
    fn device(cfg: Arc<Mutex<Cfg>>) -> Fake {
        Fake::with(move |s: &mut State, path: &str, kind, q| {
            assert_eq!(kind, Sent::Write);
            assert!(q.len() == 64 && q[0] == 8 && q[2] == 0x72 && q[3] == 2 && q[7] == 1);
            let nk = path.contains(":1216:");
            let cable = path.contains(":1154:");
            assert_eq!(q[5], if cable { 0 } else { 1 });
            if nk {
                let sum = q[2..].iter().fold(0u8, |a, b| a.wrapping_add(*b));
                assert_eq!(q[1], sum);
            } else {
                assert!(q[1] == 0x7D && (q[6] == 7 || (cable && q[6] == 8)));
            }
            let c = *cfg.lock().unwrap();
            let value = if q[6] == 7 {
                c.level
            } else if nk {
                1
            } else {
                2
            };
            let mut r = vec![8, 0x72, c.status, 0x3A, 0, q[6], 1, value];
            if c.bad_header {
                r[6] = 0;
            }
            s.push_input("", vec![8]);
            let mut wrong = r.clone();
            wrong[5] = 0xFF;
            s.push_input("", wrong);
            s.push_input("", r);
        })
    }

    fn cfg(level: u8, status: u8, bad_header: bool) -> Arc<Mutex<Cfg>> {
        Arc::new(Mutex::new(Cfg {
            level,
            status,
            bad_header,
        }))
    }

    #[test]
    fn zero_on_every_link() {
        for pid in [0x1154, 0x124F, 0x1155] {
            let out = poll_with(&[atk(pid)], &mut device(cfg(46, 0, false)));
            assert_eq!(out.len(), 1);
            assert_eq!((out[0].percent, out[0].charging), (Some(46), pid == 0x1154));
            assert_eq!(out[0].key, "atkzero:1154");
        }
        let mut f = device(cfg(46, 0, false));
        let out = poll_with(&[atk(0x1155), atk(0x124F), atk(0x1154)], &mut f);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].via, "USB cable");
        assert_eq!(f.s().opens, 1);
    }

    #[test]
    fn zero_rejects_bad_levels_and_stale_replies() {
        for n in [0u8, 100, 101] {
            let out = poll_with(&[atk(0x124F)], &mut device(cfg(n, 0, false)));
            assert_eq!(out.len(), usize::from(n <= 100));
        }
        assert!(poll_with(&[atk(0x124F)], &mut device(cfg(46, 1, false))).is_empty());
        assert!(poll_with(&[atk(0x124F)], &mut device(cfg(46, 0, true))).is_empty());
    }

    #[test]
    fn nearlink_dongle_reads_level_and_charging() {
        let out = poll_with(&[atk(0x1216)], &mut device(cfg(46, 0, false)));
        assert_eq!(out.len(), 1);
        assert_eq!((out[0].percent, out[0].charging), (Some(46), true));
        assert_eq!(out[0].key, "atknk:1216");
    }

    #[test]
    fn other_collections_are_never_opened() {
        for fault in 0..6 {
            let mut c = atk(0x124F);
            match fault {
                0 => c.vid += 1,
                1 => c.pid = 0xFFFF,
                2 => c.page += 1,
                3 => c.usage += 1,
                4 => c.in_len -= 1,
                _ => c.out_len += 1,
            }
            let mut f = device(cfg(46, 0, false));
            assert!(poll_with(&[c], &mut f).is_empty());
            assert_eq!(f.s().opens, 0);
        }
    }

    #[test]
    fn nano_8k_config_reply() {
        let mut r = vec![0x82, 1, 2, 3, 4, 5, 6, 0, 0x80 | 71, 1, 0];
        assert_eq!(parse_nano_8k(&r), Some((71, true)));
        r[9] = 0;
        assert_eq!(parse_nano_8k(&r), None, "no mouse linked");
        let mut f = Fake::with(|s, _, kind, q| {
            assert_eq!((kind, q), (Sent::Feature, &[0x00, 0x82][..]));
            s.push_input("", vec![0x12, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            s.push_input("", vec![0x82, 1, 2, 3, 4, 5, 6, 0, 55, 1, 0]);
        });
        let out = poll_with(&[coll(VID, 0x1178, 0, 0xFF10, 1, 65, 65, 9)], &mut f);
        assert_eq!((out[0].percent, out[0].charging), (Some(55), false));
    }
}

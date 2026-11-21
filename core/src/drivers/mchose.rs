//! MCHOSE wireless mice on VID 5253 (L7 / M7 / A7 / K7 families, receiver or cable) and the same
//! protocol on the Realtek ODM dongles 3837:100C / 100B / 100A / 1001 / 1012 (A5 V2 Ultra, A7 V2
//! Ultra+, K7 Ultra). 65-byte feature report 12 on FF01:0001 with every payload byte inverted:
//!   request [12][F9 = 06 ^ FF][FF ...]
//!   reply   [12][F9][payload ^ FF]: vid:u16, model:u16, firmware:u32, flags (09 receiver, 00 cable),
//!           level, charging. An all-zero payload is an echo while the mouse sleeps.
//!
//! Every product of 5253 is tried (BatteryHub scans the whole vendor); on 3837 only the listed
//! dongles, since its keyboards and the G3 V2 speak other protocols.
//!
//! Ported from BatteryHub (src/drivers/mchose.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0, which cites
//! github.com/alexfrih/mchose-linux (PROTOCOL.md).

use std::time::{Duration, Instant};

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::hid::{self, BUDGET, Coll, Hid, find, hex4, left, pids};
use crate::registry::Match;

const VID: u16 = 0x5253;
const VID_REALTEK: u16 = 0x3837;
const REALTEK_PIDS: [u16; 5] = [0x100C, 0x100B, 0x100A, 0x1001, 0x1012];
const REPORT: u8 = 0x12;
const CMD_STATUS: u8 = 0x06;

pub static MATCHES: &[Match] = &[
    Match {
        vid: VID,
        pids: &[],
    },
    Match {
        vid: VID_REALTEK,
        pids: &[0x100C, 0x100B, 0x100A, 0x1001, 0x1012],
    },
];

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    hid::run(api, infos, |hid, colls| poll_with(colls, hid))
}

#[derive(Debug, PartialEq)]
pub enum Status {
    /// level, charging, flags
    Battery(u8, bool, u8),
    /// A valid echo with an empty payload: the mouse is asleep.
    Asleep,
}

/// Decodes a reply; `vid_field` / `alt` are the vendor ids accepted in the payload.
pub fn parse(r: &[u8], vid_field: u16, alt: u16) -> Option<Status> {
    if r.len() < 13 || r[0] != REPORT || r[1] != CMD_STATUS ^ 0xFF {
        return None;
    }
    let p: Vec<u8> = r[2..13].iter().map(|b| b ^ 0xFF).collect();
    if p.iter().all(|b| *b == 0) {
        return Some(Status::Asleep);
    }
    let field = u16::from_le_bytes([p[0], p[1]]);
    if field != vid_field && field != alt {
        return None;
    }
    (p[9] <= 100).then_some(Status::Battery(p[9], p[10] != 0, p[8]))
}

fn poll_product(
    colls: &[Coll],
    io: &mut dyn Hid,
    vid: u16,
    pid: u16,
    vid_field: u16,
    alt: u16,
    end: Instant,
) -> Option<Device> {
    for c in find(colls, vid, pid, |c| {
        c.page == 0xFF01 && c.usage == 1 && c.feat_len >= 65
    }) {
        if left(end).is_zero() {
            return None;
        }
        let Some(mut d) = io.open(c) else { continue };
        let mut req = vec![0xFFu8; c.feat_len as usize]; // inverted zero padding
        req[0] = REPORT;
        req[1] = CMD_STATUS ^ 0xFF;
        // The request must be sent again for every read: reading again returns the request.
        let mut best: Option<Status> = None;
        for attempt in 0..5 {
            if matches!(best, Some(Status::Battery(..))) {
                break;
            }
            if attempt > 0 {
                io.sleep(Duration::from_millis(100));
            }
            if !d.set_feature(&req) {
                break;
            }
            io.sleep(Duration::from_millis(120));
            match d
                .get_feature(REPORT)
                .and_then(|r| parse(&r, vid_field, alt))
            {
                Some(st @ Status::Battery(..)) => best = Some(st),
                Some(Status::Asleep) if best.is_none() => best = Some(Status::Asleep),
                _ => {}
            }
        }
        let Some(st) = best else { continue };
        let mut s = Device::new(
            format!("mchose:{}", hex4(pid)),
            c.product_or("MCHOSE mouse"),
            Kind::Mouse,
            "2.4 GHz receiver",
        );
        match st {
            Status::Battery(level, charging, flags) => {
                s.percent = Some(level);
                s.charging = charging;
                if flags == 0 && charging {
                    s.via = "USB cable".into();
                }
            }
            Status::Asleep => s.asleep = true,
        }
        // BatteryHub reports a sleeping mouse as asleep (last value kept); 30 min like other receivers.
        s.grace = Duration::from_secs(1800);
        return Some(s);
    }
    None
}

fn poll_with(colls: &[Coll], io: &mut dyn Hid) -> Vec<Device> {
    let end = Instant::now() + BUDGET;
    let mut out = Vec::new();
    for pid in pids(colls, VID) {
        out.extend(poll_product(colls, io, VID, pid, VID, VID, end));
    }
    // The vid field differs between 3837 dongles: 41E4 on the A5 V2 Ultra (100C), 3837 on the K7
    // Ultra (1001).
    for pid in REALTEK_PIDS {
        out.extend(poll_product(
            colls,
            io,
            VID_REALTEK,
            pid,
            0x41E4,
            VID_REALTEK,
            end,
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::hid::fake::{self, Fake, iface_coll as coll};

    /// Reply with the payload inverted, as the receiver sends it.
    fn reply(vid: u16, flags: u8, level: u8, charging: u8) -> Vec<u8> {
        let mut p = [0u8; 11];
        p[..2].copy_from_slice(&vid.to_le_bytes());
        p[2..4].copy_from_slice(&0x1020u16.to_le_bytes());
        p[8] = flags;
        p[9] = level;
        p[10] = charging;
        let mut r = vec![REPORT, 0xF9];
        r.extend(p.iter().map(|b| b ^ 0xFF));
        r.resize(65, 0xFF);
        r
    }

    #[test]
    fn decodes_inverted_payload() {
        assert_eq!(
            parse(&reply(VID, 9, 77, 0), VID, VID),
            Some(Status::Battery(77, false, 9))
        );
        assert_eq!(
            parse(&reply(0x41E4, 0, 40, 1), 0x41E4, VID_REALTEK),
            Some(Status::Battery(40, true, 0))
        );
        assert!(parse(&reply(0x1234, 9, 77, 0), VID, VID).is_none());
        assert!(parse(&reply(VID, 9, 101, 0), VID, VID).is_none());
        let mut echo = vec![REPORT, 0xF9];
        echo.resize(65, 0xFF);
        assert_eq!(parse(&echo, VID, VID), Some(Status::Asleep));
    }

    #[test]
    fn receiver_and_cable_readings() {
        let mut f = Fake::with(|s, fc, op| {
            if let Some(q) = op.sent() {
                assert!(q[0] == REPORT && q[1] == 0xF9 && q[2..].iter().all(|b| *b == 0xFF));
                s.push_feature("", reply(VID, 0, 64, 1));
            }
            fake::queued(s, fc, op)
        });
        f.s().pad_features = true;
        let mut c = coll(VID, 0x1020, 2, 0xFF01, 1, 0, 0, 65);
        c.product = "MCHOSE L7 Ultra".into();
        let out = poll_with(&[c], &mut f);
        assert_eq!(out.len(), 1);
        assert_eq!((out[0].percent, out[0].charging), (Some(64), true));
        assert_eq!(
            (out[0].via.as_str(), out[0].name.as_str()),
            ("USB cable", "MCHOSE L7 Ultra")
        );
        assert_eq!(out[0].key, "mchose:1020");
    }

    #[test]
    fn sleeping_mouse_is_reported_asleep() {
        let mut f = Fake::with(|s, fc, op| {
            if op.sent().is_some() {
                let mut echo = vec![REPORT, 0xF9];
                echo.resize(65, 0xFF);
                s.push_feature("", echo);
            }
            fake::queued(s, fc, op)
        });
        f.s().pad_features = true;
        let out = poll_with(&[coll(VID_REALTEK, 0x100C, 2, 0xFF01, 1, 0, 0, 65)], &mut f);
        assert!(out[0].asleep && out[0].percent.is_none());
        assert_eq!(
            f.s().sends(),
            5,
            "asks five times before settling for asleep"
        );
    }
}

//! Fantech Helios Go XD5 (25A7: cable FA7B, dongle FA7C) and Redragon Invader Pro (FA7E), Areson /
//! SinoWealth MCU. 17-byte feature report 08: [08][cmd][0 x14][checksum], all 17 bytes summing to
//! 0x55; cmd 04 = get battery -> [6] percent, [7] charging. The reply arrives as an input report on
//! another collection of the interface, so every input collection is watched; some firmware answers
//! with the feature report instead. The Invader Pro reports 0..10 (steps of 10%, coarse).
//!
//! Ported from BatteryHub (src/drivers/fantech.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0, which cites
//! github.com/jarjut/xd5-go-web and libratbag issue #1893.

use std::time::{Duration, Instant};

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::hid_dev::{BUDGET, Coll, Dev, HidIo, Io, colls, find, hex4, ms_left, products};
use crate::registry::Match;

const VID: u16 = 0x25A7;
const WIRED: u16 = 0xFA7B;
const DONGLE: u16 = 0xFA7C;
const REDRAGON: u16 = 0xFA7E;
const REPORT_ID: u8 = 0x08;

pub static MATCHES: &[Match] = &[Match {
    vid: VID,
    pids: &[WIRED, DONGLE, REDRAGON],
}];

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    poll_with(&colls(infos), &mut HidIo(api))
}

pub fn packet(cmd: u8) -> [u8; 17] {
    let mut p = [0u8; 17];
    p[0] = REPORT_ID;
    p[1] = cmd;
    p[16] = 0x55u8.wrapping_sub(p[..16].iter().fold(0u8, |a, b| a.wrapping_add(*b)));
    p
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Reply {
    pub pct: u8,
    pub charging: bool,
    pub checksum_ok: bool,
}

/// Battery reply with the command byte at index 0 or 1 (whether the stack keeps the report id
/// varies); under command at 0, the two WebHID layouts of the reference project.
pub fn parse(r: &[u8]) -> Option<Reply> {
    let sum = r.iter().take(17).fold(0u8, |a, b| a.wrapping_add(*b));
    let checksum_ok = r.len() >= 17 && sum == 0x55;
    let plausible = |power: usize, charge: usize| {
        if r.len() <= charge {
            return None;
        }
        let (p, c) = (r[power], r[charge]);
        ((1..=100).contains(&p) && c <= 1).then_some(Reply {
            pct: p,
            charging: c != 0,
            checksum_ok,
        })
    };
    if r.len() >= 7
        && r[0] == 0x04
        && let Some(x) = plausible(5, 6).or_else(|| plausible(4, 5))
    {
        return Some(x);
    }
    if r.len() >= 8 && r[1] == 0x04 {
        return plausible(6, 7);
    }
    None
}

fn remember(c: Reply, found: &mut Option<Reply>) {
    if found.is_none_or(|f| c.checksum_ok && !f.checksum_ok) {
        *found = Some(c);
    }
}

/// Sends the read-only cmd 04 on `cmd`, then watches `cmd` (when it has input) and `readers`.
fn query(
    cmd: &mut dyn Dev,
    cmd_reads: bool,
    readers: &mut [Box<dyn Dev>],
    end: Instant,
) -> Option<Reply> {
    cmd.flush();
    if !cmd.set_feature(&packet(0x04)) {
        return None;
    }
    let mut found = None;
    let end = end.min(Instant::now() + Duration::from_millis(500));
    while ms_left(end) > 0 {
        if cmd_reads
            && let Some(c) = cmd
                .read(ms_left(end).clamp(1, 40))
                .and_then(|r| parse_input(&r))
        {
            remember(c, &mut found);
        }
        for d in readers.iter_mut() {
            let left = ms_left(end);
            if left == 0 {
                break;
            }
            if let Some(c) = d.read(left.min(40)).and_then(|r| parse_input(&r)) {
                remember(c, &mut found);
            }
        }
        if found.is_some_and(|f| f.checksum_ok) {
            break;
        }
    }
    // Some firmware answers with the feature report instead of an input report.
    if let Some(c) = cmd.get_feature(REPORT_ID).and_then(|f| parse(&f)) {
        remember(c, &mut found);
    }
    found
}

/// BatteryHub only considers input reports of at least 6 bytes.
fn parse_input(r: &[u8]) -> Option<Reply> {
    if r.len() < 6 { None } else { parse(r) }
}

fn poll_with(colls: &[Coll], io: &mut dyn Io) -> Vec<Device> {
    let end = Instant::now() + BUDGET;
    let mut out = Vec::new();
    for pid in products(colls, VID) {
        if ![WIRED, DONGLE, REDRAGON].contains(&pid) || ms_left(end) == 0 {
            continue;
        }
        let all = find(colls, VID, pid, |_| true);
        let Some(feat) = all.iter().find(|c| c.feat_len >= 17) else {
            continue;
        };
        let Some(mut cmd) = io.open(feat) else {
            continue;
        };
        // Listen on every input collection of the device, not only the one written to.
        let mut readers: Vec<Box<dyn Dev>> = all
            .iter()
            .filter(|c| !std::ptr::eq(**c, *feat) && c.in_len >= 6)
            .filter_map(|c| io.open(c))
            .collect();
        let Some(reply) = query(cmd.as_mut(), feat.in_len >= 6, &mut readers, end) else {
            continue;
        };
        let redragon = pid == REDRAGON;
        let mut s = Device::new(
            format!(
                "{}{}",
                if redragon { "redragon:" } else { "fantech:" },
                hex4(pid)
            ),
            if redragon {
                "Redragon Invader Pro"
            } else {
                "Fantech Helios Go XD5"
            },
            Kind::Mouse,
            if pid == WIRED {
                "USB cable"
            } else {
                "2.4 GHz receiver"
            },
        );
        s.percent = Some(if redragon {
            (u16::from(reply.pct) * 10).min(100) as u8
        } else {
            reply.pct
        });
        s.coarse = redragon;
        s.charging = reply.charging;
        s.grace = Duration::from_secs(1800);
        out.push(s);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::hid_dev::fake::{Fake, Sent, coll};

    fn reply(pct: u8, charging: u8) -> Vec<u8> {
        let mut r = packet(0x04).to_vec();
        r[6] = pct;
        r[7] = charging;
        r[16] = 0;
        r[16] = 0x55u8.wrapping_sub(r.iter().fold(0u8, |a, b| a.wrapping_add(*b)));
        r
    }

    #[test]
    fn parses_numbered_and_unnumbered_layouts() {
        assert_eq!(
            parse(&reply(64, 1)),
            Some(Reply {
                pct: 64,
                charging: true,
                checksum_ok: true
            })
        );
        // Report id dropped: command at index 0, percent at 5.
        let r = &reply(64, 0)[1..];
        assert_eq!(parse(r).map(|x| (x.pct, x.charging)), Some((64, false)));
        assert!(parse(&reply(0, 0)).is_none());
    }

    #[test]
    fn reply_on_another_collection_is_read() {
        let feat = coll(VID, DONGLE, 1, 0xFF02, 2, 0, 0, 17);
        let input = coll(VID, DONGLE, 1, 0xFF01, 0, 17, 0, 0);
        let path = input.path.to_string_lossy().to_string();
        let mut f = Fake::with(move |s, _, kind, q| {
            assert_eq!((kind, q), (Sent::Feature, &packet(4)[..]));
            s.push_input(&path, reply(91, 0));
        });
        let out = poll_with(&[feat, input], &mut f);
        assert_eq!(out.len(), 1);
        assert_eq!(
            (out[0].percent, out[0].key.as_str()),
            (Some(91), "fantech:FA7C")
        );
    }

    #[test]
    fn invader_pro_scales_tens() {
        let feat = coll(VID, REDRAGON, 1, 0xFF02, 2, 17, 0, 17);
        let mut f = Fake::with(|s, _, _, _| s.push_input("", reply(7, 0)));
        let out = poll_with(&[feat], &mut f);
        assert_eq!((out[0].percent, out[0].coarse), (Some(70), true));
        assert_eq!(out[0].name, "Redragon Invader Pro");
    }
}

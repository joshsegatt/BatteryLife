//! Redragon wireless mice (M913 Impact Elite, M915RGB-WL) on Areson / CompX receivers 25A7:FA07 /
//! FA08. Read-only "GetPower": SET_FEATURE [08][04][00 ...][checksum = 0x55 - sum(0..15)] on the
//! FF02:0002 collection of interface 1; the reply arrives as an input report on FF01:0000 of the
//! same interface: [09][04][00]..[6] level 0..10 [7] charging .. whole frame sums to 0x55.
//! The level is in steps of 10% (coarse). FA7B / FA7C report on another scale and are fantech.rs'.
//!
//! Ported from BatteryHub (src/drivers/areson.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0, which cites
//! github.com/Qehbr/m913-ctl and github.com/Doomguy-BFG9000/redragon-m913-battery-tray. Each
//! attempt waits 500 ms for the reply (BatteryHub: 1.8 s) to keep the driver inside its budget.

use std::time::{Duration, Instant};

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::hid_dev::{BUDGET, Coll, Dev, HidIo, Io, colls, find, hex4, ms_left, products};
use crate::registry::Match;

const VID: u16 = 0x25A7;

pub static MATCHES: &[Match] = &[Match {
    vid: VID,
    pids: &[0xFA07, 0xFA08],
}];

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    poll_with(&colls(infos), &mut HidIo(api))
}

pub fn request() -> [u8; 17] {
    let mut q = [0u8; 17];
    q[0] = 0x08; // report id
    q[1] = 0x04; // GetPower
    q[16] = 0x55u8.wrapping_sub(q[..16].iter().fold(0u8, |a, b| a.wrapping_add(*b)));
    q
}

/// (percent, charging) from a GetPower reply.
pub fn parse(r: &[u8]) -> Option<(u8, bool)> {
    let sum = r.iter().fold(0u8, |a, b| a.wrapping_add(*b));
    (r.len() >= 17 && sum == 0x55 && r[..3] == [0x09, 0x04, 0x00] && r[6] <= 10)
        .then(|| (r[6] * 10, r[7] != 0))
}

fn query(
    cmd: &mut dyn Dev,
    reply: &mut dyn Dev,
    io: &mut dyn Io,
    end: Instant,
) -> Option<(u8, bool)> {
    for attempt in 0..2 {
        if attempt > 0 {
            io.sleep(200);
        }
        // A report queued before the request would be mistaken for the answer.
        reply.flush();
        if !cmd.set_feature(&request()) {
            return None; // receiver gone; retrying will not help
        }
        let left = ms_left(end).min(500);
        if left == 0 {
            return None;
        }
        if let Some(p) = reply.read(left).and_then(|r| parse(&r)) {
            return Some(p);
        }
    }
    None
}

fn poll_with(colls: &[Coll], io: &mut dyn Io) -> Vec<Device> {
    let end = Instant::now() + BUDGET;
    let mut out = Vec::new();
    'products: for pid in products(colls, VID) {
        if pid != 0xFA07 && pid != 0xFA08 {
            continue;
        }
        for c in find(colls, VID, pid, |c| {
            c.iface == 1 && c.page == 0xFF02 && c.usage == 2 && c.feat_len >= 17
        }) {
            for i in find(colls, VID, pid, |c| {
                c.iface == 1 && c.page == 0xFF01 && c.usage == 0 && c.in_len >= 17
            }) {
                if ms_left(end) == 0 {
                    return out;
                }
                let Some(mut cmd) = io.open(c) else { continue };
                let Some(mut reply) = io.open(i) else {
                    continue;
                };
                let Some((pct, charging)) = query(cmd.as_mut(), reply.as_mut(), io, end) else {
                    continue;
                };
                let mut s = Device::new(
                    format!("areson:{}:{}", hex4(VID), hex4(pid)),
                    "Redragon wireless mouse",
                    Kind::Mouse,
                    "2.4 GHz receiver",
                );
                s.percent = Some(pct);
                s.coarse = true;
                s.charging = charging;
                s.grace = Duration::from_secs(1800);
                out.push(s);
                continue 'products;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::hid_dev::fake::{Fake, Sent, coll};

    fn reply(level: u8, charging: u8) -> Vec<u8> {
        let mut r = vec![0u8; 17];
        r[..3].copy_from_slice(&[9, 4, 0]);
        r[6] = level;
        r[7] = charging;
        r[16] = 0x55u8.wrapping_sub(r[..16].iter().fold(0u8, |a, b| a.wrapping_add(*b)));
        r
    }

    #[test]
    fn level_8_reads_as_80_percent() {
        assert_eq!(parse(&reply(8, 0)), Some((80, false)));
        assert_eq!(parse(&reply(10, 1)), Some((100, true)));
        assert!(parse(&reply(11, 0)).is_none());
        let mut bad = reply(5, 0);
        bad[16] ^= 1;
        assert!(parse(&bad).is_none());
    }

    #[test]
    fn asks_on_feature_and_reads_the_input_collection() {
        let cmd = coll(VID, 0xFA07, 1, 0xFF02, 2, 0, 0, 17);
        let rep = coll(VID, 0xFA07, 1, 0xFF01, 0, 17, 0, 0);
        let reply_path = rep.path.to_string_lossy().to_string();
        let mut f = Fake::with(move |s, _, kind, q| {
            assert_eq!(kind, Sent::Feature);
            assert_eq!(q, request());
            s.push_input(&reply_path, reply(8, 1));
        });
        let out = poll_with(&[cmd, rep], &mut f);
        assert_eq!(out.len(), 1);
        assert_eq!(
            (out[0].percent, out[0].charging, out[0].coarse),
            (Some(80), true, true)
        );
    }
}

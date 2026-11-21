//! MCHOSE G3 V2 wireless mouse (3837: receiver 4245, USB cable 418C) on vendor collection
//! FF01:0010 (65-byte reports): output [00][55 30 A5 0B 2E 01 01 01 00 00 ...] -> input
//! [AA][30] .. [8] percent (1..100), [9] charging (1). 3837 is shared by other MCHOSE protocols,
//! so only the two G3 V2 ids are declared.
//!
//! Ported from BatteryHub (src/drivers/mchose_g3.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0, which cites
//! github.com/Jstdi001/MCHOSE-Monitor. The reply wait is 500 ms (BatteryHub: 1 s).

use std::time::{Duration, Instant};

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::hid::{self, BUDGET, Coll, Dev, Hid, find, left, read_until};
use crate::registry::Match;

const VID: u16 = 0x3837;
const WIRED: u16 = 0x418C;
const WIRELESS: u16 = 0x4245;

pub static MATCHES: &[Match] = &[Match {
    vid: VID,
    pids: &[WIRED, WIRELESS],
}];

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    hid::run(api, infos, |hid, colls| poll_with(colls, hid))
}

pub const REQUEST: [u8; 11] = [
    0x00, 0x55, 0x30, 0xA5, 0x0B, 0x2E, 0x01, 0x01, 0x01, 0x00, 0x00,
];

/// (percent, charging) from an AA 30 reply; None when the level is out of range.
pub fn parse(r: &[u8]) -> Option<(u8, bool)> {
    (r.len() >= 10 && r[0] == 0xAA && r[1] == 0x30 && (1..=100).contains(&r[8]))
        .then(|| (r[8], r[9] == 1))
}

fn query(d: &mut dyn Dev, end: Instant) -> Option<(u8, bool)> {
    d.flush();
    if !d.write(&REQUEST) {
        return None;
    }
    let end = end.min(Instant::now() + Duration::from_millis(500));
    let r = read_until(d, left(end), |r| {
        r.len() >= 10 && r[0] == 0xAA && r[1] == 0x30
    })?;
    parse(&r)
}

fn poll_with(colls: &[Coll], io: &mut dyn Hid) -> Vec<Device> {
    let end = Instant::now() + BUDGET;
    // Prefer wired over 2.4 GHz when both are connected at the same time.
    for pid in [WIRED, WIRELESS] {
        for c in find(colls, VID, pid, |c| {
            c.page == 0xFF01 && c.usage == 0x0010 && c.out_len >= 65 && c.in_len >= 65
        }) {
            if left(end).is_zero() {
                return Vec::new();
            }
            let Some((pct, charging)) = io.open(c).and_then(|mut d| query(d.as_mut(), end)) else {
                continue;
            };
            let mut s = Device::new(
                "mchose:g3v2",
                c.product_or("MCHOSE G3 V2"),
                Kind::Mouse,
                if pid == WIRED {
                    "USB cable"
                } else {
                    "2.4 GHz receiver"
                },
            );
            s.percent = Some(pct);
            s.charging = charging;
            s.grace = Duration::from_secs(1800);
            return vec![s];
        }
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::hid::fake::{self, Fake, iface_coll as coll};

    fn reply(pct: u8, charging: u8) -> Vec<u8> {
        let mut r = vec![0u8; 64];
        r[0] = 0xAA;
        r[1] = 0x30;
        r[8] = pct;
        r[9] = charging;
        r
    }

    #[test]
    fn reads_level_and_charging() {
        let mut f = Fake::with(|s, fc, op| {
            if let Some(q) = op.sent() {
                assert_eq!(q, REQUEST);
                s.push_input("", reply(58, 1));
            }
            fake::queued(s, fc, op)
        });
        let out = poll_with(&[coll(VID, WIRELESS, 1, 0xFF01, 0x10, 65, 65, 0)], &mut f);
        assert_eq!(out.len(), 1);
        assert_eq!((out[0].percent, out[0].charging), (Some(58), true));
        assert_eq!(out[0].via, "2.4 GHz receiver");
    }

    #[test]
    fn zero_level_is_rejected() {
        assert!(parse(&reply(0, 0)).is_none());
        assert!(parse(&reply(101, 0)).is_none());
    }
}

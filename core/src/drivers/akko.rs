//! Akko V9 Dash Pro (38EE:000C) and Akko Dash V9 Ultra (38EE:000F, "Akko 8K Receiver") over 2.4 GHz.
//!
//! Same 64-byte vendor channel on FF1C:0092 and the same read-only status query as the CHERRY
//! keyboards (see `cherry.rs`): reply [0] 4, [3] 1A, [8] percent (0..100), [9] non-zero while charging.
//!
//! Ported from BatteryHub (src/drivers/akko.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0.

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::cherry::ff1c_status;
use crate::drivers::hid::{self, Coll, Hid, RECEIVER, RECEIVER_GRACE};

pub const VID: u16 = 0x38EE;
pub const PIDS: &[u16] = &[0x000C, 0x000F];

const MODELS: [(u16, &str, &str); 2] = [
    (0x000C, "Akko V9 Dash Pro", "akko:38ee:000c"),
    (0x000F, "Akko Dash V9 Ultra", "akko:38ee:000f"),
];

fn is_status_reply(r: &[u8]) -> bool {
    r.len() >= 10 && r[0] == 0x04 && r[3] == 0x1A && r[8] <= 100
}

pub fn poll_with(colls: &[Coll], hid: &mut dyn Hid) -> Vec<Device> {
    let mut out = Vec::new();
    for (pid, name, key) in MODELS {
        for c in colls.iter().filter(|c| {
            c.is(VID, pid)
                && c.page == 0xFF1C
                && c.usage == 0x0092
                && c.in_len == 64
                && c.out_len == 64
        }) {
            let Some(mut port) = hid.open(c) else {
                continue;
            };
            let Some(r) = ff1c_status(port.as_mut(), is_status_reply) else {
                continue;
            };
            let mut d = Device::new(key, name, Kind::Mouse, RECEIVER);
            d.percent = Some(r[8]);
            d.charging = r[9] != 0;
            d.grace = RECEIVER_GRACE;
            out.push(d);
            break;
        }
    }
    out
}

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    hid::run(api, infos, |hid, colls| poll_with(colls, hid))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::cherry::ff1c_status_query;
    use crate::drivers::hid::fake::{coll, per_write, script};

    /// A flush drops queued replies, as on the device.
    fn reply(percent: u8, charging: u8) -> Vec<u8> {
        let mut r = vec![0u8; 64];
        r[0] = 0x04;
        r[3] = 0x1A;
        r[8] = percent;
        r[9] = charging;
        r
    }

    #[test]
    fn reads_both_receivers() {
        for (pid, key) in [(0x000C, "akko:38ee:000c"), (0x000F, "akko:38ee:000f")] {
            let s = per_write(vec![vec![reply(81, 1)]]);
            let out = poll_with(&[coll(VID, pid, 0xFF1C, 0x0092, 64)], &mut s.clone());
            assert_eq!(s.s().writes(), vec![ff1c_status_query()]);
            assert_eq!(out.len(), 1);
            assert_eq!(
                (out[0].key.as_str(), out[0].percent, out[0].charging),
                (key, Some(81), true)
            );
            assert_eq!(out[0].kind, Kind::Mouse);
        }
    }

    #[test]
    fn out_of_range_level_is_not_a_reply() {
        let s = per_write(vec![vec![reply(101, 0)], vec![reply(40, 0)]]);
        let out = poll_with(&[coll(VID, 0x000C, 0xFF1C, 0x0092, 64)], &mut s.clone());
        assert_eq!(out[0].percent, Some(40));
        let s = script();
        assert!(poll_with(&[coll(VID, 0x000C, 0xFF1C, 0x0092, 9)], &mut s.clone()).is_empty());
        assert!(s.s().opened.is_empty());
    }
}

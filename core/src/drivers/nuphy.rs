//! NuPhy WH80 over its USB cable (19F5:A011).
//!
//! NuPhyIO SDK getBatteryState(0): GetFunc (05), offset 0020, one byte; only this GET is sent.
//! Unnumbered 64-byte reports on usage 0001:0000, interface 1:
//!   55 05 00 21 01 20 00 00 ... -> AA 05 00 checksum 01 20 00 00 value ...
//! Checksum = low byte of the sum of bytes 4..63. Value: bit 7 charging, bits 0..6 percent (the WH80
//! reports in 10 % steps).
//!
//! Ported from BatteryHub (src/drivers/nuphy.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0.

use std::time::Duration;

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::hid::{self, CABLE, Coll, Hid, read_until, sum};

pub const VID: u16 = 0x19F5;
pub const PIDS: &[u16] = &[0xA011];

const REQUEST: [u8; 9] = [0, 0x55, 0x05, 0, 0x21, 1, 0x20, 0, 0];

/// (percent, charging) from a battery reply.
pub fn parse(r: &[u8]) -> Option<(u8, bool)> {
    if r.len() != 64 || r[..3] != [0xAA, 0x05, 0] || r[4..8] != [1, 0x20, 0, 0] {
        return None;
    }
    if sum(&r[4..]) != r[3] || r[8] & 0x7F > 100 {
        return None;
    }
    Some((r[8] & 0x7F, r[8] & 0x80 != 0))
}

pub fn poll_with(colls: &[Coll], hid: &mut dyn Hid) -> Vec<Device> {
    for c in colls.iter().filter(|c| {
        c.is(VID, 0xA011)
            && c.iface == 1
            && c.page == 1
            && c.usage == 0
            && c.in_len == 65
            && c.out_len == 65
    }) {
        let Some(mut port) = hid.open(c) else {
            continue;
        };
        port.flush();
        if !port.write(&REQUEST) {
            continue;
        }
        let Some(r) = read_until(port.as_mut(), Duration::from_millis(400), |r| {
            parse(r).is_some()
        }) else {
            continue;
        };
        let (percent, charging) = parse(&r).unwrap_or_default();
        let mut d = Device::new("nuphy:WH80", "NuPhy WH80", Kind::Keyboard, CABLE);
        d.percent = Some(percent);
        d.charging = charging;
        d.coarse = true; // 10 % steps
        return vec![d];
    }
    Vec::new()
}

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    hid::run(api, infos, |hid, colls| poll_with(colls, hid))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::hid::fake::{Fake, coll, per_write};

    fn collection() -> Coll {
        let mut c = coll(VID, 0xA011, 1, 0, 65);
        c.iface = 1;
        c
    }

    fn reply(battery: u8) -> Vec<u8> {
        let mut r = vec![0u8; 64];
        r[0] = 0xAA;
        r[1] = 5;
        r[4] = 1;
        r[5] = 0x20;
        r[8] = battery;
        r[3] = 1u8.wrapping_add(0x20).wrapping_add(battery);
        r
    }

    fn run(replies: Vec<Vec<u8>>, c: Coll) -> (Vec<Device>, Fake) {
        let s = per_write(vec![replies]);
        (poll_with(&[c], &mut s.clone()), s)
    }

    #[test]
    fn levels_and_charging_bit() {
        for level in [0u8, 10, 50, 100, 0xE4] {
            let (out, s) = run(vec![reply(level)], collection());
            assert_eq!(out.len(), 1);
            assert_eq!(out[0].percent, Some(level & 0x7F));
            assert_eq!(out[0].charging, level & 0x80 != 0);
            assert_eq!(out[0].kind, Kind::Keyboard);
            assert_eq!(s.s().writes(), vec![REQUEST.to_vec()]);
        }
    }

    #[test]
    fn echoes_damaged_and_short_replies_are_rejected() {
        for index in 0..8 {
            let mut bad = reply(50);
            bad[index] ^= 1;
            assert!(run(vec![bad.clone()], collection()).0.is_empty());
            assert_eq!(run(vec![bad, reply(50)], collection()).0.len(), 1);
        }
        let mut short = reply(50);
        short.truncate(8);
        assert!(run(vec![short], collection()).0.is_empty());
        assert!(run(vec![reply(101)], collection()).0.is_empty());
        assert!(run(vec![], collection()).0.is_empty());
    }

    #[test]
    fn only_the_exact_collection_is_opened() {
        for field in 0..6 {
            let mut c = collection();
            match field {
                0 => c.vid += 1,
                1 => c.pid += 1,
                2 => c.iface += 1,
                3 => c.usage += 1,
                4 => c.in_len -= 1,
                _ => c.out_len -= 1,
            }
            let (out, s) = run(vec![reply(50)], c);
            assert!(out.is_empty() && s.s().opened.is_empty());
        }
    }
}

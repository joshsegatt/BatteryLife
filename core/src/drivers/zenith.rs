//! Zenith Pro mouse (260D:1114 receiver, 260D:1143 cable, "Segment" software): one 17-byte battery
//! request [08][04][0 x14][49] on the FF02:0002 collection of interface 1; the reply carries
//! [6] percent, [7] charging, [8..9] millivolts (big-endian), and its bytes sum to 0x55.
//!
//! Ported from BatteryHub (src/drivers/zenith.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0. The reply wait is 500 ms
//! (BatteryHub: 1 s) to keep the driver inside its time budget.

use std::time::{Duration, Instant};

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::hid_dev::{BUDGET, Coll, HidIo, Io, colls, find, hex4, ms_left};
use crate::registry::Match;

const VID: u16 = 0x260D;

pub static MATCHES: &[Match] = &[Match {
    vid: VID,
    pids: &[0x1114, 0x1143],
}];

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    poll_with(&colls(infos), &mut HidIo(api))
}

/// Battery request; byte 16 makes the 17 bytes sum to 0x55.
pub const REQUEST: [u8; 17] = [8, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x49];

/// (percent, charging, millivolts) from a battery reply.
pub fn parse(r: &[u8]) -> Option<(u8, bool, u16)> {
    let sum = r.iter().fold(0u8, |a, b| a.wrapping_add(*b));
    (r.len() == 17 && r[0] == 8 && r[1] == 4 && r[6] <= 100 && sum == 0x55)
        .then(|| (r[6], r[7] != 0, u16::from_be_bytes([r[8], r[9]])))
}

fn poll_pid(colls: &[Coll], io: &mut dyn Io, pid: u16, via: &str, end: Instant) -> Option<Device> {
    let found = find(colls, VID, pid, |c| {
        c.iface == 1 && c.page == 0xFF02 && c.usage == 2 && c.in_len == 17 && c.out_len == 17
    });
    if found.len() != 1 || ms_left(end) == 0 {
        return None;
    }
    let mut d = io.open(found[0])?;
    d.flush();
    if !d.write(&REQUEST) {
        return None;
    }
    let end = end.min(Instant::now() + Duration::from_millis(500));
    for _ in 0..32 {
        let left = ms_left(end);
        if left == 0 {
            return None;
        }
        let r = d.read(left)?;
        if let Some((pct, charging, mv)) = parse(&r) {
            let mut s = Device::new(
                format!("zenith:{}:{}", hex4(VID), hex4(pid)),
                "Zenith Pro",
                Kind::Mouse,
                via,
            );
            s.percent = Some(pct);
            s.charging = charging;
            s.millivolts = Some(mv).filter(|v| *v != 0);
            // The dongle stops answering while the mouse sleeps: keep the last value.
            s.grace = Duration::from_secs(1800);
            return Some(s);
        }
    }
    None
}

fn poll_with(colls: &[Coll], io: &mut dyn Io) -> Vec<Device> {
    let end = Instant::now() + BUDGET;
    [(0x1114, "2.4 GHz receiver"), (0x1143, "USB cable")]
        .into_iter()
        .filter_map(|(pid, via)| poll_pid(colls, io, pid, via, end))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::hid_dev::fake::{Fake, coll};

    #[test]
    fn request_checksum_sums_to_0x55() {
        let sum = REQUEST.iter().fold(0u8, |a, b| a.wrapping_add(*b));
        assert_eq!(sum, 0x55);
    }

    fn reply(pct: u8, charging: u8) -> Vec<u8> {
        let mut r = vec![0u8; 17];
        r[0] = 8;
        r[1] = 4;
        r[6] = pct;
        r[7] = charging;
        r[8] = 0x0F;
        r[9] = 0xA0;
        let sum = r[..16].iter().fold(0u8, |a, b| a.wrapping_add(*b));
        r[16] = 0x55u8.wrapping_sub(sum);
        r
    }

    #[test]
    fn reads_percent_charging_and_voltage() {
        let mut f = Fake::with(|s, _, _, q| {
            assert_eq!(q, REQUEST);
            s.push_input("", vec![0x01, 0x02]); // other traffic is skipped
            s.push_input("", reply(77, 1));
        });
        let out = poll_with(&[coll(VID, 0x1114, 1, 0xFF02, 2, 17, 17, 0)], &mut f);
        assert_eq!(out.len(), 1);
        assert_eq!(
            (out[0].percent, out[0].charging, out[0].millivolts),
            (Some(77), true, Some(4000))
        );
        assert_eq!(out[0].key, "zenith:260D:1114");
    }

    #[test]
    fn bad_checksum_is_ignored() {
        let mut bad = reply(50, 0);
        bad[16] ^= 1;
        assert!(parse(&bad).is_none());
        assert!(parse(&reply(101, 0)).is_none());
    }
}

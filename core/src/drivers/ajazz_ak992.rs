//! AJAZZ AK992 on its Telink 2.4 GHz receiver (320F:50C3).
//!
//! EVision's read-only battery query on interface 1, FF1C:0092 (64-byte reports, no feature report):
//! numbered report 04 [04][request id LE][1A]; the reply echoes the request id and command 1A, then
//! reports the percentage and charging state at bytes 8 and 9 (report id included).
//!
//! Ported from BatteryHub (src/drivers/ajazz_ak992.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0, which credits
//! HeyOkay/HaloBattery (docs/protocols.md).

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::atk_keyboard::{Reply, evision_reply};
use crate::drivers::hid::{self, Coll, Dev, Hid, RECEIVER, RECEIVER_GRACE};

pub const VID: u16 = 0x320F;
const PID: u16 = 0x50C3;
pub const PIDS: &[u16] = &[PID];

fn read_battery(port: &mut dyn Dev, seq: u16) -> Option<(u8, bool)> {
    let [lo, hi] = seq.to_le_bytes();
    port.flush();
    if !port.write(&[0x04, lo, hi, 0x1A]) {
        return None;
    }
    let end = Instant::now() + Duration::from_millis(600);
    loop {
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return None;
        }
        // An unusable level is skipped like any other report, as BatteryHub does here.
        if let Reply::Level(p, charging) = evision_reply(&port.read(left)?, seq) {
            return Some((p, charging));
        }
    }
}

pub fn poll_with(colls: &[Coll], hid: &mut dyn Hid, seq: u16) -> Vec<Device> {
    for c in colls.iter().filter(|c| {
        c.is(VID, PID)
            && c.iface == 1
            && c.page == 0xFF1C
            && c.usage == 0x0092
            && c.in_len == 64
            && c.out_len == 64
            && c.feat_len == 0
    }) {
        let Some(mut port) = hid.open(c) else {
            continue;
        };
        let Some((percent, charging)) = read_battery(port.as_mut(), seq) else {
            continue;
        };
        let mut d = Device::new(
            "ajazz-ak992:320F:50C3",
            "AJAZZ AK992",
            Kind::Keyboard,
            RECEIVER,
        );
        d.percent = Some(percent);
        d.charging = charging;
        d.grace = RECEIVER_GRACE;
        return vec![d];
    }
    Vec::new()
}

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    // BatteryHub uses the low 16 bits of the tick count as the request id.
    let seq = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(1, |d| d.as_millis() as u16);
    hid::run(api, infos, |hid, colls| poll_with(colls, hid, seq))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::hid::fake::{coll, per_write, script};

    /// A flush drops queued replies, as on the device.
    fn receiver() -> Coll {
        let mut c = coll(VID, PID, 0xFF1C, 0x0092, 64);
        c.iface = 1;
        c
    }

    fn reply(seq: u16, percent: u8, state: u8) -> Vec<u8> {
        let [lo, hi] = seq.to_le_bytes();
        let mut r = vec![0u8; 64];
        r[..4].copy_from_slice(&[0x04, lo, hi, 0x1A]);
        r[8] = percent;
        r[9] = state;
        r
    }

    #[test]
    fn reads_level_and_skips_other_reports() {
        let s = per_write(vec![vec![
            reply(0x1233, 50, 0),
            reply(0x1234, 101, 0),
            reply(0x1234, 86, 1),
        ]]);
        let out = poll_with(&[receiver()], &mut s.clone(), 0x1234);
        assert_eq!(s.s().writes(), vec![vec![0x04, 0x34, 0x12, 0x1A]]);
        let d = &out[0];
        assert_eq!(
            (d.percent, d.charging, d.kind),
            (Some(86), true, Kind::Keyboard)
        );
        assert_eq!(d.key, "ajazz-ak992:320F:50C3");
    }

    #[test]
    fn silent_receiver_and_other_collections() {
        assert!(poll_with(&[receiver()], &mut script(), 7).is_empty());
        let mut with_feature = receiver();
        with_feature.feat_len = 64;
        let mut other_iface = receiver();
        other_iface.iface = 0;
        let s = script();
        assert!(poll_with(&[with_feature, other_iface], &mut s.clone(), 7).is_empty());
        assert!(s.s().opened.is_empty());
    }
}

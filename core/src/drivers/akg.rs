//! AKG N9 Hybrid on its USB-C dongle (0ECB:20D9). The dongle is an Airoha part speaking RACE on the
//! vendor collection FF13:0001 (interface 3):
//!   request: 62-byte output report 06: [06][07][80][05 5A 03 00 D6 0C 00] = battery (id 0x0CD6)
//!   answer:  input report 07 via GetInputReport: [07][valid length][80] + recent RACE frames; the
//!            battery reply is [05 5D 05 00 D6 0C][status 00][agent][percent].
//! Only bytes inside the valid length are read: with the headset off an old battery frame can still
//! sit in the leftover tail. Only this single read-only query is sent. No charging flag is known.
//!
//! Ported from BatteryHub (src/drivers/akg.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0.

use std::time::{Duration, Instant};

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::hid::{self, Coll, Dev, Hid, find};

pub const VID: u16 = 0x0ECB;
pub const PID: u16 = 0x20D9;

/// The newest battery reply in a report 07 buffer.
pub fn find_level(b: &[u8]) -> Option<u8> {
    if b.len() < 4 {
        return None;
    }
    let end = b.len().min(3 + usize::from(b[1]));
    let mut level = None;
    let mut i = 3;
    while i + 8 < end {
        if b[i..i + 7] == [0x05, 0x5D, 0x05, 0x00, 0xD6, 0x0C, 0x00] && b[i + 8] <= 100 {
            level = Some(b[i + 8]);
        }
        i += 1;
    }
    level
}

pub fn read_level(io: &mut dyn Dev, out_len: usize) -> Option<u8> {
    let mut req = vec![0u8; out_len.max(10)];
    req[..10].copy_from_slice(&[0x06, 0x07, 0x80, 0x05, 0x5A, 0x03, 0x00, 0xD6, 0x0C, 0x00]);
    if !io.write(&req) {
        return None;
    }
    // The reply shows up within ~100 ms while the headset is on.
    let end = Instant::now() + hid::MAX_WAIT;
    while Instant::now() < end {
        io.sleep(Duration::from_millis(15));
        if let Some(l) = io.get_input(0x07).and_then(|r| find_level(&r)) {
            return Some(l);
        }
    }
    None
}

pub fn poll_colls(colls: &[Coll], hid: &mut dyn Hid) -> Vec<Device> {
    for c in find(colls, VID, PID, |c| {
        c.iface == 3 && c.page == 0xFF13 && c.usage == 0x0001 && c.in_len == 62 && c.out_len == 62
    }) {
        let Some(level) = hid
            .open(c)
            .and_then(|mut io| read_level(io.as_mut(), c.out_len.into()))
        else {
            continue;
        };
        let mut d = Device::new("akg:20D9", "AKG N9 Hybrid", Kind::Headset, "USB dongle");
        d.percent = Some(level);
        return vec![d];
    }
    Vec::new()
}

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    hid::run(api, infos, |hid, colls| poll_colls(colls, hid))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::hid::fake::Fake;

    /// 05 5D 05 00 D6 0C 00 00 32 = 50 %, as checked against the AKG Headphones app.
    fn report(valid: u8, tail: &[u8]) -> Vec<u8> {
        let mut r = vec![0x07, valid, 0x80];
        r.extend_from_slice(tail);
        r.resize(62, 0);
        r
    }

    #[test]
    fn battery_frame_inside_the_valid_part_only() {
        let frame = [0x05, 0x5D, 0x05, 0x00, 0xD6, 0x0C, 0x00, 0x00, 0x32];
        assert_eq!(find_level(&report(9, &frame)), Some(50));
        assert_eq!(find_level(&report(8, &frame)), None);
        // An acknowledgement and a notification before it.
        let mut tail = vec![0x05, 0x5B, 0x03, 0x00, 0xD6, 0x0C, 0x00];
        tail.extend_from_slice(&frame);
        assert_eq!(find_level(&report(tail.len() as u8, &tail)), Some(50));
    }

    #[test]
    fn dongle_with_headset_on() {
        let mut c = Coll::new(VID, PID, 0xFF13, 1, 62, 62);
        c.iface = 3;
        let f = Fake::new();
        let frame = [0x05, 0x5D, 0x05, 0x00, 0xD6, 0x0C, 0x00, 0x00, 0x32];
        f.s().input_reports.insert(
            String::new(),
            vec![report(0, &[]), report(9, &frame)].into(),
        );
        let out = poll_colls(&[c], &mut f.clone());
        assert_eq!(out[0].key, "akg:20D9");
        assert_eq!(out[0].percent, Some(50));
        assert_eq!(f.s().writes()[0].len(), 62);
        assert_eq!(
            f.s().writes()[0][..10],
            [6, 7, 0x80, 5, 0x5A, 3, 0, 0xD6, 0x0C, 0]
        );
    }
}

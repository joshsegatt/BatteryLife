//! DARKRIM SHADOW headset receiver (3654:6901).
//!
//! Interface 3, FF02:0002, 65-byte reports (report id 0 + 64 bytes). Only GET 0010 (request 01) with
//! the battery TLV selector 01 00 is sent:
//!   FE seq cmd LE type index count length LE payload CRC-16/CCITT (BE)
//! The reply carries TLV 01: bit 7 charging, bits 0..6 percent.
//!
//! Ported from BatteryHub (src/drivers/darkrim.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0 (from DARKRIM 0.1.26, statically
//! inspected there).

use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::hid::{self, Coll, Hid, RECEIVER, RECEIVER_GRACE, read_until};

pub const VID: u16 = 0x3654;
const PID: u16 = 0x6901;
pub const PIDS: &[u16] = &[PID];

/// CRC-16/CCITT-FALSE (poly 1021, init FFFF).
pub fn crc(bytes: &[u8]) -> u16 {
    let mut crc = 0xFFFFu16;
    for b in bytes {
        crc ^= u16::from(*b) << 8;
        for _ in 0..8 {
            crc = (crc << 1) ^ if crc & 0x8000 != 0 { 0x1021 } else { 0 };
        }
    }
    crc
}

/// The battery request with report id 0 in front.
pub fn battery_request(seq: u8) -> Vec<u8> {
    let mut r = vec![0xFE, seq, 0x10, 0, 1, 0, 1, 2, 0, 1, 0];
    let c = crc(&r);
    r.extend(c.to_be_bytes());
    r.insert(0, 0);
    r
}

/// (percent, charging) from a reply to request `seq`.
pub fn battery_reply(r: &[u8], seq: u8) -> Option<(u8, bool)> {
    if r.len() < 11
        || r[0] != 0xFE
        || r[1] != seq
        || r[2] != 0x10
        || r[3] != 0
        || r[4] != 2
        || r[5] != 0
        || r[6] > 1
    {
        return None;
    }
    let len = usize::from(u16::from_le_bytes([r[7], r[8]]));
    if len > 53 || len + 11 > r.len() {
        return None;
    }
    let end = 9 + len;
    if crc(&r[..end]) != u16::from_be_bytes([r[end], r[end + 1]]) {
        return None;
    }
    let mut value = None;
    let mut i = 9;
    while i < end {
        if end - i < 2 {
            return None;
        }
        let (kind, size) = (r[i], usize::from(r[i + 1]));
        i += 2;
        if size > end - i {
            return None;
        }
        if kind == 1 {
            if size != 1 || value.is_some() || r[i] & 0x7F > 100 {
                return None;
            }
            value = Some(r[i]);
        }
        i += size;
    }
    value.map(|v| (v & 0x7F, v & 0x80 != 0))
}

pub fn poll_with(colls: &[Coll], hid: &mut dyn Hid, seq: u8) -> Vec<Device> {
    for c in colls.iter().filter(|c| {
        c.is(VID, PID)
            && c.iface == 3
            && c.page == 0xFF02
            && c.usage == 2
            && c.in_len == 65
            && c.out_len == 65
    }) {
        let Some(mut port) = hid.open(c) else {
            continue;
        };
        port.flush();
        if !port.write(&battery_request(seq)) {
            continue;
        }
        // The receiver first sends several startup notifications; the reply was measured at ~406 ms.
        let Some(r) = read_until(port.as_mut(), Duration::from_millis(800), |r| {
            battery_reply(r, seq).is_some()
        }) else {
            continue;
        };
        let (percent, charging) = battery_reply(&r, seq).unwrap_or_default();
        let mut d = Device::new("darkrim:SHADOW", "DARKRIM SHADOW", Kind::Headset, RECEIVER);
        d.percent = Some(percent);
        d.charging = charging;
        d.grace = RECEIVER_GRACE;
        return vec![d];
    }
    Vec::new()
}

static SEQ: AtomicU8 = AtomicU8::new(0);

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    let seq = SEQ.load(Ordering::Relaxed) % 15 + 1;
    SEQ.store(seq, Ordering::Relaxed);
    hid::run(api, infos, |hid, colls| poll_with(colls, hid, seq))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::hid::fake::{Fake, coll, per_write};

    fn seal(r: &mut [u8]) {
        let end = 9 + usize::from(r[7]);
        let c = crc(&r[..end]);
        r[end..end + 2].copy_from_slice(&c.to_be_bytes());
    }

    fn reply(seq: u8, value: u8) -> Vec<u8> {
        let mut r = vec![0u8; 64];
        r[..12].copy_from_slice(&[0xFE, seq, 0x10, 0, 2, 0, 0, 3, 0, 1, 1, value]);
        seal(&mut r);
        r
    }

    fn receiver() -> Coll {
        let mut c = coll(VID, PID, 0xFF02, 2, 65);
        c.iface = 3;
        c
    }

    fn run(seq: u8, replies: Vec<Vec<u8>>, c: Coll) -> (Vec<Device>, Fake) {
        let s = per_write(vec![replies]);
        (poll_with(&[c], &mut s.clone(), seq), s)
    }

    #[test]
    fn captured_reply_and_request() {
        // Captured reply FE 03 10 00 02 00 00 03 00 01 01 28 96 0D.
        let r = reply(3, 40);
        assert_eq!(r[12..14], [0x96, 0x0D]);
        let (out, s) = run(3, vec![r], receiver());
        let w = &s.s().writes()[0];
        assert_eq!((w.len(), w[0], w[1], w[2]), (14, 0, 0xFE, 3));
        assert_eq!(w[3..12], [0x10, 0, 1, 0, 1, 2, 0, 1, 0]);
        assert_eq!(crc(&w[1..12]), u16::from_be_bytes([w[12], w[13]]));
        assert_eq!(
            (out[0].percent, out[0].charging, out[0].kind),
            (Some(40), false, Kind::Headset)
        );
    }

    #[test]
    fn charging_bit_and_range() {
        for v in [0u8, 100, 0x80, 0xE4] {
            let (out, _) = run(5, vec![reply(5, v)], receiver());
            assert_eq!(
                (out[0].percent, out[0].charging),
                (Some(v & 0x7F), v & 0x80 != 0)
            );
        }
        assert!(run(5, vec![reply(5, 101)], receiver()).0.is_empty());
    }

    #[test]
    fn rejects_damaged_frames_then_takes_a_good_one() {
        for index in [0usize, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 12, 13] {
            let mut bad = reply(7, 40);
            bad[index] ^= if index == 6 { 2 } else { 1 };
            if index < 7 || index == 9 || index == 10 {
                seal(&mut bad);
            }
            assert!(
                run(7, vec![bad.clone()], receiver()).0.is_empty(),
                "byte {index}"
            );
            assert_eq!(run(7, vec![bad, reply(7, 40)], receiver()).0.len(), 1);
        }
        assert!(run(7, vec![vec![0; 8]], receiver()).0.is_empty());
        assert!(run(7, vec![], receiver()).0.is_empty());
    }

    #[test]
    fn strict_matching() {
        for field in 0..7 {
            let mut c = receiver();
            match field {
                0 => c.vid += 1,
                1 => c.pid += 1,
                2 => c.iface += 1,
                3 => c.page += 1,
                4 => c.usage += 1,
                5 => c.in_len -= 1,
                _ => c.out_len -= 1,
            }
            let (out, s) = run(1, vec![reply(1, 40)], c);
            assert!(out.is_empty() && s.s().opened.is_empty());
        }
    }
}

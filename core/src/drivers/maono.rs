//! MAONO PD200W microphone on its 2.4 GHz receiver (352F:040E, interface 3, collection 000C:0001,
//! 64-byte input / output reports, no feature reports). Register GET (0x04) of battery register
//! 0x0042: request C4 09 00 00 04 42 00 <checksum16 LE>; the reply holds little-endian (register,
//! value) pairs. Only GET is sent, never SET (0x03). Charging is not implemented.
//!
//! Ported from BatteryHub (src/drivers/maono.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0, which credits
//! iamsh4/maomao and shahriyardx/maono.

use std::time::{Duration, Instant};

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::hid_io::{self, Coll, Io, Open, find, stable_key};

pub const VID: u16 = 0x352F;
pub const PID: u16 = 0x040E;
const REPORT_ID: u8 = 0xC4;
const GET: u8 = 0x04;
const BATTERY_REGISTER: u16 = 0x0042;
const REPORT_LEN: u16 = 64;

fn le16(d: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([d[at], d[at + 1]])
}

fn checksum(d: &[u8]) -> u16 {
    let sum = d.iter().fold(0u32, |s, v| s + u32::from(*v));
    (0x10000 - (sum & 0xFFFF)) as u16
}

pub fn request() -> [u8; 64] {
    let mut r = [0u8; 64];
    r[0] = REPORT_ID;
    r[1] = 9;
    r[4] = GET;
    r[5..7].copy_from_slice(&BATTERY_REGISTER.to_le_bytes());
    let c = checksum(&r[..7]);
    r[7..9].copy_from_slice(&c.to_le_bytes());
    r
}

pub fn parse_battery(r: &[u8]) -> Option<u8> {
    if r.len() < 11 || r[0] != REPORT_ID {
        return None;
    }
    let length = usize::from(r[1]);
    if length < 11 || length > r.len() || r[2] != 0 || r[3] != 0 || r[4] != GET {
        return None;
    }
    let body_end = length - 2;
    if (body_end - 5) % 4 != 0 || le16(r, body_end) != checksum(&r[..body_end]) {
        return None;
    }
    let at = (5..body_end)
        .step_by(4)
        .find(|&o| le16(r, o) == BATTERY_REGISTER)?;
    let value = le16(r, at + 2);
    (value <= 100).then_some(value as u8)
}

fn read_battery(io: &mut dyn Io) -> Option<u8> {
    // One deadline shared by sending and receiving; unrelated reports do not restart it.
    let end = Instant::now() + Duration::from_millis(hid_io::MAX_REPLY_MS);
    io.flush();
    if !io.write(&request()) {
        return None;
    }
    for _ in 0..128 {
        let left = end.saturating_duration_since(Instant::now()).as_millis() as u64;
        if left == 0 {
            break;
        }
        let r = io.read(left)?;
        if Instant::now() >= end {
            break;
        }
        if let Some(p) = parse_battery(&r) {
            return Some(p);
        }
    }
    None
}

pub fn poll_colls(colls: &[Coll], open: &mut Open<'_>) -> Vec<Device> {
    let mut out = Vec::new();
    for c in find(colls, VID, PID, |c| {
        c.iface == 3
            && c.page == 0x000C
            && c.usage == 0x0001
            && c.in_len == REPORT_LEN
            && c.out_len == REPORT_LEN
            && c.feat_len == 0
    }) {
        let Some(pct) = open(c).and_then(|mut io| read_battery(io.as_mut())) else {
            continue;
        };
        // A microphone: BatteryLife has no microphone kind, so it is listed as "other".
        let key = stable_key(&out, "maono:pd200w");
        let mut d = Device::new(key, "MAONO PD200W", Kind::Other, "2.4 GHz receiver");
        d.percent = Some(pct);
        out.push(d);
        break;
    }
    out
}

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    hid_io::run(api, infos, poll_colls)
}

#[cfg(test)]
mod tests {
    // tests/maono_driver_test.cpp
    use super::*;
    use crate::drivers::hid_io::fake::Fake;
    use std::cell::RefCell;

    fn fix_checksum(r: &mut [u8]) {
        let end = usize::from(r[1]) - 2;
        let c = checksum(&r[..end]);
        r[end..end + 2].copy_from_slice(&c.to_le_bytes());
    }
    fn battery(pct: u16, reg: u16) -> Vec<u8> {
        let mut r = vec![0u8; 64];
        r[0] = 0xC4;
        r[1] = 11;
        r[4] = 0x04;
        r[5..7].copy_from_slice(&reg.to_le_bytes());
        r[7..9].copy_from_slice(&pct.to_le_bytes());
        fix_checksum(&mut r);
        r
    }
    fn collection() -> Coll {
        let mut c = Coll::new(VID, PID, 0x000C, 0x0001, 64, 64);
        c.iface = 3;
        c
    }
    fn run(c: Coll, replies: Vec<Vec<u8>>) -> (Vec<Device>, Fake, usize) {
        let f = Fake::new();
        f.queue(replies);
        let opened = RefCell::new(Vec::new());
        let out = poll_colls(&[c], &mut *f.opener(&opened));
        let n = opened.borrow().len();
        (out, f, n)
    }

    #[test]
    fn request_is_the_battery_get() {
        assert_eq!(
            request()[..9],
            [0xC4, 0x09, 0, 0, 0x04, 0x42, 0, 0xED, 0xFE]
        );
    }

    #[test]
    fn levels_including_real_zero() {
        for pct in [0, 1, 15, 64, 100] {
            let (out, f, opens) = run(collection(), vec![battery(pct, 0x0042)]);
            let s = f.s();
            assert_eq!((opens, s.flushes, s.writes.len()), (1, 1, 1));
            drop(s);
            let d = &out[0];
            assert_eq!(d.percent, Some(pct as u8));
            assert_eq!(
                (d.key.as_str(), d.name.as_str()),
                ("maono:pd200w", "MAONO PD200W")
            );
            assert_eq!((d.via.as_str(), d.charging), ("2.4 GHz receiver", false));
            assert_eq!(d.grace, Duration::ZERO);
            assert_eq!(f.s().writes[0], request().to_vec());
        }
    }

    #[test]
    fn mic_off_and_write_failure_list_nothing() {
        assert!(run(collection(), vec![]).0.is_empty());
        let f = Fake::new();
        f.s().write_ok = false;
        let opened = RefCell::new(Vec::new());
        assert!(poll_colls(&[collection()], &mut *f.opener(&opened)).is_empty());
        assert_eq!(f.s().reads, 0);
    }

    #[test]
    fn strict_identity_and_collection_matching() {
        for field in 0..8 {
            let mut c = collection();
            match field {
                0 => c.vid = 0x3530,
                1 => c.pid = 0x0411,
                2 => c.iface = 2,
                3 => c.page = 0xFF00,
                4 => c.usage = 2,
                5 => c.in_len = 63,
                6 => c.out_len = 63,
                _ => c.feat_len = 64,
            }
            let (out, _, opens) = run(c, vec![battery(64, 0x0042)]);
            assert!(out.is_empty() && opens == 0);
        }
    }

    #[test]
    fn malformed_and_unrelated_replies_are_skipped() {
        let mut invalid = vec![vec![0u8; 10]];
        for field in 0..9 {
            let mut r = battery(64, 0x0042);
            match field {
                0 => r[0] = 0xC5,
                1 => r[1] = 65,
                2 => r[1] = 10,
                3 => {
                    r[2] = 1;
                    fix_checksum(&mut r);
                }
                4 => {
                    r[3] = 1;
                    fix_checksum(&mut r);
                }
                5 => {
                    r[4] = 3;
                    fix_checksum(&mut r);
                }
                6 => r[9] ^= 1,
                7 => r = battery(101, 0x0042),
                _ => r = battery(64, 0x0044),
            }
            invalid.push(r);
        }
        assert!(run(collection(), invalid.clone()).0.is_empty());
        invalid.push(battery(42, 0x0042));
        assert_eq!(run(collection(), invalid).0[0].percent, Some(42));
    }

    #[test]
    fn battery_register_may_follow_other_pairs() {
        let mut multi = battery(0, 0x0044);
        multi[1] = 15;
        multi[9..13].copy_from_slice(&[0x42, 0, 37, 0]);
        fix_checksum(&mut multi);
        assert_eq!(run(collection(), vec![multi]).0[0].percent, Some(37));
    }
}

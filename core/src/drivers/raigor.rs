//! Fantech Raigor III, Raigor II WG10 and other 2.4 GHz mice on the reused Sharkoon 1EA7:0066 /
//! 0064 dongle. Vendor collection FFB5:0001, report id B5, 8-byte reports:
//!   output [B5 05 00 ...] -> the dongle streams its 256-byte status page as 64 frames
//!   [B5 05 addr 04 d0 d1 d2 d3]; chunk 94 holds [id3, state, percent].
//!   The dongle also pushes [B5 67 id[4] state level] on wake / re-link / heartbeat.
//!
//! The page holds the last radio report, so a sleeping mouse keeps its last value. No charging.
//!
//! Ported from BatteryHub (src/drivers/raigor.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0, which cites
//! github.com/tuttizpro218/t20-battery (docs/PROTOCOL.md). The page stream keeps BatteryHub's
//! 1.2 s window (64 frames), read in steps of at most 200 ms.

use std::time::{Duration, Instant};

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::hid_dev::{
    BUDGET, Coll, Dev, HidIo, Io, colls, find, hex4, kind_of, ms_left, products,
};
use crate::registry::Match;

const VID: u16 = 0x1EA7;

pub static MATCHES: &[Match] = &[Match {
    vid: VID,
    pids: &[0x0066, 0x0064],
}];

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    poll_with(&colls(infos), &mut HidIo(api))
}

pub const REQUEST: [u8; 8] = [0xB5, 0x05, 0, 0, 0, 0, 0, 0];

/// Percent from one 8-byte frame: status page chunk 94, or a wake / heartbeat announcement.
pub fn parse(r: &[u8]) -> Option<u8> {
    if r.len() != 8 || r[0] != 0xB5 {
        return None;
    }
    if r[1] == 0x05 && r[2] == 0x94 && r[3] == 0x04 && r[6] <= 100 {
        return Some(r[6]);
    }
    (r[1] == 0x67 && r[7] <= 100).then_some(r[7])
}

fn query(d: &mut dyn Dev, end: Instant) -> Option<u8> {
    d.flush();
    if !d.write(&REQUEST) {
        return None;
    }
    let end = end.min(Instant::now() + Duration::from_millis(1200));
    loop {
        let left = ms_left(end);
        if left == 0 {
            return None;
        }
        if let Some(p) = parse(&d.read(left.min(200))?) {
            return Some(p);
        }
    }
}

fn poll_with(colls: &[Coll], io: &mut dyn Io) -> Vec<Device> {
    let end = Instant::now() + BUDGET;
    let mut out = Vec::new();
    for pid in products(colls, VID) {
        if pid != 0x0066 && pid != 0x0064 {
            continue;
        }
        for c in find(colls, VID, pid, |c| {
            c.page == 0xFFB5 && c.usage == 1 && c.in_len == 8 && c.out_len == 8
        }) {
            if ms_left(end) == 0 {
                return out;
            }
            let Some(pct) = io.open(c).and_then(|mut d| query(d.as_mut(), end)) else {
                continue;
            };
            let kind = match kind_of(colls, VID, pid) {
                Kind::Other => Kind::Mouse,
                k => k,
            };
            let mut s = Device::new(
                format!("raigor:{}:{}", hex4(VID), hex4(pid)),
                c.product_or("2.4 GHz mouse"),
                kind,
                "2.4 GHz receiver",
            );
            s.percent = Some(pct);
            s.grace = Duration::from_secs(1800);
            out.push(s);
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::hid_dev::fake::{Fake, coll};

    fn dongle(pid: u16) -> Coll {
        coll(VID, pid, 0, 0xFFB5, 1, 8, 8, 0)
    }

    fn page(level: u8) -> Vec<u8> {
        vec![0xB5, 5, 0x94, 4, 0x35, 9, level, 0x38]
    }

    fn poll_frames(c: Coll, frames: Vec<Vec<u8>>) -> (Vec<Device>, Fake) {
        let mut f = Fake::new();
        for r in frames {
            f.s().push_input("", r);
        }
        let out = poll_with(&[c], &mut f);
        (out, f)
    }

    // From raigor_driver_test: chunk 94 = 35 09 40 38 -> 64% on a Raigor III dongle.
    #[test]
    fn reads_status_page_chunk() {
        for pid in [0x0066, 0x0064] {
            for level in [0u8, 64, 100] {
                let (out, f) = poll_frames(dongle(pid), vec![page(level)]);
                assert_eq!(out.len(), 1);
                assert_eq!((out[0].percent, out[0].charging), (Some(level), false));
                assert_eq!(out[0].key, format!("raigor:1EA7:{pid:04X}"));
                assert_eq!(out[0].grace, Duration::from_secs(1800));
                assert_eq!(
                    f.s().sent,
                    vec![(crate::drivers::hid_dev::fake::Sent::Write, REQUEST.to_vec())]
                );
            }
        }
        let (out, _) = poll_frames(dongle(0x66), vec![vec![0xB5, 0x67, 1, 2, 3, 4, 9, 42]]);
        assert_eq!(out[0].percent, Some(42));
    }

    #[test]
    fn rejects_malformed_frames() {
        for fault in 0..5 {
            let mut bad = page(64);
            match fault {
                0 => bad[0] = 0,
                1 => bad[1] = 0,
                2 => bad[2] = 0,
                3 => bad[3] = 0,
                _ => bad[6] = 101,
            }
            assert!(poll_frames(dongle(0x66), vec![bad]).0.is_empty());
        }
        assert_eq!(
            poll_frames(dongle(0x66), vec![vec![0xB5], page(64)])
                .0
                .len(),
            1
        );
        assert!(
            poll_frames(dongle(0x66), vec![vec![0xB5, 0x67, 1, 2, 3, 4, 9, 255]])
                .0
                .is_empty()
        );
        assert!(poll_frames(dongle(0x66), vec![]).0.is_empty());
    }

    #[test]
    fn other_collections_are_never_opened() {
        for fault in 0..8 {
            let mut c = dongle(0x66);
            match fault {
                0 => c.vid += 1,
                1 => c.pid = 0xFFFF,
                2 => c.page += 1,
                3 => c.usage += 1,
                4 => c.in_len += 1,
                5 => c.out_len += 1,
                6 => c.in_len -= 1,
                _ => c.out_len -= 1,
            }
            let (out, f) = poll_frames(c, vec![page(64)]);
            assert!(out.is_empty());
            assert_eq!(f.s().opens, 0);
        }
        let mut f = Fake::new();
        f.s().refuse_send = true;
        assert!(poll_with(&[dongle(0x66)], &mut f).is_empty());
    }
}

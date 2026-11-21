//! Press Play ICARUS V2 Nearlink mouse on its 2.4 GHz receiver (7768:6434). On vendor collection
//! FFB2:0001 (64-byte input / output) the battery query is output report 06 05 followed by zeroes;
//! the receiver answers [06][05][26] .. [13] = battery percent. The other commands seen in captures
//! of Press Play's web hub are not sent.
//!
//! Ported from BatteryHub (src/drivers/pressplay_icarus.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0.

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::hid::{self, Coll, Dev, Hid, find};

pub const VID: u16 = 0x7768;
pub const PID: u16 = 0x6434;
const REPORT_LEN: u16 = 64;

pub fn parse(r: &[u8]) -> Option<u8> {
    (r.len() > 13 && r[0] == 0x06 && r[1] == 0x05 && r[2] == 0x26 && r[13] <= 100).then(|| r[13])
}

fn query(io: &mut dyn Dev, out_len: usize) -> Option<u8> {
    io.flush();
    let mut req = vec![0u8; out_len.max(2)];
    req[..2].copy_from_slice(&[0x06, 0x05]);
    if !io.set_output(&req) {
        return None;
    }
    parse(&io.read(hid::MAX_WAIT)?)
}

pub fn poll_colls(colls: &[Coll], hid: &mut dyn Hid) -> Vec<Device> {
    for c in find(colls, VID, PID, |c| {
        c.page == 0xFFB2 && c.usage == 0x0001 && c.in_len == REPORT_LEN && c.out_len == REPORT_LEN
    }) {
        let Some(pct) = hid
            .open(c)
            .and_then(|mut io| query(io.as_mut(), c.out_len.into()))
        else {
            continue;
        };
        let mut d = Device::new(
            "pressplay:icarus-v2",
            "Press Play ICARUS V2 Nearlink",
            Kind::Mouse,
            "2.4 GHz receiver",
        );
        d.percent = Some(pct);
        return vec![d];
    }
    Vec::new()
}

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    hid::run(api, infos, |hid, colls| poll_colls(colls, hid))
}

#[cfg(test)]
mod tests {
    // tests/pressplay_icarus_test.cpp
    use super::*;
    use crate::drivers::hid::fake::Fake;

    fn reply(level: u8) -> Vec<u8> {
        let mut r = vec![0u8; 64];
        r[..3].copy_from_slice(&[6, 5, 0x26]);
        r[13] = level;
        r
    }
    fn run(c: Coll, reply: Option<Vec<u8>>) -> (Vec<Device>, Fake, usize) {
        let f = Fake::new();
        if let Some(r) = reply {
            f.queue(vec![r]);
        }
        let out = poll_colls(&[c], &mut f.clone());
        let n = f.s().opened.len();
        (out, f, n)
    }
    fn collection() -> Coll {
        Coll::new(VID, PID, 0xFFB2, 1, 64, 64)
    }

    #[test]
    fn levels() {
        for level in [0, 90, 100, 101] {
            let (out, f, _) = run(collection(), Some(reply(level)));
            let mut expected = vec![0u8; 64];
            expected[..2].copy_from_slice(&[6, 5]);
            assert_eq!(f.s().outputs(), vec![expected]);
            if level <= 100 {
                assert_eq!(out[0].percent, Some(level));
                assert_eq!(out[0].kind, Kind::Mouse);
            } else {
                assert!(out.is_empty());
            }
        }
    }

    #[test]
    fn malformed_replies() {
        for fault in 0..5 {
            let mut r = reply(90);
            match fault {
                0..=2 => r[fault] ^= 1,
                3 => r.truncate(13),
                _ => r.clear(),
            }
            assert!(run(collection(), Some(r)).0.is_empty());
        }
        assert!(run(collection(), None).0.is_empty());
    }

    #[test]
    fn strict_matching() {
        for fault in 0..6 {
            let mut c = collection();
            match fault {
                0 => c.vid += 1,
                1 => c.pid += 1,
                2 => c.page += 1,
                3 => c.usage += 1,
                4 => c.in_len -= 1,
                _ => c.out_len -= 1,
            }
            let (out, _, opens) = run(c, Some(reply(90)));
            assert!(out.is_empty() && opens == 0);
        }
    }
}

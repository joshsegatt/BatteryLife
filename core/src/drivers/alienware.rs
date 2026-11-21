//! Alienware AW920H Tri-Mode headset on its 2.4 GHz dongle (413C:A515), as Alienware Command Center
//! reads it: interface 5, collection FF13:0001, 62-byte output report 06 / input report 07.
//!   request (HidD_SetOutputReport): [06][C0][0A][00 ...]
//!   reply (GetInputReport 07):      [07][C0][0A][09][00][linked][percent][charging][critical][..][..]
//!                                   [critical percent][charge mode][checksum = XOR of 3..12]
//! [07][00][80] means "no answer yet". About 1 query in 100 is dropped, so it is resent (3 tries).
//! In Bluetooth mode (413C:A514) Windows keeps the level itself, so the Bluetooth source covers it.
//!
//! Ported from BatteryHub (src/drivers/alienware.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0.

use std::time::{Duration, Instant};

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::hid::{self, Coll, Dev, Hid, find};

pub const VID: u16 = 0x413C;
pub const PID: u16 = 0xA515;
const ATTEMPTS: usize = 3;
const ATTEMPT: Duration = Duration::from_millis(180); // a normal reply takes 30-100 ms
const TOTAL: Duration = Duration::from_millis(600);

/// (percent, charging) from a battery reply, decoded the way Command Center does.
pub fn decode(r: &[u8]) -> Option<(u8, bool)> {
    if r.len() < 14 || r[..4] != [0x07, 0xC0, 0x0A, 0x09] {
        return None;
    }
    let x = r[3..=12].iter().fold(0, |x, v| x ^ v);
    if x != r[13] || r[6] == 0 {
        return None;
    }
    // Battery critical: the level is in byte 11 instead of byte 6.
    let pct = if r[8] == 1 { r[11] } else { r[6] };
    (pct <= 100).then_some((pct, r[7] == 1))
}

fn query(io: &mut dyn Dev) -> Option<(u8, bool)> {
    let mut req = [0u8; 62];
    req[..3].copy_from_slice(&[0x06, 0xC0, 0x0A]);
    let deadline = Instant::now() + TOTAL;
    for _ in 0..ATTEMPTS {
        if Instant::now() >= deadline {
            break;
        }
        // Take the reply if it also arrives as an interrupt report.
        if io.set_output(&req)
            && let Some(b) = io.read(Duration::from_millis(20)).and_then(|r| decode(&r))
        {
            return Some(b);
        }
        let attempt_end = deadline.min(Instant::now() + ATTEMPT);
        loop {
            io.sleep(Duration::from_millis(15));
            if let Some(b) = io.get_input(0x07).and_then(|r| decode(&r)) {
                return Some(b);
            }
            if Instant::now() >= attempt_end {
                break;
            }
        }
    }
    None
}

pub fn poll_colls(colls: &[Coll], hid: &mut dyn Hid) -> Vec<Device> {
    for c in find(colls, VID, PID, |c| {
        c.iface == 5 && c.page == 0xFF13 && c.usage == 0x0001 && c.in_len == 62 && c.out_len == 62
    }) {
        let Some(mut io) = hid.open(c) else { continue };
        // Headset off, out of range, or every query dropped: left out, the last level stays for
        // the grace period.
        let Some((pct, charging)) = query(io.as_mut()) else {
            break;
        };
        let mut d = Device::new(
            "alienware:A515",
            "Alienware AW920H",
            Kind::Headset,
            "2.4 GHz dongle",
        );
        d.percent = Some(pct);
        d.charging = charging;
        d.grace = Duration::from_secs(180);
        return vec![d];
    }
    Vec::new()
}

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    hid::run(api, infos, |hid, colls| poll_colls(colls, hid))
}

#[cfg(test)]
mod tests {
    // tests/alienware_driver_test.cpp
    use super::*;
    use crate::drivers::hid::fake::{Fake, Op, queued};
    use std::collections::VecDeque;

    fn report(head: &[u8]) -> Vec<u8> {
        let mut r = head.to_vec();
        r.resize(62, 0);
        r
    }
    // 95%, checksums 11 and 06 (XOR of bytes 3..12).
    fn captured() -> Vec<u8> {
        report(&[
            0x07, 0xC0, 0x0A, 0x09, 0x00, 0x01, 0x5F, 0x00, 0x00, 0x0B, 0x10, 0x5F, 0x02, 0x11,
        ])
    }
    fn captured2() -> Vec<u8> {
        report(&[
            0x07, 0xC0, 0x0A, 0x09, 0x00, 0x01, 0x5F, 0x00, 0x00, 0x1C, 0x10, 0x5F, 0x02, 0x06,
        ])
    }
    fn busy() -> Vec<u8> {
        report(&[0x07, 0x00, 0x80])
    }
    fn battery(pct: u8, charging: u8, critical: u8, critical_pct: Option<u8>, mode: u8) -> Vec<u8> {
        let p11 = critical_pct.unwrap_or(pct);
        let mut r = report(&[
            0x07, 0xC0, 0x0A, 0x09, 0x00, 0x01, pct, charging, critical, 0x1C, 0x10, p11, mode,
        ]);
        r[13] = r[3..=12].iter().fold(0, |x, v| x ^ v);
        r
    }

    fn collection() -> Coll {
        let mut c = Coll::new(VID, PID, 0xFF13, 0x0001, 62, 62);
        c.iface = 5;
        c.feat_len = 3;
        c
    }
    fn consumer() -> Coll {
        let mut c = collection();
        c.page = 0x000C;
        c.in_len = 2;
        c.out_len = 0;
        c
    }

    /// The dongle answers "busy" until `answer_on` queries were sent, then the queued replies.
    fn dongle(replies: Vec<Vec<u8>>, answer_on: usize, repeat: bool) -> Fake {
        let mut q: VecDeque<Vec<u8>> = replies.into();
        Fake::with(move |s, c, op| {
            let Op::GetInput(id) = op else {
                return queued(s, c, op);
            };
            assert_eq!(*id, 0x07);
            let sends = s.outputs().len();
            if q.is_empty() || sends < answer_on {
                return Some(busy());
            }
            if repeat {
                q.front().cloned()
            } else {
                q.pop_front()
            }
        })
    }

    fn run(f: &Fake) -> (Vec<Device>, usize) {
        let colls = [consumer(), collection()];
        let out = poll_colls(&colls, &mut f.clone());
        assert!(
            f.s()
                .opened
                .iter()
                .all(|p| colls.iter().any(|c| c.path == *p && c.page == 0xFF13))
        );
        let n = f.s().opened.len();
        (out, n)
    }

    fn level(replies: Vec<Vec<u8>>, pct: u8, charging: bool) {
        let n = replies.len();
        let f = dongle(replies, 1, false);
        let (out, opens) = run(&f);
        let s = f.s();
        assert_eq!((opens, s.outputs().len(), s.input_reads()), (1, 1, n));
        assert_eq!((out[0].percent, out[0].charging), (Some(pct), charging));
    }

    #[test]
    fn captured_reply_after_two_busy_reads() {
        assert_eq!(battery(0x5F, 0, 0, None, 0x02), captured2());
        let f = dongle(vec![busy(), busy(), captured()], 1, false);
        let (out, _) = run(&f);
        assert_eq!(f.s().outputs(), vec![report(&[0x06, 0xC0, 0x0A])]);
        assert_eq!(f.s().input_reads(), 3);
        let d = &out[0];
        assert_eq!((d.percent, d.charging, d.asleep), (Some(95), false, false));
        assert_eq!(
            (d.key.as_str(), d.name.as_str()),
            ("alienware:A515", "Alienware AW920H")
        );
        assert_eq!((d.kind, d.via.as_str()), (Kind::Headset, "2.4 GHz dongle"));
        assert!(d.grace >= Duration::from_secs(60));
    }

    #[test]
    fn levels_critical_and_charging() {
        level(vec![busy(), captured2()], 95, false);
        level(vec![battery(1, 0, 0, None, 2)], 1, false);
        level(vec![battery(100, 0, 0, None, 2)], 100, false);
        // Level 0 is rejected like Command Center does; a later valid reply is used.
        level(
            vec![battery(0, 0, 0, None, 2), battery(37, 0, 0, None, 2)],
            37,
            false,
        );
        level(vec![battery(80, 0, 0, Some(79), 2)], 80, false);
        level(vec![battery(10, 0, 1, Some(4), 2)], 4, false);
        for mode in [0, 1, 2] {
            level(vec![battery(60, 1, 0, None, mode)], 60, true);
        }
        level(vec![battery(60, 2, 0, None, 2)], 60, false); // only 1 means charging
    }

    #[test]
    fn interrupt_reply_is_used_directly() {
        let f = dongle(vec![], 1, false);
        f.queue(vec![captured()]); // answered right after the output report
        let (out, _) = run(&f);
        assert_eq!((out[0].percent, f.s().input_reads()), (Some(95), 0));
        let f = dongle(vec![captured()], 1, false);
        f.queue(vec![busy()]);
        let (out, _) = run(&f);
        assert_eq!((out[0].percent, f.s().input_reads()), (Some(95), 1));
    }

    #[test]
    fn malformed_replies_skipped_before_a_valid_one() {
        let (mut bad_sum, mut wrong_id, mut wrong_cmd, mut long_len, mut short_len) =
            (captured(), captured(), captured(), captured(), captured());
        bad_sum[13] ^= 1;
        wrong_id[0] = 0x08;
        wrong_cmd[2] = 0x0B;
        long_len[3] = 0x3A;
        short_len[3] = 0x02;
        let replies = vec![
            bad_sum,
            wrong_id,
            wrong_cmd,
            long_len,
            short_len,
            battery(101, 0, 0, None, 2),
            vec![0u8; 5],
            battery(42, 0, 0, None, 2),
        ];
        let f = dongle(replies, 1, false);
        let (out, _) = run(&f);
        assert_eq!((out[0].percent, f.s().input_reads()), (Some(42), 8));
    }

    #[test]
    fn dropped_query_is_resent() {
        for answered_by in [2, 3] {
            let f = dongle(vec![captured()], answered_by, false);
            let start = Instant::now();
            let (out, _) = run(&f);
            assert!(start.elapsed() < Duration::from_millis(800));
            assert_eq!(
                (out[0].percent, f.s().outputs().len()),
                (Some(95), answered_by)
            );
        }
    }

    #[test]
    fn headset_off_is_left_out_within_the_deadline() {
        let f = dongle(vec![], 1, false);
        let start = Instant::now();
        let (out, _) = run(&f);
        assert!(start.elapsed() < Duration::from_millis(800));
        assert!(out.is_empty());
        assert_eq!(f.s().outputs().len(), 3);
        let mut bad = captured();
        bad[13] ^= 1;
        let f = dongle(vec![bad], 1, true);
        assert!(run(&f).0.is_empty());
    }

    #[test]
    fn strict_matching() {
        for field in 0..7 {
            let mut c = collection();
            match field {
                0 => c.vid = 0x413D,
                1 => c.pid = 0xA514,
                2 => c.iface = 4,
                3 => c.page = 0xFF00,
                4 => c.usage = 2,
                5 => c.in_len = 61,
                _ => c.out_len = 61,
            }
            let f = dongle(vec![captured()], 1, false);
            assert!(poll_colls(&[c], &mut f.clone()).is_empty());
            assert!(f.s().opened.is_empty());
        }
    }
}

//! Redragon wireless mice on the Evision 2.4 GHz receiver 320F:226F ("2.4G Wireless Receiver"), e.g. the
//! K1NG M999.
//!
//! Same battery request as the ATK V75X (see `atk_keyboard.rs`), with variant byte 02 as the vendor
//! software sends for this receiver: output report 04 on FF1C:0092 [04][seq LE][1A][02][00 ...];
//! reply [04][seq LE][1A][02][00 00][00][percent][state: 0 on battery, 1/2 charging]. Asked once per
//! poll; no reply is shown as asleep.
//!
//! Ported from BatteryHub (src/drivers/redragon_evision.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0.

use std::sync::atomic::{AtomicU16, Ordering};
use std::time::{Duration, Instant};

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::atk_keyboard::{evision_read, next_seq};
use crate::drivers::hidport::{BUDGET, Coll, Opener, RECEIVER, RECEIVER_GRACE, colls, hid_opener};

pub const VID: u16 = 0x320F;
pub const PIDS: &[u16] = &[0x226F];

/// `seq` is the previous request's sequence; the next one is used.
pub fn poll_with(colls: &[Coll], open: &mut Opener, seq: &mut u16) -> Vec<Device> {
    let Some(c) = colls
        .iter()
        .find(|c| c.is(VID, 0x226F) && c.page == 0xFF1C && c.usage == 0x0092)
    else {
        return Vec::new();
    };
    // The receiver id may be shared by several models; only the M999 is verified.
    let mut d = Device::new(
        "redragon_evision:226F",
        "Redragon wireless mouse",
        Kind::Mouse,
        RECEIVER,
    );
    d.grace = RECEIVER_GRACE;
    let s = next_seq(seq);
    match open(c).and_then(|mut p| evision_read(p.as_mut(), s, 0x02, Duration::from_secs(1))) {
        Some((p, ch)) => (d.percent, d.charging) = (Some(p), ch),
        None => d.asleep = true, // asleep, off or busy: keep the last value
    }
    vec![d]
}

static SEQ: AtomicU16 = AtomicU16::new(0);

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    let mut seq = SEQ.load(Ordering::Relaxed);
    let out = poll_with(
        &colls(infos),
        &mut hid_opener(api, Instant::now() + BUDGET),
        &mut seq,
    );
    SEQ.store(seq, Ordering::Relaxed);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::hidport::fake::{self, at, coll};

    fn report(head: &[u8]) -> Vec<u8> {
        let mut r = head.to_vec();
        r.resize(64, 0);
        r
    }

    /// Captured: request 04 1C 00 1A 02, reply 04 1C 00 1A 02 00 00 00 61 00 (0x61 = 97 %).
    fn captured(seq: u16) -> Vec<u8> {
        let [lo, hi] = seq.to_le_bytes();
        report(&[0x04, lo, hi, 0x1A, 0x02, 0, 0, 0, 0x61, 0])
    }

    fn colls() -> Vec<Coll> {
        vec![
            at(coll(VID, 0x226F, 0x0001, 0x0002, 65), "mouse"),
            at(coll(VID, 0x226F, 0xFF00, 0x0000, 65), "other-vendor"),
            at(coll(VID, 0x226F, 0xFF1C, 0x0092, 65), "vendor"),
        ]
    }

    #[test]
    fn captured_request_and_reply() {
        let s = fake::per_write(vec![vec![captured(1)]]);
        let mut seq = 0;
        let out = poll_with(&colls(), &mut fake::opener(&s), &mut seq);
        assert_eq!(s.borrow().opened, vec!["vendor".to_string()]);
        let w = &s.borrow().writes[0];
        assert_eq!((w.len(), w[0], w[3], w[4]), (32, 0x04, 0x1A, 0x02));
        let d = &out[0];
        assert_eq!((d.percent, d.charging, d.asleep), (Some(97), false, false));
        assert_eq!(
            (d.key.as_str(), d.kind),
            ("redragon_evision:226F", Kind::Mouse)
        );
    }

    #[test]
    fn keep_alive_and_mismatched_replies_are_skipped() {
        let ping = report(&[0x04, 0xAA, 0x00, 0xAA, 0, 0, 0, 0, 0xFF, 0]);
        let good = captured(2);
        let mut wrong_seq = good.clone();
        wrong_seq[1] = 0x55;
        let mut wrong_cmd = good.clone();
        wrong_cmd[3] = 0x1B;
        let mut non_zero = good.clone();
        non_zero[5] = 1;
        let s = fake::per_write(vec![vec![ping, wrong_seq, wrong_cmd, non_zero, good]]);
        let mut seq = 1;
        let out = poll_with(&colls(), &mut fake::opener(&s), &mut seq);
        assert_eq!((out[0].percent, s.borrow().reads), (Some(97), 5));
    }

    #[test]
    fn charging_bad_level_silence_and_failures() {
        let mut r = captured(3);
        r[8] = 55;
        r[9] = 1;
        let s = fake::per_write(vec![vec![r]]);
        let mut seq = 2;
        let d = &poll_with(&colls(), &mut fake::opener(&s), &mut seq)[0];
        assert_eq!((d.percent, d.charging), (Some(55), true));

        let mut r = captured(4);
        r[8] = 0xFF;
        let s = fake::per_write(vec![vec![r]]);
        let d = &poll_with(&colls(), &mut fake::opener(&s), &mut seq)[0];
        assert!(d.asleep && d.percent.is_none() && d.grace > Duration::ZERO);

        let d = &poll_with(&colls(), &mut fake::opener(&fake::script()), &mut seq)[0];
        assert!(d.asleep);

        let s = fake::script();
        s.borrow_mut().fail_open = true;
        assert!(poll_with(&colls(), &mut fake::opener(&s), &mut seq)[0].asleep);
        let s = fake::script();
        s.borrow_mut().fail_write = true;
        assert!(poll_with(&colls(), &mut fake::opener(&s), &mut seq)[0].asleep);
        assert_eq!(s.borrow().reads, 0);
    }

    #[test]
    fn strict_matching() {
        let s = fake::script();
        let other = [
            at(coll(VID, 0x5088, 0xFF1C, 0x0092, 65), "vendor"),
            at(coll(VID, 0x226F, 0xFF00, 0x0000, 65), "other-vendor"),
        ];
        assert!(poll_with(&other, &mut fake::opener(&s), &mut 0).is_empty());
        assert!(s.borrow().opened.is_empty());
    }
}

//! CHERRY (VID 046A): MX 3.0S Wireless keyboard on its 2.4 GHz dongle (00EA) or USB cable (00E3),
//! and the Cherry XTRFY 1K Mouse Receiver (0330) with its mouse.
//!
//! Keyboard: 64-byte vendor channel on usage page FF1C, usage 0092 (the 9-byte FF1C:0092 collection
//! is not used). Frame: [0] report 4, [1..2] checksum LE = sum(bytes[3..]) & 0xFFFF, [3] type,
//! [4..] payload. Status query: type 1A, payload 03 00 00 55 (read-only); reply [0] 4, [3] 1A,
//! [8] percent, [9] non-zero while USB powered. Framing from B1Chat/BatteryWatch (MIT).
//!
//! XTRFY receiver: 17-byte CompX frames on FF02:0002, report 08, checksum = 0x55 - sum(bytes 0..15).
//! Command 03 = link status ([6] non-zero while the mouse is linked), 04 = GetPower ([6] percent,
//! [7] charging, [8..9] mV big endian). Only these two read-only commands are sent.
//!
//! Ported from BatteryHub (src/drivers/cherry.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0.

use std::time::{Duration, Instant};

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::hidport::{
    BUDGET, CABLE, Coll, Opener, Port, RECEIVER, RECEIVER_GRACE, colls, hid_opener, wait_for,
};

pub const VID: u16 = 0x046A;
const XTRFY: u16 = 0x0330;
pub const PIDS: &[u16] = &[0x00E3, 0x00EA, XTRFY];

/// Cable first: when it is plugged in the keyboard is awake, and only one entry is reported.
const KEYBOARDS: [(u16, &str); 2] = [(0x00E3, CABLE), (0x00EA, RECEIVER)];

/// The FF1C status query (report 4, type 1A, payload 03 00 00 55), shared by Evision-based boards.
pub fn ff1c_status_query() -> Vec<u8> {
    let mut f = vec![0u8; 64];
    f[0] = 0x04;
    f[3] = 0x1A;
    f[4] = 0x03;
    f[7] = 0x55;
    let s: u32 = f[3..].iter().map(|b| u32::from(*b)).sum();
    f[1] = s as u8;
    f[2] = (s >> 8) as u8;
    f
}

fn is_status_reply(r: &[u8]) -> bool {
    r.len() >= 10 && r[0] == 0x04 && r[3] == 0x1A
}

/// Sends the FF1C status query (two tries, 400 ms each) and returns the first reply `accept` takes.
pub fn ff1c_status(port: &mut dyn Port, accept: impl Fn(&[u8]) -> bool) -> Option<Vec<u8>> {
    port.flush();
    let q = ff1c_status_query();
    for _ in 0..2 {
        if !port.write(&q) {
            return None;
        }
        if let Some(r) = wait_for(port, Duration::from_millis(400), &accept) {
            return Some(r);
        }
    }
    None
}

fn is_keyboard_channel(c: &Coll) -> bool {
    c.page == 0xFF1C && c.usage == 0x0092 && c.in_len == 64 && c.out_len == 64
}

fn poll_keyboard(colls: &[Coll], open: &mut Opener) -> Option<Device> {
    for (pid, via) in KEYBOARDS {
        for c in colls
            .iter()
            .filter(|c| c.is(VID, pid) && is_keyboard_channel(c))
        {
            let Some(mut port) = open(c) else { continue };
            // Keyboard off or asleep: the dongle stays silent and the tracker keeps the last value.
            let Some(r) = ff1c_status(port.as_mut(), is_status_reply) else {
                continue;
            };
            let mut d = Device::new(
                "cherry:mx30s",
                "CHERRY MX 3.0S Wireless",
                Kind::Keyboard,
                via,
            );
            if r[8] <= 100 {
                d.percent = Some(r[8]);
                d.charging = r[9] != 0; // USB powered, i.e. plugged in
            } else {
                d.asleep = true; // answered without a usable level
            }
            d.grace = RECEIVER_GRACE;
            return Some(d);
        }
    }
    None
}

fn xtrfy_query(cmd: u8) -> Vec<u8> {
    let mut f = vec![0u8; 17];
    f[0] = 0x08;
    f[1] = cmd;
    f[16] = 0x55u8.wrapping_sub(crate::drivers::hidport::sum(&f[..16]));
    f
}

fn xtrfy_frame_ok(r: &[u8], cmd: u8) -> bool {
    r.len() == 17
        && r[0] == 0x08
        && r[1] == cmd
        && 0x55u8.wrapping_sub(crate::drivers::hidport::sum(&r[..16])) == r[16]
}

fn xtrfy_exchange(port: &mut dyn Port, cmd: u8) -> Option<Vec<u8>> {
    port.flush();
    if !port.write(&xtrfy_query(cmd)) {
        return None;
    }
    wait_for(port, Duration::from_millis(500), |r| xtrfy_frame_ok(r, cmd))
}

fn poll_xtrfy(colls: &[Coll], open: &mut Opener) -> Option<Device> {
    let c = colls.iter().find(|c| {
        c.is(VID, XTRFY)
            && c.page == 0xFF02
            && c.usage == 0x0002
            && c.in_len == 17
            && c.out_len == 17
    })?;
    let mut port = open(c)?;
    let mut d = Device::new(
        "cherry:xtrfy",
        "Cherry XTRFY wireless mouse",
        Kind::Mouse,
        RECEIVER,
    );
    let mut answered = false;
    let mut connected = false;
    // Check whether the mouse is switched on and linked before asking it for power.
    if let Some(r) = xtrfy_exchange(port.as_mut(), 0x03) {
        answered = true;
        connected = r[6] != 0;
    }
    if !answered || connected {
        if let Some(r) = xtrfy_exchange(port.as_mut(), 0x04) {
            answered = true;
            connected = true;
            if r[6] <= 100 {
                d.percent = Some(r[6]);
            }
            d.charging = r[7] != 0;
            let mv = u16::from_be_bytes([r[8], r[9]]);
            d.millivolts = (mv != 0).then_some(mv);
        } else if answered {
            d.asleep = true; // linked, but no usable power reply
        }
    }
    // Silent receiver (mouse asleep) or mouse switched off: nothing to show.
    if !answered || !connected {
        return None;
    }
    d.grace = RECEIVER_GRACE;
    Some(d)
}

pub fn poll_with(colls: &[Coll], open: &mut Opener) -> Vec<Device> {
    poll_keyboard(colls, open)
        .into_iter()
        .chain(poll_xtrfy(colls, open))
        .collect()
}

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    poll_with(&colls(infos), &mut hid_opener(api, Instant::now() + BUDGET))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::hidport::fake::{self, coll};

    fn kb(pid: u16) -> Coll {
        coll(VID, pid, 0xFF1C, 0x0092, 64)
    }

    fn reply(percent: u8, usb: u8, kind: u8) -> Vec<u8> {
        let mut r = vec![0u8; 64];
        r[0] = 0x04;
        r[3] = kind;
        r[8] = percent;
        r[9] = usb;
        r
    }

    fn run(s: &fake::Shared, colls: &[Coll]) -> Vec<Device> {
        poll_with(colls, &mut fake::opener(s))
    }

    #[test]
    fn query_frame_matches_batteryhub() {
        // Report 4, checksum 0x0072 (= 0x1A + 3 + 0x55), type 1A, payload 03 00 00 55.
        let mut expected = vec![0u8; 64];
        expected[..8].copy_from_slice(&[0x04, 0x72, 0x00, 0x1A, 0x03, 0, 0, 0x55]);
        assert_eq!(ff1c_status_query(), expected);
    }

    #[test]
    fn dongle_reply_gives_level() {
        let s = fake::per_write(vec![vec![reply(73, 0, 0x1A)]]);
        let out = run(&s, &[kb(0x00EA)]);
        assert_eq!(s.borrow().writes, vec![ff1c_status_query()]);
        assert_eq!(out.len(), 1);
        let d = &out[0];
        assert_eq!((d.percent, d.charging, d.asleep), (Some(73), false, false));
        assert_eq!(
            (d.key.as_str(), d.name.as_str()),
            ("cherry:mx30s", "CHERRY MX 3.0S Wireless")
        );
        assert_eq!((d.kind, d.via.as_str()), (Kind::Keyboard, RECEIVER));
        assert!(d.grace > Duration::ZERO);

        let s = fake::per_write(vec![vec![reply(100, 1, 0x1A)]]);
        let d = &run(&s, &[kb(0x00EA)])[0];
        assert_eq!((d.percent, d.charging), (Some(100), true));
    }

    #[test]
    fn unrelated_reports_skipped_and_query_retried_once() {
        let s = fake::per_write(vec![vec![
            reply(50, 0, 0x2B),
            vec![4, 0, 0, 0x1A],
            reply(42, 0, 0x1A),
        ]]);
        assert_eq!(run(&s, &[kb(0x00EA)])[0].percent, Some(42));

        let s = fake::per_write(vec![vec![], vec![reply(64, 0, 0x1A)]]);
        assert_eq!(run(&s, &[kb(0x00EA)])[0].percent, Some(64));
        assert_eq!(s.borrow().writes.len(), 2);

        let s = fake::script();
        assert!(run(&s, &[kb(0x00EA)]).is_empty());
        assert_eq!(s.borrow().writes.len(), 2);
    }

    #[test]
    fn answer_without_level_is_asleep() {
        let s = fake::per_write(vec![vec![reply(0xFF, 0, 0x1A)]]);
        let d = &run(&s, &[kb(0x00EA)])[0];
        assert!(d.asleep && d.percent.is_none());
    }

    #[test]
    fn only_the_64_byte_ff1c_channel_is_opened() {
        let mut short = kb(0x00EA);
        short.in_len = 9;
        short.out_len = 9;
        for c in [
            kb(0x01E4),
            short.clone(),
            coll(VID, 0x00EA, 0xFF00, 0x0092, 64),
            coll(VID, 0x00EA, 0xFF1C, 0x0001, 64),
        ] {
            let s = fake::per_write(vec![vec![reply(80, 0, 0x1A)]]);
            assert!(run(&s, &[c]).is_empty());
            assert!(s.borrow().opened.is_empty());
        }
        let s = fake::per_write(vec![vec![reply(55, 0, 0x1A)]]);
        let out = run(&s, &[short, kb(0x00EA)]);
        assert_eq!((s.borrow().opened.len(), out[0].percent), (1, Some(55)));
    }

    #[test]
    fn cable_is_preferred_and_dongle_is_the_fallback() {
        let s = fake::per_write(vec![vec![reply(91, 1, 0x1A)]]);
        let d = &run(&s, &[kb(0x00E3)])[0];
        assert_eq!(
            (d.percent, d.charging, d.via.as_str()),
            (Some(91), true, CABLE)
        );
        assert_eq!(d.key, "cherry:mx30s");

        let s = fake::per_write(vec![vec![reply(90, 1, 0x1A)]]);
        let out = run(&s, &[kb(0x00EA), kb(0x00E3)]);
        assert_eq!((s.borrow().opened.len(), out.len()), (1, 1));
        assert_eq!((out[0].via.as_str(), out[0].percent), (CABLE, Some(90)));

        let s = fake::per_write(vec![vec![], vec![], vec![reply(60, 0, 0x1A)]]);
        let out = run(&s, &[kb(0x00E3), kb(0x00EA)]);
        assert_eq!(s.borrow().writes.len(), 3);
        assert_eq!((out[0].via.as_str(), out[0].percent), (RECEIVER, Some(60)));
    }

    fn xtrfy() -> Coll {
        coll(VID, XTRFY, 0xFF02, 0x0002, 17)
    }

    fn x_reply(cmd: u8, v6: u8, charging: u8, mv_hi: u8, mv_lo: u8) -> Vec<u8> {
        let mut r = vec![0u8; 17];
        r[0] = 0x08;
        r[1] = cmd;
        r[5] = 0x02;
        r[6] = v6;
        r[7] = charging;
        r[8] = mv_hi;
        r[9] = mv_lo;
        r[16] = 0x55u8.wrapping_sub(crate::drivers::hidport::sum(&r[..16]));
        r
    }

    #[test]
    fn xtrfy_captured_power_reply() {
        // Captured live: 08 04 00 00 00 02 5f 00 0f f4 00 00 00 00 00 00 e5 (95 %, 4084 mV).
        let captured = vec![
            0x08, 0x04, 0, 0, 0, 0x02, 0x5F, 0, 0x0F, 0xF4, 0, 0, 0, 0, 0, 0, 0xE5,
        ];
        assert!(xtrfy_frame_ok(&captured, 0x04));
        let s = fake::per_write(vec![vec![x_reply(3, 1, 0, 0, 0)], vec![captured]]);
        let out = run(&s, &[xtrfy()]);
        let mut link = vec![0u8; 17];
        link[0] = 0x08;
        link[1] = 0x03;
        link[16] = 0x55 - 0x08 - 0x03;
        let mut power = link.clone();
        power[1] = 0x04;
        power[16] = 0x55 - 0x08 - 0x04;
        assert_eq!(s.borrow().writes, vec![link, power]);
        let d = &out[0];
        assert_eq!(
            (d.percent, d.charging, d.millivolts),
            (Some(95), false, Some(4084))
        );
        assert_eq!((d.key.as_str(), d.kind), ("cherry:xtrfy", Kind::Mouse));

        let s = fake::per_write(vec![
            vec![x_reply(3, 1, 0, 0, 0)],
            vec![x_reply(4, 100, 1, 0x10, 0x94)],
        ]);
        let d = &run(&s, &[xtrfy()])[0];
        assert_eq!(
            (d.percent, d.charging, d.millivolts),
            (Some(100), true, Some(0x1094))
        );
    }

    #[test]
    fn xtrfy_skips_bad_frames_and_handles_link_states() {
        let mut wrong = x_reply(4, 42, 0, 0, 0);
        wrong[6] = 43; // checksum no longer matches
        let s = fake::per_write(vec![
            vec![x_reply(3, 1, 0, 0, 0)],
            vec![x_reply(9, 7, 0, 0, 0), wrong, x_reply(4, 42, 0, 0, 0)],
        ]);
        assert_eq!(run(&s, &[xtrfy()])[0].percent, Some(42));

        // Mouse switched off: link 0, power not asked, nothing reported.
        let s = fake::per_write(vec![vec![x_reply(3, 0, 0, 0, 0)]]);
        assert!(run(&s, &[xtrfy()]).is_empty());
        assert_eq!(s.borrow().writes.len(), 1);

        // Silent receiver: nothing.
        assert!(run(&fake::script(), &[xtrfy()]).is_empty());

        // Linked but silent on power: asleep.
        let s = fake::per_write(vec![vec![x_reply(3, 1, 0, 0, 0)], vec![]]);
        let d = &run(&s, &[xtrfy()])[0];
        assert!(d.asleep && d.percent.is_none());

        // A short frame is never taken for a reply.
        let s = fake::per_write(vec![
            vec![vec![0x08, 0x04, 0x00]],
            vec![x_reply(4, 61, 0, 0, 0)],
        ]);
        assert_eq!(run(&s, &[xtrfy()])[0].percent, Some(61));
    }

    #[test]
    fn xtrfy_matching_is_strict_and_keyboard_is_separate() {
        let mut long = xtrfy();
        long.in_len = 64;
        long.out_len = 64;
        for c in [
            coll(VID, 0x00EA, 0xFF02, 0x0002, 17),
            coll(VID, XTRFY, 0xFF01, 0, 17),
            long,
        ] {
            let s = fake::per_write(vec![vec![x_reply(3, 1, 0, 0, 0), x_reply(4, 80, 0, 0, 0)]]);
            assert!(run(&s, &[c]).is_empty());
            assert!(s.borrow().writes.is_empty());
        }
        let s = fake::per_write(vec![
            vec![reply(70, 0, 0x1A)],
            vec![x_reply(3, 1, 0, 0, 0)],
            vec![x_reply(4, 55, 0, 0, 0)],
        ]);
        let out = run(&s, &[kb(0x00EA), xtrfy()]);
        assert_eq!(out.len(), 2);
        assert_eq!((out[0].kind, out[0].percent), (Kind::Keyboard, Some(70)));
        assert_eq!((out[1].kind, out[1].percent), (Kind::Mouse, Some(55)));
    }
}

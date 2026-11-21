//! Chimera V2 wireless mouse (3554:F512): 17-byte report 08 on the vendor collection FF02:0002,
//! checksum = 0x55 - sum(bytes 0..15). Command 03 = online status ([5] != 0 online), command 04 =
//! battery ([5] 02 connected, [6] percent, [7] charging, [8..9] millivolts big-endian).
//! 3554 is shared with Kysona and AULA, so only F512 is declared.
//!
//! Ported from BatteryHub (src/drivers/chimera.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0. Each query gets 500 ms
//! (BatteryHub: 800 ms) to keep the driver inside its time budget.

use std::time::{Duration, Instant};

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::hid_dev::{BUDGET, Coll, Dev, HidIo, Io, colls, find, hex4, ms_left};
use crate::registry::Match;

const VID: u16 = 0x3554;
const PID: u16 = 0xF512;

pub static MATCHES: &[Match] = &[Match {
    vid: VID,
    pids: &[PID],
}];

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    poll_with(&colls(infos), &mut HidIo(api))
}

fn checksum(b: &[u8]) -> u8 {
    0x55u8.wrapping_sub(b[..16].iter().fold(0u8, |a, x| a.wrapping_add(*x)))
}

pub fn request(cmd: u8) -> [u8; 17] {
    let mut q = [0u8; 17];
    q[0] = 8;
    q[1] = cmd;
    q[16] = checksum(&q);
    q
}

/// A checksummed reply to `cmd`.
pub fn is_reply(r: &[u8], cmd: u8) -> bool {
    r.len() >= 17 && r[0] == 8 && r[1] == cmd && r[16] == checksum(r)
}

/// (percent, charging, millivolts) from a battery (04) reply.
pub fn parse_battery(r: &[u8]) -> Option<(u8, bool, u16)> {
    (r.len() >= 10 && r[5] == 0x02 && (1..=100).contains(&r[6]))
        .then(|| (r[6], r[7] != 0, u16::from_be_bytes([r[8], r[9]])))
}

/// Sends `cmd` (again after every unanswered read) until a valid reply or `end`.
fn query(d: &mut dyn Dev, cmd: u8, end: Instant) -> Option<Vec<u8>> {
    let end = end.min(Instant::now() + Duration::from_millis(500));
    d.flush();
    loop {
        let left = ms_left(end);
        if left == 0 || !d.write(&request(cmd)) {
            return None;
        }
        if let Some(r) = d.read(left)
            && is_reply(&r, cmd)
        {
            return Some(r);
        }
    }
}

fn poll_with(colls: &[Coll], io: &mut dyn Io) -> Vec<Device> {
    let end = Instant::now() + BUDGET;
    for c in find(colls, VID, PID, |c| {
        c.page == 0xFF02 && c.usage == 2 && c.in_len >= 17 && c.out_len >= 17
    }) {
        let Some(mut d) = io.open(c) else { continue };
        // Wake the device with the online-status query, then read the battery.
        match query(d.as_mut(), 0x03, end) {
            Some(r) if r[5] != 0 => {}
            _ => continue,
        }
        let Some((pct, charging, mv)) =
            query(d.as_mut(), 0x04, end).and_then(|r| parse_battery(&r))
        else {
            continue;
        };
        let mut s = Device::new(
            format!("chimera:{}:{}", hex4(VID), hex4(PID)),
            "Chimera V2",
            Kind::Mouse,
            "2.4 GHz receiver",
        );
        s.percent = Some(pct);
        s.charging = charging;
        s.millivolts = Some(mv).filter(|v| *v != 0);
        s.grace = Duration::from_secs(1800);
        return vec![s];
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::hid_dev::fake::{Fake, coll};

    fn reply(cmd: u8, status: u8, pct: u8, charging: u8) -> Vec<u8> {
        let mut r = vec![0u8; 17];
        r[0] = 8;
        r[1] = cmd;
        r[5] = status;
        r[6] = pct;
        r[7] = charging;
        r[8] = 0x0F;
        r[9] = 0x3C;
        r[16] = checksum(&r);
        r
    }

    #[test]
    fn request_matches_the_documented_frame() {
        let q = request(0x04);
        assert_eq!(q[..2], [8, 4]);
        assert_eq!(q[16], 0x55 - 12);
    }

    #[test]
    fn reads_battery_after_online_check() {
        let mut f = Fake::with(|s, _, _, q| {
            let r = if q[1] == 3 {
                reply(3, 1, 0, 0)
            } else {
                reply(4, 2, 66, 1)
            };
            s.push_input("", r);
        });
        let out = poll_with(&[coll(VID, PID, 1, 0xFF02, 2, 17, 17, 0)], &mut f);
        assert_eq!(out.len(), 1);
        assert_eq!(
            (out[0].percent, out[0].charging, out[0].millivolts),
            (Some(66), true, Some(3900))
        );
    }

    #[test]
    fn offline_mouse_is_not_reported() {
        let mut f = Fake::with(|s, _, _, q| s.push_input("", reply(q[1], 0, 50, 0)));
        assert!(poll_with(&[coll(VID, PID, 1, 0xFF02, 2, 17, 17, 0)], &mut f).is_empty());
        assert!(parse_battery(&reply(4, 1, 50, 0)).is_none());
        assert!(parse_battery(&reply(4, 2, 0, 0)).is_none());
    }
}

//! MCHOSE V9 Pro 2 wireless headset on its 2.4 GHz dongle (3837:6011), "MC" protocol on vendor
//! collection FF07:0222 (report id 0x55). Frame after the report id: [0] AA start, [1] 01 version,
//! [2] flags (1 = XOR checksum), [3] length (sub-command + body), [4..5] type (00 01 request /
//! response, 00 04 notify), [6] sub-command, [7..] body, checksum. Read battery = sub-command 0B; the
//! reply and the battery notification (22) carry body[0] = 0 when the headset is off or asleep,
//! else body[1] = charging (bit 7) | percent. Read-only.
//!
//! Ported from BatteryHub (src/drivers/mchose_headset.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0, which credits MCHOSE's own
//! M HUB driver.

use std::time::{Duration, Instant};

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::hid::{self, Coll, Dev, Hid, find};

pub const VID: u16 = 0x3837;
pub const PID: u16 = 0x6011;
const REPORT_ID: u8 = 0x55;
const READ_BATTERY: u8 = 0x0B;
const NOTIFY_BATTERY: u8 = 0x22;

#[derive(Debug, PartialEq)]
pub enum Reply {
    Off,
    Level(u8, bool),
}

/// An MC battery reply or battery notification in one input report (r[0] = report id).
pub fn parse_battery(r: &[u8]) -> Option<Reply> {
    if r.len() < 9 || r[0] != REPORT_ID {
        return None;
    }
    let f = &r[1..];
    let n = f.len();
    if f[0] != 0xAA || f[1] != 0x01 || f[4] != 0x00 {
        return None;
    }
    let reply = f[5] == 0x01 && f[6] == READ_BATTERY;
    let notify = f[5] == 0x04 && f[6] == NOTIFY_BATTERY;
    if !reply && !notify {
        return None;
    }
    let len = usize::from(f[3]); // sub-command + body
    if len < 3 || 6 + len >= n {
        return None;
    }
    let check = if f[2] == 1 {
        f[2..6].iter().chain(&f[6..6 + len]).fold(0u8, |x, v| x ^ v)
    } else {
        0
    };
    if f[6 + len] != check {
        return None;
    }
    let body = &f[7..];
    if body[0] == 0 {
        return Some(Reply::Off);
    }
    let pct = body[1] & 0x7F;
    (pct <= 100).then_some(Reply::Level(pct, body[1] & 0x80 != 0))
}

fn query(io: &mut dyn Dev) -> Option<Reply> {
    io.flush();
    // Start, version, flags 0, length 1, type 00 01, sub-command, checksum 0.
    if !io.write(&[
        REPORT_ID,
        0xAA,
        0x01,
        0x00,
        0x01,
        0x00,
        0x01,
        READ_BATTERY,
        0x00,
    ]) {
        return None;
    }
    let end = Instant::now() + hid::MAX_WAIT;
    loop {
        let left = end.saturating_duration_since(Instant::now()).as_millis() as u64;
        if left == 0 {
            return None;
        }
        if let Some(reply) = parse_battery(&io.read(Duration::from_millis(left))?) {
            return Some(reply);
        }
    }
}

pub fn poll_colls(colls: &[Coll], hid: &mut dyn Hid) -> Vec<Device> {
    for c in find(colls, VID, PID, |c| {
        c.page == 0xFF07 && c.usage == 0x0222 && c.out_len >= 64 && c.in_len >= 64
    }) {
        let Some(reply) = hid.open(c).and_then(|mut io| query(io.as_mut())) else {
            continue;
        };
        let name = if c.product.is_empty() {
            "MCHOSE V9 Pro 2"
        } else {
            &c.product
        };
        let mut d = Device::new("mchose:v9pro2", name, Kind::Headset, "2.4 GHz receiver");
        d.grace = Duration::from_secs(1800);
        match reply {
            // The dongle answered, the headset is off or asleep: keep the last value.
            Reply::Off => d.asleep = true,
            Reply::Level(pct, charging) => {
                d.percent = Some(pct);
                d.charging = charging;
            }
        }
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

    /// [55][AA 01 flags len 00 type sub][body..][check]
    fn frame(flags: u8, kind: u8, sub: u8, body: &[u8]) -> Vec<u8> {
        let len = 1 + body.len() as u8;
        let mut f = vec![0xAA, 0x01, flags, len, 0x00, kind, sub];
        f.extend_from_slice(body);
        let check = if flags == 1 {
            f[2..].iter().fold(0u8, |x, v| x ^ v)
        } else {
            0
        };
        f.push(check);
        let mut r = vec![REPORT_ID];
        r.extend_from_slice(&f);
        r.resize(64, 0);
        r
    }

    #[test]
    fn reply_notification_and_off() {
        assert_eq!(
            parse_battery(&frame(0, 0x01, READ_BATTERY, &[1, 0x80 | 77])),
            Some(Reply::Level(77, true))
        );
        assert_eq!(
            parse_battery(&frame(1, 0x04, NOTIFY_BATTERY, &[1, 40])),
            Some(Reply::Level(40, false))
        );
        assert_eq!(
            parse_battery(&frame(0, 0x01, READ_BATTERY, &[0, 0])),
            Some(Reply::Off)
        );
        let mut bad = frame(1, 0x04, NOTIFY_BATTERY, &[1, 40]);
        bad[9] ^= 1; // a body byte: the checksum no longer matches
        assert_eq!(parse_battery(&bad), None);
        assert_eq!(parse_battery(&frame(0, 0x01, 0x0C, &[1, 40])), None);
    }

    #[test]
    fn dongle_query() {
        let f = Fake::new();
        f.queue(vec![frame(0, 0x01, READ_BATTERY, &[1, 63])]);
        let mut c = Coll::new(VID, PID, 0xFF07, 0x0222, 64, 64);
        c.product = "MCHOSE V9 Pro 2 Receiver".into();
        let out = poll_colls(&[c], &mut f.clone());
        assert_eq!(out[0].key, "mchose:v9pro2");
        assert_eq!(out[0].name, "MCHOSE V9 Pro 2 Receiver");
        assert_eq!(out[0].percent, Some(63));
        assert_eq!(f.s().writes()[0], vec![0x55, 0xAA, 1, 0, 1, 0, 1, 0x0B, 0]);
    }
}

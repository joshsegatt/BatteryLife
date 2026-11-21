//! Royal Kludge (RK) MG6 Pro wireless mouse, VID 2E3C: 2.4 GHz dongle 00A6 / 00E6 (FF00:0001) and USB
//! cable 00A5 / 00E5 (FFF2:0001), 64-byte reports without a report id.
//!
//! RK web driver "QiWang" getDeviceBattery(), sent as output report 0:
//!   [0] 01 remote param (to the mouse), [1] 01 system control, [2] 02 get battery voltage, [3] 01 read.
//! Reply: same first three bytes; [3] bit 4 = the mouse answered, [4..5] mV LE, [6..7] percent LE,
//! [10] USB plugged in. A receiver reply without the mouse's answer is shown as asleep.
//!
//! Ported from BatteryHub (src/drivers/royal_kludge.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0.

use std::time::{Duration, Instant};

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::hid::{self, CABLE, Coll, Dev, Hid, RECEIVER, RECEIVER_GRACE};

pub const VID: u16 = 0x2E3C;
pub const PIDS: &[u16] = &[0x00A5, 0x00A6, 0x00E5, 0x00E6];

const REQUEST: [u8; 5] = [0x00, 0x01, 0x01, 0x02, 0x01];

fn is_cable(pid: u16) -> bool {
    pid == 0x00A5 || pid == 0x00E5
}

#[derive(Debug, PartialEq)]
pub enum Battery {
    /// The receiver answered for a mouse that did not.
    Asleep,
    /// (percent, millivolts, USB plugged in)
    Online(u8, u16, bool),
    /// An unusable level: try again.
    Bad,
}

/// Classifies one report: None when it is not a battery reply (other notifications).
pub fn parse(r: &[u8]) -> Option<Battery> {
    if r.len() < 11 || r[0] & 0x1F != 0x01 || r[1] & 0x0F != 0x01 || r[2] != 0x02 {
        return None;
    }
    if (r[3] >> 4) & 1 == 0 {
        return Some(Battery::Asleep);
    }
    let p = u16::from_le_bytes([r[6], r[7]]);
    if p > 100 {
        return Some(Battery::Bad);
    }
    Some(Battery::Online(
        p as u8,
        u16::from_le_bytes([r[4], r[5]]),
        r[10] != 0,
    ))
}

/// Up to three tries; Some(battery) when the receiver answered at all.
fn query(port: &mut dyn Dev) -> Option<Battery> {
    let mut answered = None;
    for attempt in 0..3 {
        if attempt > 0 {
            port.sleep(Duration::from_millis(100));
        }
        port.flush();
        if !port.write(&REQUEST) {
            break;
        }
        let end = Instant::now() + Duration::from_millis(600);
        loop {
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            // The receiver also pushes other notifications here; a timeout or short report ends the wait.
            let Some(r) = port.read(left).filter(|r| r.len() >= 11) else {
                break;
            };
            match parse(&r) {
                None => continue,
                Some(b @ Battery::Online(..)) => return Some(b),
                Some(b) => {
                    answered = Some(b);
                    break;
                }
            }
        }
    }
    answered.map(|_| Battery::Asleep)
}

pub fn poll_with(colls: &[Coll], hid: &mut dyn Hid) -> Vec<Device> {
    let mut out = Vec::new();
    for &pid in PIDS {
        let page = if is_cable(pid) { 0xFFF2 } else { 0xFF00 };
        for c in colls.iter().filter(|c| {
            c.is(VID, pid) && c.page == page && c.usage == 1 && c.out_len == 65 && c.in_len == 65
        }) {
            let Some(mut port) = hid.open(c) else {
                continue;
            };
            let Some(b) = query(port.as_mut()) else {
                continue;
            };
            let via = if is_cable(pid) { CABLE } else { RECEIVER };
            let mut d = Device::new(
                format!("rk:{VID:04X}:{pid:04X}"),
                "RK MG6 Pro",
                Kind::Mouse,
                via,
            );
            match b {
                Battery::Online(p, mv, plugged) => {
                    d.percent = Some(p);
                    d.millivolts = (3000..=4500).contains(&mv).then_some(mv);
                    d.charging = plugged || is_cable(pid);
                }
                _ => d.asleep = true, // asleep or off: keep the last value
            }
            if !is_cable(pid) {
                d.grace = RECEIVER_GRACE;
            }
            out.push(d);
            break;
        }
    }
    out
}

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    hid::run(api, infos, |hid, colls| poll_with(colls, hid))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::hid::fake::{coll, per_write, script};

    /// A flush drops queued replies, as on the device.
    fn reply(answered: bool, mv: u16, pct: u16, usb: u8) -> Vec<u8> {
        let mut r = vec![0u8; 64];
        r[..3].copy_from_slice(&[0x01, 0x01, 0x02]);
        r[3] = 0x01 | if answered { 0x10 } else { 0 };
        r[4..6].copy_from_slice(&mv.to_le_bytes());
        r[6..8].copy_from_slice(&pct.to_le_bytes());
        r[10] = usb;
        r
    }

    #[test]
    fn parses_battery_reply() {
        assert_eq!(
            parse(&reply(true, 3900, 77, 0)),
            Some(Battery::Online(77, 3900, false))
        );
        assert_eq!(parse(&reply(false, 0, 0, 0)), Some(Battery::Asleep));
        assert_eq!(parse(&reply(true, 3900, 101, 0)), Some(Battery::Bad));
        let mut other = reply(true, 3900, 77, 0);
        other[2] = 0x05;
        assert_eq!(parse(&other), None);
    }

    #[test]
    fn dongle_and_cable() {
        let s = per_write(vec![vec![reply(true, 3950, 64, 0)]]);
        let out = poll_with(&[coll(VID, 0x00A6, 0xFF00, 1, 65)], &mut s.clone());
        assert_eq!(s.s().writes(), vec![REQUEST.to_vec()]);
        let d = &out[0];
        assert_eq!(
            (d.percent, d.millivolts, d.charging),
            (Some(64), Some(3950), false)
        );
        assert_eq!((d.key.as_str(), d.via.as_str()), ("rk:2E3C:00A6", RECEIVER));

        let s = per_write(vec![vec![reply(true, 4100, 90, 0)]]);
        let d = &poll_with(&[coll(VID, 0x00A5, 0xFFF2, 1, 65)], &mut s.clone())[0];
        assert!(d.charging && d.via == CABLE);
    }

    #[test]
    fn mouse_not_answering_is_asleep_after_three_tries() {
        let none = reply(false, 0, 0, 0);
        let s = per_write(vec![vec![none.clone()], vec![none.clone()], vec![none]]);
        let out = poll_with(&[coll(VID, 0x00E6, 0xFF00, 1, 65)], &mut s.clone());
        assert!(out[0].asleep && out[0].percent.is_none());
        assert_eq!(s.s().writes().len(), 3);
        let s = script();
        assert!(poll_with(&[coll(VID, 0x00E6, 0xFF00, 1, 65)], &mut s.clone()).is_empty());
    }
}

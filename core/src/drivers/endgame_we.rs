//! Endgame Gear XM2we on its original 2.4 GHz receiver (3367:1961), not the 4K dongle (endgame.rs).
//! Request: 17-byte feature report 08 on FF02:0002, [08][op][00 x14][checksum], the 17 bytes summing
//! to 0x55. Reply: 17-byte input report 09 on FF01:0000 of the same receiver,
//! [09][op][00 00 00][size][payload]. op 04 = battery: [percent][charging 0/1]; op 03 = radio link:
//! [1 = mouse linked]. Only these two status queries are sent; no configuration or EEPROM writes.
//!
//! Ported from BatteryHub (src/drivers/endgame_we.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0, which credits HaloBattery
//! and xm2we-linux.

use std::time::Duration;

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::hid_util::physical_key;
use crate::drivers::report_io::{self, Coll, Dev, Hid, device, find};

pub const VID: u16 = 0x3367;
pub const PID: u16 = 0x1961;
const BATTERY: u8 = 0x04;
const LINK: u8 = 0x03;

pub fn frame(op: u8) -> Vec<u8> {
    let mut d = vec![0u8; 17];
    d[0] = 0x08;
    d[1] = op;
    let sum = d[..16].iter().fold(0u8, |a, b| a.wrapping_add(*b));
    d[16] = 0x55u8.wrapping_sub(sum);
    d
}

/// The payload of a reply to `op`, or None when the report is not one.
pub fn payload(r: &[u8], op: u8) -> Option<Vec<u8>> {
    if r.len() < 17 || r[0] != 0x09 || r[1] != op || r[2] != 0 || r[3] != 0 || r[4] != 0 {
        return None;
    }
    if r[..17].iter().fold(0u8, |a, b| a.wrapping_add(*b)) != 0x55 {
        return None;
    }
    let size = usize::from(r[5]);
    (1..=10).contains(&size).then(|| r[6..6 + size].to_vec())
}

fn query(hid: &dyn Hid, feat: &mut dyn Dev, input: &mut dyn Dev, op: u8) -> Option<Vec<u8>> {
    input.flush();
    if !feat.set_feature(&frame(op)) {
        return None;
    }
    let r = report_io::read_until(hid, input, Duration::from_millis(400), |r| {
        payload(r, op).is_some()
    })?;
    payload(&r, op)
}

/// The receiver part of a collection path, so a feature collection finds its reply collection.
fn receiver_key(path: &str) -> String {
    match physical_key(path) {
        k if k.is_empty() => path.to_lowercase(),
        k => k,
    }
}

fn poll_with(hid: &mut dyn Hid, colls: &[Coll]) -> Vec<Device> {
    let ins = find(colls, VID, PID, |c| {
        c.page == 0xFF01 && c.usage == 0x0000 && c.input >= 17
    });
    let feats = find(colls, VID, PID, |c| {
        c.page == 0xFF02 && c.usage == 0x0002 && c.feature >= 17
    });
    for f in feats {
        // Without the reply collection nothing is sent.
        let Some(in_coll) = ins
            .iter()
            .rev()
            .find(|i| receiver_key(&i.path) == receiver_key(&f.path))
        else {
            continue;
        };
        let (Some(mut feat), Some(mut input)) = (hid.open(f), hid.open(in_coll)) else {
            continue;
        };
        // Battery first: the link can drop as the charger goes in.
        let battery = query(hid, feat.as_mut(), input.as_mut(), BATTERY);
        let link = query(hid, feat.as_mut(), input.as_mut(), LINK);
        let Some(battery) = battery.filter(|b| b[0] <= 100) else {
            continue;
        };
        let mut s = device(
            "endgame:1961",
            "Endgame Gear XM2we",
            Kind::Mouse,
            "2.4 GHz receiver",
        );
        s.grace = Duration::from_secs(30 * 60);
        let charging = battery.get(1) == Some(&1);
        if charging || link.is_some_and(|l| l[0] == 1) {
            s.percent = Some(battery[0]);
            s.charging = charging;
        } else {
            s.asleep = true; // the receiver answered, the mouse is not linked: keep the last value
        }
        return vec![s];
    }
    Vec::new()
}

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    report_io::run(api, infos, poll_with)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::report_io::fake::{Fake, Op};

    const FEAT: &str = r"\\?\HID#VID_3367&PID_1961&MI_01&Col02#7&1f0e3a&0&0001#{4d1e55b2-f16f-11cf-88cb-001111000030}";
    const INPUT: &str = r"\\?\HID#VID_3367&PID_1961&MI_01&Col01#7&1f0e3a&0&0000#{4d1e55b2-f16f-11cf-88cb-001111000030}";
    const OTHER: &str = r"\\?\HID#VID_3367&PID_1961&MI_01&Col01#7&99999&0&0000#{4d1e55b2-f16f-11cf-88cb-001111000030}";

    fn reply(op: u8, data: &[u8]) -> Vec<u8> {
        let mut r = vec![0u8; 17];
        r[0] = 0x09;
        r[1] = op;
        r[5] = data.len() as u8;
        r[6..6 + data.len()].copy_from_slice(data);
        let sum = r[..16].iter().fold(0u8, |a, b| a.wrapping_add(*b));
        r[16] = 0x55u8.wrapping_sub(sum);
        r
    }

    fn colls(input_path: &str) -> Vec<Coll> {
        let base = Coll {
            vid: VID,
            pid: PID,
            ..Default::default()
        };
        vec![
            Coll {
                page: 0xFF02,
                usage: 0x0002,
                feature: 17,
                path: FEAT.into(),
                ..base.clone()
            },
            Coll {
                page: 0xFF01,
                usage: 0x0000,
                input: 17,
                path: input_path.into(),
                ..base
            },
        ]
    }

    /// The receiver answers each feature request on the input collection.
    fn receiver(battery: Vec<u8>, link: Vec<u8>) -> Fake {
        let mut pending: Option<Vec<u8>> = None;
        Fake::new(move |c, op| match op {
            Op::SetFeature(w) => {
                assert_eq!(c.path, FEAT);
                assert!(w == frame(BATTERY) || w == frame(LINK));
                pending = Some(if w[1] == BATTERY {
                    battery.clone()
                } else {
                    link.clone()
                });
                Some(vec![])
            }
            Op::Read(_) => {
                assert_eq!(c.path, INPUT);
                pending.take().filter(|r| !r.is_empty())
            }
            Op::Flush => Some(vec![]),
            _ => None,
        })
    }

    #[test]
    fn checksum_and_payload() {
        assert_eq!(frame(BATTERY)[16], 0x55 - 0x0C);
        assert_eq!(
            payload(&reply(BATTERY, &[64, 0]), BATTERY),
            Some(vec![64, 0])
        );
        let mut bad = reply(BATTERY, &[64, 0]);
        bad[16] ^= 1;
        assert_eq!(payload(&bad, BATTERY), None);
        assert_eq!(payload(&reply(LINK, &[1]), BATTERY), None);
    }

    #[test]
    fn linked_mouse_on_battery_and_charging() {
        let mut f = receiver(reply(BATTERY, &[64, 0]), reply(LINK, &[1]));
        let out = poll_with(&mut f, &colls(INPUT));
        let s = &out[0];
        assert_eq!((s.percent, s.charging, s.asleep), (Some(64), false, false));
        assert_eq!(
            (s.key.as_str(), s.name.as_str()),
            ("endgame:1961", "Endgame Gear XM2we")
        );
        // Charging counts even when the link query fails.
        let mut f = receiver(reply(BATTERY, &[90, 1]), vec![]);
        let out = poll_with(&mut f, &colls(INPUT));
        assert_eq!((out[0].percent, out[0].charging), (Some(90), true));
    }

    #[test]
    fn unlinked_is_asleep_and_another_receiver_is_not_used() {
        let mut f = receiver(reply(BATTERY, &[64, 0]), reply(LINK, &[0]));
        let out = poll_with(&mut f, &colls(INPUT));
        assert!(out[0].asleep && out[0].percent.is_none());
        let mut f = receiver(reply(BATTERY, &[64, 0]), reply(LINK, &[1]));
        assert!(poll_with(&mut f, &colls(OTHER)).is_empty());
        assert_eq!(f.opens.get(), 0);
        let mut f = receiver(reply(BATTERY, &[101, 0]), reply(LINK, &[1]));
        assert!(poll_with(&mut f, &colls(INPUT)).is_empty());
    }
}

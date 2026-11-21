//! Cooler Master wireless mice (VID 2516): MM712 (cable 0169) and MM731 (cable 0167), both pairing
//! with the "Cooler Master Mice Dongle" (01B7). Vendor interface 3, usage page FF0A, no report id:
//!   request [46][81][00] -> status [47][81][model] ... [10] percent, [11] charging, [12] link
//!   (0 cable, 1 dongle). Model 50 = MM712, 70 = MM731 on the shared dongle.
//!
//! Ported from BatteryHub (src/drivers/coolermaster.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0, which cites
//! github.com/syntactic-spice/coolermaster-usb-protocol and github.com/riVFerd/MM731-Controller.

use std::time::{Duration, Instant};

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::hid::{self, BUDGET, Coll, Dev, Hid, find, hex4, left, pids};
use crate::registry::Match;

const VID: u16 = 0x2516;
const DONGLE: u16 = 0x01B7;
const MM712_CABLE: u16 = 0x0169;
const MM731_CABLE: u16 = 0x0167;

pub static MATCHES: &[Match] = &[Match {
    vid: VID,
    pids: &[DONGLE, MM712_CABLE, MM731_CABLE],
}];

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    hid::run(api, infos, |hid, colls| poll_with(colls, hid))
}

pub fn model_name(pid: u16, model: u8) -> &'static str {
    if pid == MM712_CABLE || (pid == DONGLE && model == 0x50) {
        "Cooler Master MM712"
    } else if pid == MM731_CABLE || (pid == DONGLE && model == 0x70) {
        "Cooler Master MM731"
    } else {
        "Cooler Master wireless mouse"
    }
}

/// Device from a 47 81 status report.
pub fn parse(pid: u16, r: &[u8]) -> Option<Device> {
    if r.len() < 13 || r[0] != 0x47 || r[1] != 0x81 {
        return None;
    }
    let mut s = Device::new(
        format!("coolermaster:{}", hex4(pid)),
        model_name(pid, r[2]),
        Kind::Mouse,
        if r[12] == 0 {
            "USB cable"
        } else {
            "2.4 GHz receiver"
        },
    );
    s.percent = Some(r[10]).filter(|p| (1..=100).contains(p));
    s.charging = r[11] != 0;
    s.grace = Duration::from_secs(1800);
    Some(s)
}

fn query(d: &mut dyn Dev, end: Instant) -> Option<Vec<u8>> {
    d.flush();
    if !d.write(&[0x00, 0x46, 0x81, 0x00]) {
        return None;
    }
    let end = end.min(Instant::now() + Duration::from_millis(500));
    loop {
        let left = left(end);
        if left.is_zero() {
            return None;
        }
        let r = d.read(left).filter(|r| r.len() >= 13)?;
        if r[0] == 0x47 && r[1] == 0x81 {
            return Some(r);
        }
    }
}

fn poll_with(colls: &[Coll], io: &mut dyn Hid) -> Vec<Device> {
    let end = Instant::now() + BUDGET;
    let mut out: Vec<Device> = Vec::new();
    // Sorted: the cable ids come before the dongle.
    for pid in pids(colls, VID) {
        if ![DONGLE, MM712_CABLE, MM731_CABLE].contains(&pid) {
            continue;
        }
        for c in find(colls, VID, pid, |c| c.iface == 3 && c.page == 0xFF0A) {
            if left(end).is_zero() {
                return out;
            }
            let Some(s) = io
                .open(c)
                .and_then(|mut d| query(d.as_mut(), end))
                .and_then(|r| parse(pid, &r))
            else {
                continue;
            };
            // With the cable and the dongle both plugged in the mouse shows up twice; keep one.
            let mut dup = false;
            for o in out.iter_mut().filter(|o| o.name == s.name) {
                dup = true;
                o.charging |= s.charging;
            }
            if !dup {
                out.push(s);
            }
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::hid::fake::{self, Fake, iface_coll as coll};

    fn status(model: u8, pct: u8, charging: u8, link: u8) -> Vec<u8> {
        let mut r = vec![0u8; 32];
        r[..3].copy_from_slice(&[0x47, 0x81, model]);
        r[10] = pct;
        r[11] = charging;
        r[12] = link;
        r
    }

    #[test]
    fn dongle_names_the_model_from_the_status() {
        let s = parse(DONGLE, &status(0x50, 73, 0, 1)).unwrap();
        assert_eq!(s.name, "Cooler Master MM712");
        assert_eq!((s.percent, s.via.as_str()), (Some(73), "2.4 GHz receiver"));
        assert_eq!(
            parse(DONGLE, &status(0x70, 0, 1, 0)).unwrap().name,
            "Cooler Master MM731"
        );
        assert_eq!(parse(DONGLE, &status(0x70, 0, 1, 0)).unwrap().percent, None);
    }

    #[test]
    fn cable_and_dongle_are_listed_once() {
        let mut f = Fake::with(|s, fc, op| {
            if let Some(q) = op.sent() {
                let path = fc.path.as_str();
                assert_eq!(q, [0, 0x46, 0x81, 0]);
                let cable = path.contains("0169");
                s.push_input(path, status(0x50, 60, u8::from(cable), u8::from(!cable)));
            }
            fake::queued(s, fc, op)
        });
        let out = poll_with(
            &[
                coll(VID, DONGLE, 3, 0xFF0A, 1, 33, 33, 0),
                coll(VID, MM712_CABLE, 3, 0xFF0A, 1, 65, 65, 0),
            ],
            &mut f,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].via, "USB cable");
        assert!(out[0].charging);
    }
}

//! JBL Quantum wireless headsets (VID 0ECB).
//! - HIDV1 (Quantum 350 / 610 / 360 Wireless): feature GET 68 (link, 01 = headset connected) and GET 49
//!   (battery 0-100), one payload byte each. The firmware answers an unimplemented report id with the
//!   previous one, so the reply's first byte must match the id asked for.
//! - HIDV3 (Quantum 520 / 650 / 850 / 950 and X variants), vendor collection FF12: feature 88
//!   [88][00 DD][01 00][01][00][02 00][0D 00] = "get device info" for feature 000D (battery), reply on
//!   input 86: [10] bit 7 = error, [11..12] feature, [15] = bit 7 charging, bits 0..6 percent.
//!
//! Only GETs are sent.
//!
//! Ported from BatteryHub (src/drivers/jbl.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0, which credits JBL
//! QuantumENGINE 2.9.4, sidmax7/QuantumEngine and Tymon3310/jbl-quantum-linux.

use std::time::{Duration, Instant};

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::hid_io::{self, Coll, Io, Open, find};

pub const VID: u16 = 0x0ECB;
const BATTERY_FEATURE: u16 = 0x000D;

/// HIDV3 models: (pid, name, on the dongle).
const MODELS: &[(u16, &str, bool)] = &[
    (0x2125, "JBL Quantum 650 Wireless", true),
    (0x2126, "JBL Quantum 650 Wireless", false),
    (0x213B, "JBL Quantum 650X Wireless", true),
    (0x213D, "JBL Quantum 650X Wireless", false),
    (0x2127, "JBL Quantum 950 Wireless", true),
    (0x2128, "JBL Quantum 950 Wireless", false),
    (0x213E, "JBL Quantum 950X Wireless", true),
    (0x2140, "JBL Quantum 950X Wireless", false),
    (0x218D, "JBL Quantum 520 Wireless", true),
    (0x218E, "JBL Quantum 520 Wireless", false),
    (0x21A7, "JBL Quantum 520X Wireless", true),
    (0x21A9, "JBL Quantum 520X Wireless", false),
    (0x219A, "JBL Quantum 850 Wireless", true),
    (0x219B, "JBL Quantum 850 Wireless", false),
];

/// HIDV1 models: (pid, interface, usage page or 0 = any collection, name).
const HIDV1_MODELS: &[(u16, i32, u16, &str)] = &[
    (0x206B, 3, 0x0001, "JBL Quantum 350 Wireless"),
    (0x205C, 5, 0, "JBL Quantum 610 Wireless"),
    (0x2115, 5, 0x0001, "JBL Quantum 360 Wireless"),
];

pub const PIDS: &[u16] = &[
    0x2125, 0x2126, 0x213B, 0x213D, 0x2127, 0x2128, 0x213E, 0x2140, 0x218D, 0x218E, 0x21A7, 0x21A9,
    0x219A, 0x219B, 0x206B, 0x205C, 0x2115,
];

const BATTERY_REQUEST: [u8; 11] = [
    0x88,
    0x00,
    0xDD,
    0x01,
    0x00,
    0x01,
    0x00,
    0x02,
    0x00,
    BATTERY_FEATURE as u8,
    (BATTERY_FEATURE >> 8) as u8,
];

/// HIDV3 battery reply: Some(Some(info)) = battery byte, Some(None) = error or invalid value
/// (stop), None = not this reply (keep reading).
pub fn hidv3_reply(r: &[u8]) -> Option<Option<u8>> {
    // Only the reply to "get device info" sent to the device itself.
    if r[..6] != [0x86, 0x00, 0xDD, 0x01, 0x00, 0x01] {
        return None;
    }
    if r[10] & 0x80 != 0 {
        return Some(None); // error status (bit 15)
    }
    if r[11..13] != BATTERY_FEATURE.to_le_bytes() || (r[13] == 0 && r[14] == 0) {
        return None;
    }
    Some(Some(r[15]).filter(|i| i & 0x7F <= 100))
}

/// With the headset switched off the dongle does not answer at all, so a timeout means "not there".
fn get_battery(io: &mut dyn Io) -> Option<u8> {
    io.flush();
    if !io.set_feature(&BATTERY_REQUEST) {
        return None;
    }
    let end = Instant::now() + Duration::from_millis(hid_io::MAX_REPLY_MS);
    loop {
        let left = end.saturating_duration_since(Instant::now()).as_millis() as u64;
        if left == 0 {
            return None;
        }
        let r = io.read(left).filter(|r| r.len() >= 16)?;
        if let Some(info) = hidv3_reply(&r) {
            return info;
        }
    }
}

fn poll_hidv1(colls: &[Coll], open: &mut Open<'_>, out: &mut Vec<Device>) {
    for &(pid, iface, page, name) in HIDV1_MODELS {
        for c in find(colls, VID, pid, |c| {
            c.iface == iface && c.feat_len >= 2 && (page == 0 || (c.page == page && c.usage == 0))
        }) {
            let Some(mut io) = open(c) else { continue };
            if io.get_feature_byte(0x68) != Some(1) {
                continue;
            }
            let Some(pct) = io.get_feature_byte(0x49).filter(|p| *p <= 100) else {
                continue;
            };
            let mut d = Device::new(
                format!("jbl:{pid:04X}"),
                name,
                Kind::Headset,
                "2.4 GHz receiver",
            );
            d.percent = Some(pct);
            out.push(d);
            break;
        }
    }
}

pub fn poll_colls(colls: &[Coll], open: &mut Open<'_>) -> Vec<Device> {
    let mut out = Vec::new();
    poll_hidv1(colls, open, &mut out);
    for &(pid, name, dongle) in MODELS {
        for c in find(colls, VID, pid, |c| {
            c.page == 0xFF12 && c.feat_len >= 11 && c.in_len >= 16
        }) {
            let Some(mut io) = open(c) else { continue };
            let Some(info) = get_battery(io.as_mut()) else {
                continue;
            };
            let via = if dongle {
                "2.4 GHz receiver"
            } else {
                "USB cable"
            };
            let mut d = Device::new(format!("jbl:{pid:04X}"), name, Kind::Headset, via);
            d.percent = Some(info & 0x7F);
            d.charging = info & 0x80 != 0;
            out.push(d);
            break;
        }
    }
    out
}

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    hid_io::run(api, infos, poll_colls)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::hid_io::fake::Fake;
    use std::cell::RefCell;

    fn reply(info: u8) -> Vec<u8> {
        let mut r = vec![0u8; 64];
        r[..6].copy_from_slice(&[0x86, 0x00, 0xDD, 0x01, 0x00, 0x01]);
        r[11..16].copy_from_slice(&[0x0D, 0x00, 0x01, 0x00, info]);
        r
    }

    #[test]
    fn hidv3_reply_parsing() {
        assert_eq!(hidv3_reply(&reply(0x80 | 55)), Some(Some(0x80 | 55)));
        assert_eq!(hidv3_reply(&reply(101)), Some(None));
        let mut err = reply(50);
        err[10] = 0x80;
        assert_eq!(hidv3_reply(&err), Some(None));
        let mut other = reply(50);
        other[11] = 0x0E;
        assert_eq!(hidv3_reply(&other), None);
        let mut empty = reply(50);
        empty[13] = 0;
        assert_eq!(hidv3_reply(&empty), None);
    }

    #[test]
    fn quantum_650_on_its_dongle() {
        let mut c = Coll::new(VID, 0x2125, 0xFF12, 1, 64, 64);
        c.feat_len = 64;
        let f = Fake::new();
        f.queue(vec![
            vec![0x86, 0x00, 0xDD, 0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            reply(0x80 | 64),
        ]);
        let opened = RefCell::new(Vec::new());
        let out = poll_colls(&[c], &mut *f.opener(&opened));
        assert_eq!(out[0].key, "jbl:2125");
        assert_eq!(out[0].via, "2.4 GHz receiver");
        assert_eq!((out[0].percent, out[0].charging), (Some(64), true));
        assert_eq!(f.s().set_features, vec![BATTERY_REQUEST.to_vec()]);
    }

    #[test]
    fn hidv1_link_then_level() {
        let mut c = Coll::new(VID, 0x206B, 0x0001, 0, 0, 0);
        c.iface = 3;
        c.feat_len = 64;
        let f = Fake::new();
        f.s().feature_bytes = vec![(0x68, 1), (0x49, 47)];
        let opened = RefCell::new(Vec::new());
        let out = poll_colls(std::slice::from_ref(&c), &mut *f.opener(&opened));
        assert_eq!(out[0].name, "JBL Quantum 350 Wireless");
        assert_eq!(out[0].percent, Some(47));
        // Headset not linked: nothing listed.
        f.s().feature_bytes = vec![(0x68, 0), (0x49, 47)];
        assert!(poll_colls(&[c], &mut *f.opener(&opened)).is_empty());
    }
}

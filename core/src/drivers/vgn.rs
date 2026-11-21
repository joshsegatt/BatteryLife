//! VGN headsets on the "ZHOU" protocol (VID 372E): the VGN OMNISOUND (ZHOUTING) headset on its
//! 2.4 GHz dongle 372E:10BB, control collection 000C:0001 (32-byte input, 64-byte output reports,
//! report id 55). Read battery: output report 55 [65 01] (the collection has no interrupt OUT pipe, so
//! it goes out with HidD_SetOutputReport); the reply input report is [55][65][percent]. No charging
//! bit is known. The wired variant and the Siren V2 are untested and not enabled.
//!
//! Ported from BatteryHub (src/drivers/vgn.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0, which credits VGN's V HUB.

use std::time::Duration;

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::hid::{self, Coll, Hid, find};

pub const VID: u16 = 0x372E;
pub const PID_ZHOU_WIRELESS: u16 = 0x10BB;
const REPORT_ID: u8 = 0x55;
const READ_BATTERY: u8 = 0x65;

/// Battery percent from one input report: numbered ([55][65][%]) or unnumbered ([65][%]).
pub fn parse(r: &[u8]) -> Option<u8> {
    let pct = if r.len() >= 3 && r[0] == REPORT_ID && r[1] == READ_BATTERY {
        r[2]
    } else if r.len() >= 2 && r[0] == READ_BATTERY {
        r[1]
    } else {
        return None;
    };
    (pct <= 100).then_some(pct)
}

fn query(hid: &mut dyn Hid, c: &Coll) -> Option<u8> {
    let mut d = hid.open(c)?;
    d.flush();
    if !d.set_output(&[REPORT_ID, READ_BATTERY, 0x01]) {
        return None;
    }
    let first = d.read(hid::MAX_WAIT)?;
    if let Some(p) = parse(&first) {
        return Some(p);
    }
    // The battery frame can arrive behind other notifications.
    let r = hid::read_until(d.as_mut(), Duration::from_millis(300), |r| {
        parse(r).is_some()
    })?;
    parse(&r)
}

fn poll_with(hid: &mut dyn Hid, colls: &[Coll]) -> Vec<Device> {
    let control = find(colls, VID, PID_ZHOU_WIRELESS, |c| {
        c.page == 0x000C && c.usage == 0x0001 && c.out_len >= 63 && c.in_len >= 3
    });
    for c in control {
        let Some(pct) = query(hid, c) else { continue };
        let name = if c.product.is_empty() {
            "VGN OMNISOUND (ZHOUTING)"
        } else {
            &c.product
        };
        let mut s = Device::new("vgn:zhou", name, Kind::Headset, "2.4 GHz receiver");
        s.percent = Some(pct);
        // The dongle answers while the headset is awake; keep the last value otherwise.
        s.grace = Duration::from_secs(30 * 60);
        return vec![s];
    }
    Vec::new()
}

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    hid::run(api, infos, poll_with)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::hid::fake::{Fake, Op};
    use std::collections::VecDeque;

    /// The control collection of the ZHOUTING dongle.
    fn coll(pid: u16, page: u16, usage: u16, output: u16, input: u16) -> Coll {
        Coll {
            vid: VID,
            pid,
            page,
            usage,
            out_len: output,
            in_len: input,
            path: "vgn-control".into(),
            product: "VGN ZHOU".into(),
            ..Default::default()
        }
    }

    fn control() -> Coll {
        coll(0x10BB, 0x000C, 0x0001, 64, 32)
    }

    /// What the dongle sent back on real hardware.
    fn reply(pct: u8) -> Vec<u8> {
        let mut r = vec![0u8; 32];
        r[0] = 0x55;
        r[1] = 0x65;
        r[2] = pct;
        r[3] = 0x02;
        r
    }

    fn fake(replies: Vec<Vec<u8>>) -> Fake {
        let mut q: VecDeque<Vec<u8>> = replies.into();
        Fake::with(move |_, c, op| match op.clone() {
            Op::SetOutput(w) => {
                assert_eq!(c.path, "vgn-control");
                assert_eq!(w, [0x55, 0x65, 0x01]);
                Some(vec![])
            }
            Op::Read(_) => q.pop_front(),
            Op::Flush => Some(vec![]),
            _ => None,
        })
    }

    fn sends(f: &Fake) -> usize {
        f.count(|o| matches!(o, Op::SetOutput(_)))
    }

    #[test]
    fn reads_the_battery_once() {
        let mut f = fake(vec![reply(90)]);
        let out = poll_with(&mut f, &[control()]);
        assert_eq!(sends(&f), 1);
        let s = &out[0];
        assert_eq!((s.percent, s.charging, s.asleep), (Some(90), false, false));
        assert_eq!((s.name.as_str(), s.key.as_str()), ("VGN ZHOU", "vgn:zhou"));
        assert_eq!(
            (s.kind, s.via.as_str()),
            (Kind::Headset, "2.4 GHz receiver")
        );
        assert_eq!(s.grace, Duration::from_secs(1800));
    }

    #[test]
    fn unnumbered_full_and_empty_replies() {
        let mut unnumbered = vec![0u8; 32];
        unnumbered[0] = 0x65;
        unnumbered[1] = 75;
        assert_eq!(
            poll_with(&mut fake(vec![unnumbered]), &[control()])[0].percent,
            Some(75)
        );
        assert_eq!(
            poll_with(&mut fake(vec![reply(100)]), &[control()])[0].percent,
            Some(100)
        );
        assert_eq!(
            poll_with(&mut fake(vec![reply(0)]), &[control()])[0].percent,
            Some(0)
        );
    }

    #[test]
    fn bad_or_unrelated_replies_and_silence() {
        let volume = {
            let mut v = vec![0u8; 32];
            v[0] = 0x02;
            v
        };
        assert!(poll_with(&mut fake(vec![reply(101)]), &[control()]).is_empty());
        let mut f = fake(vec![volume.clone(), volume.clone()]);
        assert!(poll_with(&mut f, &[control()]).is_empty());
        assert_eq!(sends(&f), 1);
        let mut f = fake(vec![]);
        assert!(poll_with(&mut f, &[control()]).is_empty());
        assert_eq!(sends(&f), 1);
        // The battery frame behind an unrelated notification.
        let mut f = fake(vec![volume, reply(42)]);
        assert_eq!(poll_with(&mut f, &[control()])[0].percent, Some(42));
        assert_eq!(sends(&f), 1);
    }

    #[test]
    fn other_products_and_collections_are_never_opened() {
        for c in [
            coll(0x10BD, 0x000C, 0x0001, 64, 32), // Siren V2: untested, not enabled
            coll(0x10BB, 0xFF02, 0x0002, 64, 32), // vendor collection
            coll(0x10BB, 0x000C, 0x0001, 0, 3),   // media keys, no output report
            coll(0x10BB, 0x000C, 0x0001, 64, 2),  // reply cannot hold the two bytes we read
            coll(0x10BB, 0x000C, 0x0001, 10, 32), // output too short for the request
        ] {
            let mut f = fake(vec![]);
            assert!(poll_with(&mut f, &[c]).is_empty());
            assert_eq!(f.s().opened.len(), 0);
        }
    }
}

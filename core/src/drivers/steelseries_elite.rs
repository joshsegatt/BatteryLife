//! SteelSeries Arctis Nova Elite base station (1038:2244), without SteelSeries GG. Sends the status
//! request 01 B0 (64-byte output report) to the station's interface-3 vendor collection (FFC0 first)
//! and reads either the direct reply 01 B0 ([6] level, [14] power: 01 headset off / 02 on the cable,
//! [15] charging 02) or the pushed frames 07 B7 <level> <spare> <charging 02> and 07 B5 <?> <bt>
//! <power>, on any of the station's collections.
//!
//! Ported from BatteryHub (src/drivers/steelseries_elite.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0, which credits HaloBattery,
//! elegos/Linux-Arctis-Manager and loteran/Arctis-Sound-Manager.

use std::time::{Duration, Instant};

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::hid::{self, Coll, Dev, Hid, find};

pub const VID: u16 = 0x1038;
pub const PID: u16 = 0x2244;

/// What the station has said so far.
#[derive(Debug, Default, PartialEq)]
pub struct Status {
    pub level: Option<u8>,
    pub power: Option<u8>,
    pub charging: bool,
}

impl Status {
    /// Takes one input report into account (direct reply or pushed frame).
    pub fn take(&mut self, r: &[u8]) {
        if r.len() < 5 {
            return;
        }
        if r[0] == 0x01 && r[1] == 0xB0 && r.len() > 15 {
            if r[6] <= 100 {
                self.level = Some(r[6]);
            }
            self.charging = r[15] == 0x02;
            self.power = Some(r[14]);
        } else if r[0] == 0x07 && r[1] == 0xB7 && r[2] <= 100 {
            self.level = Some(r[2]);
            self.charging = r[4] == 0x02;
        } else if r[0] == 0x07 && r[1] == 0xB5 {
            self.power = Some(r[4]);
        }
    }

    fn headset_off(&self) -> bool {
        self.power == Some(0x01)
    }

    /// The headset, or None when it is off or no level arrived.
    pub fn device(&self) -> Option<Device> {
        let level = self.level.filter(|_| !self.headset_off())?;
        let mut d = Device::new(
            "steelseries:2244",
            "SteelSeries Arctis Nova Elite",
            Kind::Headset,
            "Base station",
        );
        d.percent = Some(level);
        d.charging = self.charging || self.power == Some(0x02);
        Some(d)
    }
}

pub fn poll_colls(colls: &[Coll], hid: &mut dyn Hid) -> Vec<Device> {
    let mut colls = find(colls, VID, PID, |c| {
        c.iface == 3 && (c.in_len > 0 || c.out_len > 0)
    });
    // FFC0 first, then other vendor pages (the station splits interface 3 into two collections).
    colls.sort_by_key(|c| match c.page {
        0xFFC0 => 0,
        p if p >= 0xFF00 => 1,
        _ => 2,
    });
    let mut handles: Vec<(Box<dyn Dev + '_>, &Coll)> = Vec::new();
    for c in colls {
        if let Some(mut io) = hid.open(c) {
            io.flush();
            handles.push((io, c));
        }
    }
    let mut req = [0u8; 64];
    req[..2].copy_from_slice(&[0x01, 0xB0]);
    // A collection without report 01 refuses it.
    let sent = handles
        .iter_mut()
        .filter(|(_, c)| c.page >= 0xFF00 && c.out_len > 0)
        .any(|(io, _)| io.write(&req));
    if !sent {
        return Vec::new();
    }
    let mut st = Status::default();
    let end = Instant::now() + hid::MAX_WAIT;
    while Instant::now() < end && (st.level.is_none() || st.power.is_none()) {
        for (io, _) in handles.iter_mut() {
            if let Some(r) = io.read(Duration::from_millis(30)) {
                st.take(&r);
            }
        }
        if st.headset_off() {
            break;
        }
    }
    st.device().into_iter().collect()
}

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    hid::run(api, infos, |hid, colls| poll_colls(colls, hid))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::hid::fake::Fake;

    fn direct(level: u8, power: u8, charging: u8) -> Vec<u8> {
        let mut r = vec![0u8; 64];
        r[..2].copy_from_slice(&[0x01, 0xB0]);
        r[6] = level;
        r[14] = power;
        r[15] = charging;
        r
    }

    #[test]
    fn direct_reply_and_pushed_frames() {
        let mut s = Status::default();
        s.take(&direct(70, 0x00, 0x00));
        assert_eq!(s.device().unwrap().percent, Some(70));
        assert!(!s.device().unwrap().charging);

        let mut s = Status::default();
        s.take(&[0x07, 0xB7, 55, 0, 0x02]);
        s.take(&[0x07, 0xB5, 0, 0, 0x00]);
        let d = s.device().unwrap();
        assert_eq!((d.percent, d.charging), (Some(55), true));

        // On the cable (power 02) counts as charging; headset off lists nothing.
        let mut s = Status::default();
        s.take(&direct(40, 0x02, 0x00));
        assert!(s.device().unwrap().charging);
        let mut s = Status::default();
        s.take(&direct(40, 0x01, 0x00));
        assert!(s.device().is_none());
        // Short or out-of-range frames are ignored.
        let mut s = Status::default();
        s.take(&[0x07, 0xB7, 101, 0, 0]);
        s.take(&[0x01, 0xB0, 0, 0]);
        assert_eq!(s, Status::default());
    }

    #[test]
    fn asks_the_ffc0_collection_first() {
        let mut ff00 = Coll::new(VID, PID, 0xFF00, 1, 64, 64);
        ff00.iface = 3;
        let mut ffc0 = Coll::new(VID, PID, 0xFFC0, 1, 64, 64);
        ffc0.iface = 3;
        let mut other_iface = Coll::new(VID, PID, 0xFFC0, 1, 64, 64);
        other_iface.iface = 4;
        let f = Fake::replies(|w| {
            assert_eq!(w[..2], [0x01, 0xB0]);
            assert_eq!(w.len(), 64);
            vec![direct(86, 0x00, 0x00)]
        });
        let out = poll_colls(&[ff00, ffc0.clone(), other_iface], &mut f.clone());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].key, "steelseries:2244");
        assert_eq!(out[0].percent, Some(86));
        assert_eq!(f.s().opened.len(), 2);
        assert_eq!(f.s().opened[0], ffc0.path); // the FFC0 collection
        assert_eq!(f.s().writes().len(), 1);
    }
}

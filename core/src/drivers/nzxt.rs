//! NZXT Lift Elite Wireless mouse on its dongle (1E71:2101) or USB cable (1E71:2129), without NZXT
//! CAM. 64-byte reports on the vendor collection FFCA:0001:
//!   request: 4E 02 81 00 B0 ...   (02 = the mouse, 81 = read a property, B0 = telemetry)
//!   reply:   4E 02 97 ...; bytes 17-18 = percent (LE), bytes 8-9 = cell mV (BE), byte 22 bit 0 =
//!            on the charging cable. The dongle first acks with 4E E5, which is skipped.
//! Only this read is sent. While NZXT CAM is open the dongle stops answering; the last value is kept.
//!
//! Ported from BatteryHub (src/drivers/nzxt.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0, which credits HaloBattery
//! (providers/nzxt.py).

use std::time::Duration;

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::hid_io::{self, Coll, Io, Open, find};

pub const VID: u16 = 0x1E71;
pub const RECEIVER: u16 = 0x2101;
pub const CABLE: u16 = 0x2129;

#[derive(Debug, PartialEq)]
pub struct Telemetry {
    pub percent: u8,
    pub mv: u16,
    pub charging: bool,
}

/// Some(Ok) = telemetry, Some(Err) = a reply that is not a percentage (refused), None = other report.
pub fn parse(r: &[u8]) -> Option<Result<Telemetry, ()>> {
    if r.len() < 27 || r[0] != 0x4E || r[1] == 0xE5 || r[1] != 0x02 || r[2] != 0x97 {
        return None;
    }
    let level = u16::from_le_bytes([r[17], r[18]]);
    if level > 100 {
        return Some(Err(()));
    }
    Some(Ok(Telemetry {
        percent: level as u8,
        mv: u16::from_be_bytes([r[8], r[9]]),
        charging: r[22] & 0x01 != 0,
    }))
}

fn read_telemetry(io: &mut dyn Io) -> Option<Telemetry> {
    io.flush();
    let mut req = [0u8; 65]; // report id 0, then the 64-byte packet
    req[1..6].copy_from_slice(&[0x4E, 0x02, 0x81, 0x00, 0xB0]);
    if !io.write(&req) {
        return None;
    }
    for _ in 0..10 {
        let r = io.read(150)?;
        if let Some(t) = parse(&r) {
            return t.ok();
        }
    }
    None
}

pub fn poll_colls(colls: &[Coll], open: &mut Open<'_>) -> Vec<Device> {
    // One entry for the mouse on both ids; the cable reading wins (it is the one charging).
    let mut any = false;
    let mut reading = None;
    for pid in [CABLE, RECEIVER] {
        let cs = find(colls, VID, pid, |c| c.page == 0xFFCA && c.usage == 0x0001);
        any |= !cs.is_empty();
        for c in cs {
            if reading.is_some() {
                break;
            }
            reading = open(c).and_then(|mut io| read_telemetry(io.as_mut()));
        }
    }
    if !any {
        return Vec::new();
    }
    let mut d = Device::new(
        "nzxt:2101",
        "NZXT Lift Elite",
        Kind::Mouse,
        "2.4 GHz receiver",
    );
    d.grace = Duration::from_secs(1800);
    match reading {
        Some(t) => {
            d.percent = Some(t.percent);
            d.charging = t.charging;
            d.millivolts = (3000..=4500).contains(&t.mv).then_some(t.mv);
        }
        // Mouse off, or NZXT CAM holds the dongle: keep the last value.
        None => d.asleep = true,
    }
    vec![d]
}

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    hid_io::run(api, infos, poll_colls)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::hid_io::fake::Fake;
    use std::cell::RefCell;

    fn telemetry(level: u16, mv: u16, cable: bool) -> Vec<u8> {
        let mut r = vec![0u8; 64];
        r[..3].copy_from_slice(&[0x4E, 0x02, 0x97]);
        r[8..10].copy_from_slice(&mv.to_be_bytes());
        r[17..19].copy_from_slice(&level.to_le_bytes());
        r[22] = u8::from(cable);
        r
    }

    #[test]
    fn parse_skips_ack_and_refuses_non_percent() {
        let mut ack = vec![0u8; 64];
        ack[..2].copy_from_slice(&[0x4E, 0xE5]);
        assert_eq!(parse(&ack), None);
        assert_eq!(
            parse(&telemetry(76, 3950, true)),
            Some(Ok(Telemetry {
                percent: 76,
                mv: 3950,
                charging: true
            }))
        );
        assert_eq!(parse(&telemetry(300, 3950, false)), Some(Err(())));
    }

    #[test]
    fn receiver_reading_after_the_ack() {
        let mut ack = vec![0u8; 64];
        ack[..2].copy_from_slice(&[0x4E, 0xE5]);
        let f = Fake::new();
        f.queue(vec![ack, telemetry(55, 3800, false)]);
        let opened = RefCell::new(Vec::new());
        let c = Coll::new(VID, RECEIVER, 0xFFCA, 1, 65, 65);
        let out = poll_colls(&[c], &mut *f.opener(&opened));
        assert_eq!(out[0].key, "nzxt:2101");
        assert_eq!(out[0].kind, Kind::Mouse);
        assert_eq!((out[0].percent, out[0].millivolts), (Some(55), Some(3800)));
        let w = f.s().writes[0].clone();
        assert_eq!((w.len(), &w[..6]), (65, &[0, 0x4E, 2, 0x81, 0, 0xB0][..]));
    }

    #[test]
    fn silent_dongle_keeps_the_last_value() {
        let f = Fake::new();
        let opened = RefCell::new(Vec::new());
        let c = Coll::new(VID, RECEIVER, 0xFFCA, 1, 65, 65);
        let out = poll_colls(&[c], &mut *f.opener(&opened));
        assert_eq!((out[0].percent, out[0].asleep), (None, true));
    }
}

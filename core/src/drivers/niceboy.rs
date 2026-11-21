//! Niceboy MW400 wireless mouse on its 2.4 GHz "Wireless Receiver" (248A:FA02).
//!
//! Interface 0, the vendor collection with report 05 (usage 0001:0000, 33 bytes in and out).
//! Request [05][24][FA][48]; reply [05][xx][FA][48][lo][hi][charging]: raw = lo | hi << 8,
//! mV = raw * 990 / 240, percent from the vendor app's table in 10 % steps. All zeros: mouse not linked.
//! The 248A vendor id is shared by other Telink receivers, so only this product id is matched.
//!
//! Ported from BatteryHub (src/drivers/niceboy.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0.

use std::time::{Duration, Instant};

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::hidport::{
    BUDGET, Coll, Opener, RECEIVER, RECEIVER_GRACE, colls, hid_opener, wait_for,
};

pub const VID: u16 = 0x248A;
const PID: u16 = 0xFA02;
pub const PIDS: &[u16] = &[PID];

/// The vendor app's table: >= 4200 mV 100 %, 4100 90 %, ... 3200 20 %, else 10 %.
pub fn percent_from_millivolts(mv: u32) -> u8 {
    const MV: [u32; 9] = [4200, 4100, 4000, 3900, 3800, 3700, 3500, 3400, 3200];
    const PCT: [u8; 9] = [100, 90, 80, 70, 60, 50, 40, 30, 20];
    MV.iter()
        .zip(PCT)
        .find(|(v, _)| mv >= **v)
        .map_or(10, |(_, p)| p)
}

fn is_battery_reply(r: &[u8]) -> bool {
    r.len() >= 7 && r[0] == 0x05 && r[2] == 0xFA && r[3] == 0x48
}

pub fn poll_with(colls: &[Coll], open: &mut Opener) -> Vec<Device> {
    for c in colls.iter().filter(|c| {
        c.is(VID, PID)
            && c.iface == 0
            && c.page == 1
            && c.usage == 0
            && c.in_len == 33
            && c.out_len == 33
    }) {
        let Some(mut port) = open(c) else { continue };
        port.flush();
        if !port.write(&[0x05, 0x24, 0xFA, 0x48]) {
            continue;
        }
        let Some(r) = wait_for(port.as_mut(), Duration::from_millis(500), is_battery_reply) else {
            continue;
        };
        let mut d = Device::new(
            format!("niceboy:{PID:04X}"),
            "Niceboy MW400",
            Kind::Mouse,
            RECEIVER,
        );
        d.grace = RECEIVER_GRACE;
        if r[4] | r[5] | r[6] == 0 {
            d.asleep = true; // receiver answered, mouse asleep or off: keep the last level
        } else {
            let mv = u32::from(u16::from_le_bytes([r[4], r[5]])) * 990 / 240;
            d.millivolts = Some(mv as u16);
            d.percent = Some(percent_from_millivolts(mv));
            d.coarse = true; // 10 % steps
            d.charging = r[6] != 0;
        }
        return vec![d];
    }
    Vec::new()
}

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    poll_with(&colls(infos), &mut hid_opener(api, Instant::now() + BUDGET))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::hidport::fake::{self, coll};

    fn report(head: &[u8]) -> Vec<u8> {
        let mut r = head.to_vec();
        r.resize(33, 0);
        r
    }

    /// raw 0x03EA = 1002 -> 4133 mV -> 90 %.
    fn captured() -> Vec<u8> {
        report(&[
            0x05, 0x49, 0xFA, 0x48, 0xEA, 0x03, 0x00, 0x00, 0x01, 0xFB, 0x05,
        ])
    }

    fn with_raw(raw: u16, charging: u8) -> Vec<u8> {
        let mut r = captured();
        r[4..6].copy_from_slice(&raw.to_le_bytes());
        r[6] = charging;
        r
    }

    fn receiver() -> Coll {
        let mut c = coll(VID, PID, 1, 0, 33);
        c.iface = 0;
        c
    }

    fn level(reply: Vec<u8>) -> Device {
        let s = fake::per_write(vec![vec![reply]]);
        let out = poll_with(&[receiver()], &mut fake::opener(&s));
        assert_eq!(out.len(), 1);
        out.into_iter().next().unwrap()
    }

    #[test]
    fn captured_replies() {
        let s = fake::per_write(vec![vec![captured()]]);
        let d = &poll_with(&[receiver()], &mut fake::opener(&s))[0];
        assert_eq!(s.borrow().writes, vec![vec![0x05, 0x24, 0xFA, 0x48]]);
        assert_eq!(
            (d.percent, d.millivolts, d.charging),
            (Some(90), Some(4133), false)
        );
        assert_eq!(
            (d.key.as_str(), d.name.as_str(), d.kind),
            ("niceboy:FA02", "Niceboy MW400", Kind::Mouse)
        );
        let d2 = level(report(&[
            0x05, 0x48, 0xFA, 0x48, 0xE9, 0x03, 0, 0, 0x01, 0xFB, 0x05,
        ]));
        assert_eq!((d2.percent, d2.millivolts), (Some(90), Some(4129)));
        let d3 = level(report(&[
            0x05, 0x57, 0xFA, 0x48, 0xEC, 0x03, 0, 0, 0x01, 0xFB, 0x05,
        ]));
        assert_eq!((d3.percent, d3.millivolts), (Some(90), Some(4141)));
    }

    #[test]
    fn vendor_voltage_table_boundaries() {
        for (raw, pct, mv) in [
            (1019, 100, 4203),
            (1018, 90, 4199),
            (994, 90, 4100),
            (993, 80, 4096),
            (970, 80, 4001),
            (969, 70, 3997),
            (946, 70, 3902),
            (945, 60, 3898),
            (922, 60, 3803),
            (921, 50, 3799),
            (897, 50, 3700),
            (896, 40, 3696),
            (849, 40, 3502),
            (848, 30, 3498),
            (825, 30, 3403),
            (824, 20, 3399),
            (776, 20, 3201),
            (775, 10, 3196),
            (1, 10, 4),
        ] {
            let d = level(with_raw(raw, 0));
            assert_eq!(
                (d.percent, d.millivolts),
                (Some(pct), Some(mv)),
                "raw {raw}"
            );
        }
        let d = level(with_raw(1030, 1));
        assert_eq!(
            (d.percent, d.millivolts, d.charging),
            (Some(100), Some(4248), true)
        );
        let d = level(with_raw(900, 0x80));
        assert_eq!(
            (d.percent, d.millivolts, d.charging),
            (Some(50), Some(3712), true)
        );
    }

    #[test]
    fn zeros_mean_asleep() {
        let d = level(with_raw(0, 0));
        assert!(d.asleep && d.percent.is_none() && d.millivolts.is_none() && !d.charging);
        assert!(d.grace > Duration::ZERO);
    }

    #[test]
    fn other_report_05_traffic_is_skipped() {
        let mut other_type = captured();
        other_type[3] = 0x49;
        let mut other_echo = captured();
        other_echo[2] = 0xFB;
        let mut other_report = captured();
        other_report[0] = 0x04;
        let s = fake::per_write(vec![vec![
            other_type,
            other_echo,
            other_report,
            vec![0x05, 0x49, 0xFA, 0x48],
            captured(),
        ]]);
        let out = poll_with(&[receiver()], &mut fake::opener(&s));
        assert_eq!((out[0].percent, s.borrow().reads), (Some(90), 5));
    }

    #[test]
    fn strict_matching_and_failures() {
        for field in 0..7 {
            let mut c = receiver();
            match field {
                0 => c.vid = 0x249A,
                1 => c.pid = 0xFF4C,
                2 => c.iface = 2,
                3 => c.page = 0xFF00,
                4 => c.usage = 0x0002,
                5 => c.in_len = 32,
                _ => c.out_len = 32,
            }
            let s = fake::per_write(vec![vec![captured()]]);
            assert!(poll_with(&[c], &mut fake::opener(&s)).is_empty());
            assert!(s.borrow().opened.is_empty());
        }
        let s = fake::script();
        s.borrow_mut().fail_open = true;
        assert!(poll_with(&[receiver()], &mut fake::opener(&s)).is_empty());
        assert!(s.borrow().writes.is_empty());
        let s = fake::per_write(vec![vec![captured()]]);
        s.borrow_mut().fail_write = true;
        assert!(poll_with(&[receiver()], &mut fake::opener(&s)).is_empty());
        assert_eq!(s.borrow().reads, 0);
    }
}

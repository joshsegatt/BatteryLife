//! BenQ ZOWIE wireless mice (VID 04A5). Only the read-only battery commands below are ever sent
//! (other EC-CW commands reboot the receiver and unpair the mouse).
//!  * EC-CW series: receiver 8006, cable 8005. Unnumbered 64-byte output report [A5 03]; reply
//!    [mV lo][mV hi][percent]; mV = 0 when the mouse is not linked. The Enhanced Receiver declares this
//!    collection on usage page 00FF instead of FFxx.
//!  * DW series (receiver 800A) and U2 (receiver 8008): output report 08 [B4 15] on page FF03, reply
//!    input report 09 on page FF04: mV = ([1] * 256 + [2]) * 10, [4] == C8.
//!
//! On the charging dock nothing new shows up on USB, so charging is inferred from the voltage trend:
//! a rise of 50 mV above the lowest reading means it went on the dock, a drop of 20 mV below the
//! highest means it came off.
//!
//! Ported from BatteryHub (src/drivers/zowie.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0, which credits zowie-battery
//! and zowie-battery-tray.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind, interpolate};
use crate::drivers::hid::{self, Coll, Hid, MAX_WAIT, find, products};

pub const VID: u16 = 0x04A5;
const CW_CABLE: u16 = 0x8005;
const CW_RECEIVER: u16 = 0x8006;
const DW_RECEIVER: u16 = 0x800A;
const U2_RECEIVER: u16 = 0x8008;
const RECEIVER_GRACE: Duration = Duration::from_secs(30 * 60);

/// Voltage to percent, anchored on the mouse's own LED thresholds (10 / 25 / 50 %).
pub fn zowie_percent(mv: u16) -> u8 {
    const V: [i32; 6] = [4200, 4000, 3890, 3760, 3680, 3500];
    const P: [i32; 6] = [100, 75, 50, 25, 10, 0];
    interpolate(mv.into(), &V, &P)
}

/// Charging guess from the voltage trend of one receiver.
#[derive(Debug, Default, Clone, Copy)]
pub struct ChargeTrend {
    has: bool,
    charging: bool,
    peak: u16,
    trough: u16,
}

impl ChargeTrend {
    pub fn update(&mut self, mv: u16) -> bool {
        if !self.has {
            self.has = true;
            self.charging = mv >= 4250; // a resting Li-ion cell never sits above ~4.2 V
            self.peak = mv;
            self.trough = mv;
        } else if self.charging {
            self.peak = self.peak.max(mv);
            if mv + 20 <= self.peak {
                self.charging = false;
                self.trough = mv;
            }
        } else {
            self.trough = self.trough.min(mv);
            if mv >= self.trough + 50 {
                self.charging = true;
                self.peak = mv;
            }
        }
        self.charging
    }
}

static TREND: Mutex<Option<HashMap<u16, ChargeTrend>>> = Mutex::new(None);

/// EC-CW reply: Some(None) = not linked, Some(Some((mV, %))) = a reading, None = not a battery reply.
pub fn parse_cw(r: &[u8]) -> Option<Option<(u16, u8)>> {
    if r.len() < 3 {
        return None;
    }
    let mv = u16::from_le_bytes([r[0], r[1]]);
    if mv == 0 {
        return Some(None);
    }
    if !(3000..=4500).contains(&mv) {
        return None;
    }
    let pct = if (1..=100).contains(&r[2]) {
        r[2]
    } else {
        zowie_percent(mv)
    };
    Some(Some((mv, pct)))
}

/// DW / U2 reply (input report 09): millivolts, 0 when the mouse is not linked.
pub fn parse_dw(r: &[u8]) -> Option<u16> {
    if r.len() < 5 || r[0] != 0x09 || r[1] == 0xE6 || r[4] != 0xC8 {
        return None;
    }
    Some((u16::from(r[1]) * 256 + u16::from(r[2])) * 10)
}

fn poll_cw(hid: &mut dyn Hid, colls: &[Coll], pid: u16) -> Option<Device> {
    let cs = find(colls, VID, pid, |c| {
        (c.page >= 0xFF00 || c.page == 0x00FF) && c.out_len == 65
    });
    for c in cs {
        let Some(mut d) = hid.open(c) else { continue };
        d.flush();
        if !d.write(&[0x00, 0xA5, 0x03]) {
            continue;
        }
        let Some(reading) = d.read(MAX_WAIT).and_then(|r| parse_cw(&r)) else {
            continue;
        };
        let cable = pid == CW_CABLE;
        let (key, via) = (format!("zowie:{pid:04X}"), via(cable));
        // Not linked: the mouse is off or out of range, hidden at once.
        let Some((mv, pct)) = reading else {
            return Some(Device::switched_off(key, "ZOWIE EC-CW", Kind::Mouse, via));
        };
        let mut s = Device::new(key, "ZOWIE EC-CW", Kind::Mouse, via);
        s.charging = cable;
        s.millivolts = Some(mv);
        s.percent = Some(pct);
        if !cable {
            s.grace = RECEIVER_GRACE;
        }
        return Some(s);
    }
    None
}

fn via(cable: bool) -> &'static str {
    if cable {
        "USB cable"
    } else {
        "2.4 GHz receiver"
    }
}

fn poll_dw(hid: &mut dyn Hid, colls: &[Coll], pid: u16) -> Option<Device> {
    let cmd = find(colls, VID, pid, |c| c.page == 0xFF03 && c.out_len == 16);
    let rep = find(colls, VID, pid, |c| c.page == 0xFF04 && c.in_len == 16);
    let (cmd, rep) = (cmd.first()?, rep.first()?);
    let mut input = hid.open(rep)?; // open the reply channel first
    let mut out = hid.open(cmd)?;
    input.flush();
    if !out.write(&[0x08, 0xB4, 0x15]) {
        return None;
    }
    let end = hid.now() + MAX_WAIT;
    let r = loop {
        let left = end.saturating_sub(hid.now());
        if left.is_zero() {
            return None;
        }
        let r = input.read(left).filter(|r| r.len() >= 5)?;
        if r[0] == 0x09 {
            break r;
        }
    };
    let mv = parse_dw(&r)?;
    let cable = pid != DW_RECEIVER && pid != U2_RECEIVER;
    let name = if pid == U2_RECEIVER {
        "ZOWIE U2" // its receiver only reports "BenQ ZOWIE Gaming Mouse"
    } else if cmd.product.is_empty() || cmd.product.contains("Receiver") {
        "ZOWIE DW mouse"
    } else {
        &cmd.product
    };
    let key = format!("zowie:{pid:04X}");
    if mv == 0 {
        // Not linked: the mouse is off or out of range, hidden at once.
        return Some(Device::switched_off(key, name, Kind::Mouse, via(cable)));
    }
    let mut s = Device::new(key, name, Kind::Mouse, via(cable));
    s.millivolts = Some(mv);
    s.percent = Some(zowie_percent(mv));
    s.charging = cable || {
        let mut t = TREND.lock().unwrap_or_else(|e| e.into_inner());
        t.get_or_insert_with(HashMap::new)
            .entry(pid)
            .or_default()
            .update(mv)
    };
    if !cable {
        s.grace = RECEIVER_GRACE;
    }
    Some(s)
}

fn poll_with(hid: &mut dyn Hid, colls: &[Coll]) -> Vec<Device> {
    let mut out = Vec::new();
    for (_, pid) in products(colls).into_iter().filter(|p| p.0 == VID) {
        if pid == CW_CABLE || pid == CW_RECEIVER {
            out.extend(poll_cw(hid, colls, pid));
        } else {
            // Only acts on devices with the DW command / reply collections.
            out.extend(poll_dw(hid, colls, pid));
        }
    }
    out
}

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    hid::run(api, infos, poll_with)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::hid::fake::{Fake, Op};
    use std::collections::VecDeque;

    #[test]
    fn voltage_curve_follows_the_led_thresholds() {
        assert_eq!(zowie_percent(4250), 100);
        assert_eq!(zowie_percent(3890), 50);
        assert_eq!(zowie_percent(3720), 17);
        assert_eq!(zowie_percent(3400), 0);
    }

    #[test]
    fn charge_trend_from_voltage_steps() {
        let mut t = ChargeTrend::default();
        assert!(!t.update(3900));
        assert!(!t.update(3880));
        assert!(t.update(3930), "50 mV above the trough: on the dock");
        assert!(t.update(4100));
        assert!(t.update(4085));
        assert!(!t.update(4080), "20 mV below the peak: off the dock");
        assert!(ChargeTrend::default().update(4260));
    }

    fn fake(replies: Vec<Vec<u8>>) -> Fake {
        let mut q: VecDeque<Vec<u8>> = replies.into();
        Fake::with(move |_, _, op| match op.clone() {
            Op::Write(_) => Some(vec![]),
            Op::Read(_) => q.pop_front(),
            Op::Flush => Some(vec![]),
            _ => None,
        })
    }

    #[test]
    fn ec_cw_receiver() {
        let c = Coll {
            vid: VID,
            pid: CW_RECEIVER,
            page: 0x00FF,
            out_len: 65,
            ..Default::default()
        };
        let mut f = fake(vec![vec![0x2E, 0x0F, 64]]); // 3886 mV, 64%
        let out = poll_with(&mut f, std::slice::from_ref(&c));
        let w: Vec<Op> = f.s().log.iter().map(|(_, o)| o.clone()).collect();
        assert!(w.contains(&Op::Write(vec![0x00, 0xA5, 0x03])));
        let s = &out[0];
        assert_eq!(
            (s.percent, s.millivolts, s.charging),
            (Some(64), Some(3886), false)
        );
        assert_eq!(
            (s.name.as_str(), s.key.as_str()),
            ("ZOWIE EC-CW", "zowie:8006")
        );
        // No percent byte: the voltage curve. Not linked: switched off, same key. Implausible
        // voltage: nothing.
        let out = poll_with(
            &mut fake(vec![vec![0x32, 0x0F, 0]]),
            std::slice::from_ref(&c),
        );
        assert_eq!(out[0].percent, Some(zowie_percent(3890)));
        let out = poll_with(&mut fake(vec![vec![0, 0, 0]]), std::slice::from_ref(&c));
        assert_eq!(out.len(), 1);
        assert_eq!((out[0].key.as_str(), out[0].off), ("zowie:8006", true));
        assert_eq!(out[0].percent, None);
        assert!(poll_with(&mut fake(vec![vec![0x10, 0x27, 50]]), &[c]).is_empty());
    }

    #[test]
    fn dw_receiver() {
        let colls = [
            Coll {
                vid: VID,
                pid: DW_RECEIVER,
                page: 0xFF03,
                out_len: 16,
                product: "ZOWIE Wireless Receiver".into(),
                ..Default::default()
            },
            Coll {
                vid: VID,
                pid: DW_RECEIVER,
                page: 0xFF04,
                in_len: 16,
                ..Default::default()
            },
        ];
        // An unrelated report, then 09 with 389 * 10 mV.
        let mut f = fake(vec![
            vec![0x02, 0, 0, 0, 0],
            vec![0x09, 0x01, 0x85, 0x00, 0xC8],
        ]);
        let out = poll_with(&mut f, &colls);
        let s = &out[0];
        assert_eq!((s.millivolts, s.percent), (Some(3890), Some(50)));
        assert_eq!(
            (s.name.as_str(), s.via.as_str()),
            ("ZOWIE DW mouse", "2.4 GHz receiver")
        );
        // 0 mV: the mouse is off, listed as such under the same key.
        let mut f = fake(vec![vec![0x09, 0, 0, 0x00, 0xC8]]);
        let out = poll_with(&mut f, &colls);
        assert_eq!(out.len(), 1);
        assert_eq!((out[0].key.as_str(), out[0].off), ("zowie:800A", true));
        assert_eq!(out[0].name, "ZOWIE DW mouse");
        assert_eq!(parse_dw(&[0x09, 0xE6, 0, 0, 0xC8]), None);
        assert_eq!(parse_dw(&[0x09, 0x01, 0x85, 0, 0xC7]), None);
        // A BenQ device without the DW collections is never opened.
        let mut f = fake(vec![]);
        let other = Coll {
            vid: VID,
            pid: 0x1234,
            page: 0xFF03,
            out_len: 8,
            ..Default::default()
        };
        assert!(poll_with(&mut f, &[other]).is_empty());
        assert_eq!(f.s().opened.len(), 0);
    }
}

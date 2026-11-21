//! MSI wireless mice on their 2.4 GHz dongles (VID 0DB0), interface 1:
//!  * VERSA 300 ELITE WIRELESS (9E22, FF0D:00B0) and CLUTCH GM31 (receiver 0D3C, cable 0D3B,
//!    FF00:0001): output [00][0F 20 00 ...] -> input [0F][20] .. [14] bit 7 charging, bits 0-6
//!    percent. MSI Center's "41 80" remote-control command is deliberately never sent.
//!  * CLUTCH GM41 (receiver 0D4C, cable 0D4B, FF10:0006) has no query: it pushes
//!    [A0][raw][07][flags][94 00 94] while the mouse moves; raw maps linearly to an approximate %.
//!
//! A level of 0 while not charging is shown as asleep (last value kept).
//!
//! Ported from BatteryHub (src/drivers/msi.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0 (protocol from MSI Center's
//! Gaming Gear module; GM41 from DARL0K/Wireless-Device-Battery-Monitor PR #1 and
//! liamgibsonn/mouse-protocol PR #1). The query reply wait is 500 ms (BatteryHub: 1 s); the GM41
//! push wait keeps BatteryHub's 2 s, inside the driver's overall budget.

use std::time::{Duration, Instant};

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::hid_dev::{
    BUDGET, Coll, Dev, HidIo, Io, colls, find, hex4, ms_left, read_until,
};
use crate::registry::Match;

const VID: u16 = 0x0DB0;

struct Model {
    pid: u16,
    cable: bool,
    /// false: the dongle only pushes (GM41).
    query: bool,
    page: u16,
    usage: u16,
    name: &'static str,
}

const MODELS: [Model; 5] = [
    Model {
        pid: 0x9E22,
        cable: false,
        query: true,
        page: 0xFF0D,
        usage: 0x00B0,
        name: "MSI VERSA 300 ELITE WIRELESS",
    },
    Model {
        pid: 0x0D3C,
        cable: false,
        query: true,
        page: 0xFF00,
        usage: 0x0001,
        name: "MSI Clutch GM31 Lightweight Wireless",
    },
    Model {
        pid: 0x0D3B,
        cable: true,
        query: true,
        page: 0xFF00,
        usage: 0x0001,
        name: "MSI Clutch GM31 Lightweight Wireless",
    },
    Model {
        pid: 0x0D4C,
        cable: false,
        query: false,
        page: 0xFF10,
        usage: 0x0006,
        name: "MSI Clutch GM41 Lightweight Wireless",
    },
    Model {
        pid: 0x0D4B,
        cable: true,
        query: false,
        page: 0xFF10,
        usage: 0x0006,
        name: "MSI Clutch GM41 Lightweight Wireless",
    },
];

pub static MATCHES: &[Match] = &[Match {
    vid: VID,
    pids: &[0x9E22, 0x0D3C, 0x0D3B, 0x0D4C, 0x0D4B],
}];

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    poll_with(&colls(infos), &mut HidIo(api))
}

/// (percent, charging) from a 0F 20 status frame.
pub fn parse_status(r: &[u8]) -> Option<(u8, bool)> {
    if r.len() < 15 || r[0] != 0x0F || r[1] != 0x20 {
        return None;
    }
    let level = r[14] & 0x7F;
    (level <= 100).then_some((level, r[14] & 0x80 != 0))
}

/// Linear fit through (0x4D, 57 %), (0x48, 54 %) and (0x45, 53 %): about 0.51 % per step.
pub fn gm41_percent(raw: u8) -> u8 {
    ((i32::from(raw) * 51 + 1760 + 50) / 100).clamp(0, 100) as u8
}

/// Percent from a GM41 push frame.
pub fn parse_gm41(r: &[u8]) -> Option<u8> {
    (r.len() >= 7 && r[0] == 0xA0 && r[2] == 0x07).then(|| gm41_percent(r[1]))
}

fn query(d: &mut dyn Dev, end: Instant) -> Option<(u8, bool)> {
    d.flush();
    if !d.write(&[0x00, 0x0F, 0x20]) {
        return None;
    }
    let end = end.min(Instant::now() + Duration::from_millis(500));
    let r = read_until(d, end, |r| r.len() >= 15 && r[0] == 0x0F && r[1] == 0x20)?;
    parse_status(&r)
}

/// The GM41 only reports while the mouse moves; a sleeping mouse yields nothing.
fn listen(d: &mut dyn Dev, end: Instant) -> Option<(u8, bool)> {
    let end = end.min(Instant::now() + Duration::from_millis(2000));
    let r = read_until(d, end, |r| parse_gm41(r).is_some())?;
    parse_gm41(&r).map(|p| (p, false))
}

fn poll_with(colls: &[Coll], io: &mut dyn Io) -> Vec<Device> {
    let end = Instant::now() + BUDGET;
    let mut out = Vec::new();
    for m in &MODELS {
        let Some(c) = find(colls, VID, m.pid, |c| {
            c.iface == 1
                && c.page == m.page
                && c.usage == m.usage
                && if m.query {
                    c.out_len >= 65 && c.in_len >= 65
                } else {
                    c.in_len >= 7
                }
        })
        .into_iter()
        .next() else {
            continue;
        };
        if ms_left(end) == 0 {
            break;
        }
        let reading = io.open(c).and_then(|mut d| {
            if m.query {
                query(d.as_mut(), end)
            } else {
                listen(d.as_mut(), end)
            }
        });
        let Some((pct, charging)) = reading else {
            continue;
        };
        let mut s = Device::new(
            format!("msi:{}", hex4(m.pid)),
            m.name, // the Clutch dongles report a serial number as their product string
            Kind::Mouse,
            if m.cable {
                "USB cable"
            } else {
                "2.4 GHz receiver"
            },
        );
        if pct == 0 && !charging {
            s.asleep = true;
        } else {
            s.percent = Some(pct);
            s.charging = charging;
        }
        s.grace = Duration::from_secs(1800);
        out.push(s);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::hid_dev::fake::{Fake, coll};

    #[test]
    fn gm31_frame_confirmed_against_msi_center() {
        // Pushed frame 0F 20 00 00 23 05 7A 91 00 00 00 00 05 80 56: 0x56 = 86 %.
        let r = [
            0x0F, 0x20, 0x00, 0x00, 0x23, 0x05, 0x7A, 0x91, 0x00, 0x00, 0x00, 0x00, 0x05, 0x80,
            0x56, 0x00,
        ];
        assert_eq!(parse_status(&r), Some((86, false)));
        let mut charging = r;
        charging[14] = 0x80 | 40;
        assert_eq!(parse_status(&charging), Some((40, true)));
    }

    #[test]
    fn gm41_raw_follows_the_linear_fit() {
        assert_eq!(gm41_percent(0x4D), 57);
        assert_eq!(gm41_percent(0x48), 54);
        assert_eq!(gm41_percent(0x45), 53);
        assert_eq!(
            parse_gm41(&[0xA0, 0x4D, 0x07, 0x00, 0x94, 0x00, 0x94]),
            Some(57)
        );
        assert_eq!(
            parse_gm41(&[0xA0, 0x4D, 0x06, 0x00, 0x94, 0x00, 0x94]),
            None
        );
    }

    #[test]
    fn zero_level_reads_as_asleep() {
        let mut f = Fake::with(|s, _, _, q| {
            assert_eq!(q, [0, 0x0F, 0x20]);
            let mut r = vec![0u8; 64];
            r[0] = 0x0F;
            r[1] = 0x20;
            s.push_input("", r);
        });
        let out = poll_with(&[coll(VID, 0x0D3C, 1, 0xFF00, 1, 65, 65, 0)], &mut f);
        assert!(out[0].asleep && out[0].percent.is_none());
    }
}

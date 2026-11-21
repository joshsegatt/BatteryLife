//! Fractal Design Scape headset (VID 36BC): 2.4 GHz dongle (0001) or the headset on its USB-C cable
//! (2001), 64-byte vendor collection FF00:0001. Request output report 02 [02][device type][21]
//! (GET_DEVICE_STATE) to type 11 (dongle) or F1 (headset); the reply echoes type and command.
//!   Dongle reply:  [4] = 1 when the headset is linked.
//!   Headset reply: [15] = battery %, [23] = 1 on the USB cable (used as the charging bit).
//!
//! Ported from BatteryHub (src/drivers/fractal.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0, which credits Fractal's own
//! WebHID configurator, Adjust Pro.

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::hid_io::{self, Coll, Io, Open, find, query};

pub const VID: u16 = 0x36BC;
pub const PIDS: &[u16] = &[0x0001, 0x2001];
const TYPE_DONGLE: u8 = 0x11;
const TYPE_HEADSET: u8 = 0xF1;
const GET_DEVICE_STATE: u8 = 0x21;

fn get_state(io: &mut dyn Io, kind: u8) -> Option<Vec<u8>> {
    io.flush();
    query(
        io,
        &[0x02, kind, GET_DEVICE_STATE],
        hid_io::MAX_REPLY_MS,
        |r| r.len() >= 24 && r[0] == 0x02 && r[1] == kind && r[2] == GET_DEVICE_STATE,
    )
}

pub fn poll_colls(colls: &[Coll], open: &mut Open<'_>) -> Vec<Device> {
    let mut out = Vec::new();
    for &pid in PIDS {
        let cable = pid == 0x2001;
        for c in find(colls, VID, pid, |c| {
            c.page == 0xFF00 && c.usage == 0x0001 && c.out_len >= 4 && c.in_len >= 24
        }) {
            let Some(mut io) = open(c) else { continue };
            if !cable {
                let Some(r) = get_state(io.as_mut(), TYPE_DONGLE) else {
                    continue;
                };
                if r[4] != 1 {
                    break; // dongle answers, headset off, out of range or on its stand
                }
            }
            let Some(r) = get_state(io.as_mut(), TYPE_HEADSET).filter(|r| r[15] <= 100) else {
                continue;
            };
            let via = if cable { "USB cable" } else { "2.4 GHz dongle" };
            // Same entry on the dongle and on the cable.
            let mut d = Device::new("fractal:scape", "Fractal Scape", Kind::Headset, via);
            d.percent = Some(r[15]);
            d.charging = r[23] == 1;
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

    fn state(kind: u8, at: usize, v: u8, at2: usize, v2: u8) -> Vec<u8> {
        let mut r = vec![0u8; 64];
        r[..3].copy_from_slice(&[0x02, kind, GET_DEVICE_STATE]);
        r[at] = v;
        r[at2] = v2;
        r
    }

    fn run(pid: u16, dongle_link: u8) -> (Vec<Device>, Fake) {
        let f = Fake::new();
        f.replies(move |w| match w[1] {
            TYPE_DONGLE => vec![state(TYPE_DONGLE, 4, dongle_link, 5, 0)],
            TYPE_HEADSET => vec![state(TYPE_HEADSET, 15, 68, 23, u8::from(pid == 0x2001))],
            _ => vec![],
        });
        let opened = RefCell::new(Vec::new());
        let c = Coll::new(VID, pid, 0xFF00, 1, 64, 64);
        let out = poll_colls(&[c], &mut *f.opener(&opened));
        (out, f)
    }

    #[test]
    fn dongle_relays_the_headset() {
        let (out, f) = run(0x0001, 1);
        assert_eq!(out[0].key, "fractal:scape");
        assert_eq!((out[0].percent, out[0].charging), (Some(68), false));
        assert_eq!(out[0].via, "2.4 GHz dongle");
        assert_eq!(f.s().writes, vec![vec![2, 0x11, 0x21], vec![2, 0xF1, 0x21]]);
    }

    #[test]
    fn cable_and_unlinked_headset() {
        let (out, f) = run(0x2001, 0);
        assert_eq!((out[0].percent, out[0].charging), (Some(68), true));
        assert_eq!(f.s().writes, vec![vec![2, 0xF1, 0x21]]);
        let (out, _) = run(0x0001, 0);
        assert!(out.is_empty());
    }
}

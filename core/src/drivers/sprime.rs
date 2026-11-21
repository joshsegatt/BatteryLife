//! Sprime PM1 wireless mouse (Nordic VID 1915): receivers AC1C / AD8C, USB cable AC11 / AC12 /
//! AC13. Status query on feature report 05 of the FFA0:0001 collection: SET_FEATURE
//! [05][15][00][00][01][00 ...], then GET_FEATURE 05 -> [9] percent, [10] charging, [11] fully
//! charged, [12] online (receiver). The bootloader (AC0B) is never touched.
//!
//! Ported from BatteryHub (src/drivers/sprime.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0, from Sprime's web driver
//! (sprime.pro).

use std::time::{Duration, Instant};

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::hid::{self, BUDGET, Coll, Hid, find, hex4, left, pids};
use crate::registry::Match;

const VID: u16 = 0x1915;
const CABLES: [u16; 3] = [0xAC11, 0xAC12, 0xAC13];
const RECEIVERS: [u16; 2] = [0xAC1C, 0xAD8C];

pub static MATCHES: &[Match] = &[Match {
    vid: VID,
    pids: &[0xAC11, 0xAC12, 0xAC13, 0xAC1C, 0xAD8C],
}];

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    hid::run(api, infos, |hid, colls| poll_with(colls, hid))
}

#[derive(Debug, PartialEq)]
pub struct Status {
    /// None while the receiver answers but the mouse is off or out of range.
    pub percent: Option<u8>,
    pub charging: bool,
    pub online: bool,
}

/// Decodes a GET_FEATURE 05 status reply (indices include the report id).
pub fn parse(r: &[u8], cable: bool) -> Option<Status> {
    if r.len() < 13 || r[0] != 5 || r[1] != 0x15 || r[9] > 100 {
        return None;
    }
    let full = r[11] != 0;
    let online = cable || r[12] != 0;
    Some(Status {
        percent: online.then_some(if full { 100 } else { r[9] }),
        charging: online && r[10] != 0 && !full,
        online,
    })
}

fn poll_with(colls: &[Coll], io: &mut dyn Hid) -> Vec<Device> {
    let end = Instant::now() + BUDGET;
    let mut out = Vec::new();
    for pid in pids(colls, VID) {
        let cable = CABLES.contains(&pid);
        if !cable && !RECEIVERS.contains(&pid) {
            continue;
        }
        for c in find(colls, VID, pid, |c| {
            c.page == 0xFFA0 && c.usage == 1 && c.feat_len >= 32
        }) {
            if left(end).is_zero() {
                return out;
            }
            let Some(mut d) = io.open(c) else { continue };
            let mut req = [0u8; 32];
            req[0] = 0x05; // report id
            req[1] = 0x15; // status query
            req[4] = 0x01;
            let mut status = None;
            for attempt in 0..4 {
                if attempt > 0 {
                    io.sleep(Duration::from_millis(90));
                }
                if !d.set_feature(&req) {
                    break;
                }
                io.sleep(Duration::from_millis(15));
                if let Some(s) = d.get_feature(5).and_then(|r| parse(&r, cable)) {
                    let online = s.online;
                    status = Some(s);
                    if online {
                        break;
                    }
                }
            }
            let Some(st) = status else { continue };
            // Receiver present, mouse off or out of range: nothing to show (BatteryHub hides it).
            if st.online {
                let mut s = Device::new(
                    format!("sprime:{}:{}", hex4(VID), hex4(pid)),
                    "Sprime PM1",
                    Kind::Mouse,
                    if cable {
                        "USB cable"
                    } else {
                        "2.4 GHz receiver"
                    },
                );
                s.percent = st.percent;
                s.charging = st.charging;
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

    fn reply(pct: u8, charging: u8, full: u8, online: u8) -> Vec<u8> {
        let mut r = vec![0u8; 32];
        r[0] = 5;
        r[1] = 0x15;
        r[9] = pct;
        r[10] = charging;
        r[11] = full;
        r[12] = online;
        r
    }

    #[test]
    fn parses_status_reply() {
        let s = parse(&reply(64, 1, 0, 1), false).unwrap();
        assert_eq!((s.percent, s.charging, s.online), (Some(64), true, true));
        let s = parse(&reply(97, 1, 1, 1), false).unwrap();
        assert_eq!((s.percent, s.charging), (Some(100), false), "full wins");
        let s = parse(&reply(50, 0, 0, 0), false).unwrap();
        assert_eq!((s.percent, s.online), (None, false), "mouse off");
        assert!(
            parse(&reply(50, 0, 0, 0), true).unwrap().online,
            "cable is always online"
        );
        assert!(parse(&reply(101, 0, 0, 1), false).is_none());
    }

    #[test]
    fn polls_the_receiver() {
        let mut f = Fake::with(|s, fc, op| {
            if let Some(q) = op.sent() {
                assert_eq!(q[..5], [5, 0x15, 0, 0, 1]);
                s.push_feature("", reply(80, 0, 0, 1));
            }
            fake::queued(s, fc, op)
        });
        f.s().pad_features = true;
        let out = poll_with(&[coll(VID, 0xAC1C, 0, 0xFFA0, 1, 0, 0, 32)], &mut f);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].percent, Some(80));
        assert_eq!(out[0].key, "sprime:1915:AC1C");
    }
}

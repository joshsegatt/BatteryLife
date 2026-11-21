//! WLMouse (VID 36A7): Beast X 8K, Beast X Mini / Mini Pro, Beast X Pro, Beast MAX, Beast X V2,
//! Sword X 8K, Beast G and their receivers. Read-only status query on the vendor collection (page
//! FFFF, 65-byte feature report 0): SET_FEATURE [00][00][00][02][02][00][83][00 ...], then GET_FEATURE
//! report 0 until [00][status][00][02][02][00][83][charging][battery%]; status A1 / A2 answered, A0 the
//! mouse is asleep. A887 / A888 (Beast X 4K / original Beast X) are not handled: the sources disagree
//! on their protocol. The YING (A874) is read by the Glorious driver.
//!
//! Ported from BatteryHub (src/drivers/wlmouse.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0, which credits mouse-gauge
//! and HaloBattery.

use std::time::Duration;

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::hid::{self, Coll, Dev, Hid, find};

pub const VID: u16 = 0x36A7;

/// (pid, receiver, name, receiver of this cable). Cables come first: a mouse on its cable with the
/// receiver also plugged in shows up under both ids, and is listed once, by its cable.
const MODELS: &[(u16, bool, &str, u16)] = &[
    (0xA879, false, "WLMouse Sword X 8K", 0xA878),
    (0xA884, false, "WLMouse Beast X 8K", 0xA883),
    (0xA861, false, "WLMouse Beast G", 0xA860),
    (0xA866, false, "WLMouse mouse", 0),
    (0xA867, false, "WLMouse mouse", 0),
    (0xA871, false, "WLMouse Beast X Pro", 0xA870),
    (0xA856, true, "WLMouse Beast X V2", 0),
    (0xA860, true, "WLMouse Beast G", 0),
    (0xA868, true, "WLMouse Beast X Mini Pro", 0),
    (0xA870, true, "WLMouse Beast X Pro", 0),
    (0xA878, true, "WLMouse Sword X 8K", 0),
    (0xA880, true, "WLMouse Beast MAX", 0),
    (0xA883, true, "WLMouse Beast X 8K", 0),
    (0xA885, true, "WLMouse Beast X Mini", 0),
];

pub const PIDS: &[u16] = &[
    0xA879, 0xA884, 0xA861, 0xA866, 0xA867, 0xA871, 0xA856, 0xA860, 0xA868, 0xA870, 0xA878, 0xA880,
    0xA883, 0xA885,
];

/// The SET_FEATURE buffer: report id 0, then the request.
pub fn request() -> Vec<u8> {
    let mut req = vec![0u8; 65];
    req[3] = 0x02;
    req[4] = 0x02;
    req[6] = 0x83;
    req
}

#[derive(Debug, PartialEq)]
pub enum Reply {
    Battery {
        pct: u8,
        charging: bool,
    },
    Asleep,
    /// Bad level: give up on this collection.
    Invalid,
    /// Not the reply yet (or our own request echoed back).
    Pending,
}

/// Classifies a GET_FEATURE buffer (report id first).
pub fn parse(r: &[u8]) -> Reply {
    if r[3] != 0x02 || r[4] != 0x02 || r[6] != 0x83 {
        return Reply::Pending;
    }
    match r[1] {
        0xA0 => Reply::Asleep,
        0xA1 | 0xA2 if r[8] > 100 => Reply::Invalid,
        0xA1 | 0xA2 => Reply::Battery {
            pct: r[8],
            charging: r[7] == 0x01,
        },
        _ => Reply::Pending,
    }
}

fn query(hid: &mut dyn Hid, d: &mut dyn Dev) -> Option<Reply> {
    for _ in 0..2 {
        if !d.set_feature(&request()) {
            return None;
        }
        for _ in 0..6 {
            hid.sleep(Duration::from_millis(60));
            let r = d.get_feature(0).filter(|r| r.len() >= 9)?;
            match parse(&r) {
                Reply::Pending => {}
                Reply::Invalid => return None,
                reply => return Some(reply),
            }
        }
    }
    None
}

fn poll_with(hid: &mut dyn Hid, colls: &[Coll]) -> Vec<Device> {
    let mut out = Vec::new();
    let mut covered: Vec<u16> = Vec::new(); // receivers whose mouse answered on its cable
    for &(pid, receiver, name, receiver_pid) in MODELS {
        if covered.contains(&pid) {
            continue;
        }
        let mut cs = find(colls, VID, pid, |c| c.page == 0xFFFF && c.feat_len >= 65);
        // Usage 0 is the collection that answers in both sources; try it first.
        cs.sort_by_key(|c| c.usage != 0);
        for c in cs {
            let Some(mut d) = hid.open(c) else { continue };
            let Some(reply) = query(hid, d.as_mut()) else {
                continue;
            };
            let mut s = Device::new(
                format!("wlmouse:{pid:04X}"),
                name,
                Kind::Mouse,
                if receiver {
                    "2.4 GHz receiver"
                } else {
                    "USB cable"
                },
            );
            if receiver {
                s.grace = Duration::from_secs(30 * 60); // a sleeping mouse can leave it silent
            }
            match reply {
                Reply::Battery { pct, charging } => {
                    s.percent = Some(pct);
                    s.charging = charging;
                }
                _ => s.asleep = true, // shown with the last known value
            }
            out.push(s);
            if receiver_pid != 0 {
                covered.push(receiver_pid);
            }
            break;
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

    fn reply(status: u8, chg: u8, pct: u8) -> Vec<u8> {
        let mut r = vec![0u8; 65];
        r[1] = status;
        r[3] = 0x02;
        r[4] = 0x02;
        r[6] = 0x83;
        r[7] = chg;
        r[8] = pct;
        r
    }

    fn fake(replies: Vec<Vec<u8>>) -> Fake {
        let mut q: VecDeque<Vec<u8>> = replies.into();
        Fake::with(move |_, _, op| match op.clone() {
            Op::SetFeature(w) => {
                assert_eq!(w, request());
                Some(vec![])
            }
            Op::GetFeature(0, _) => q.pop_front(),
            _ => None,
        })
    }

    fn coll(pid: u16, usage: u16) -> Coll {
        Coll {
            vid: VID,
            pid,
            page: 0xFFFF,
            usage,
            feat_len: 65,
            path: format!("{pid:04X}-{usage}"),
            ..Default::default()
        }
    }

    #[test]
    fn beast_x_8k_on_its_receiver_after_the_echo() {
        // The request echoed back first (status 00), then the answer.
        let mut f = fake(vec![request(), reply(0xA1, 0, 81)]);
        let out = poll_with(&mut f, &[coll(0xA883, 1), coll(0xA883, 0)]);
        let s = &out[0];
        assert_eq!(
            (s.name.as_str(), s.percent, s.charging),
            ("WLMouse Beast X 8K", Some(81), false)
        );
        assert_eq!(
            (s.key.as_str(), s.via.as_str()),
            ("wlmouse:A883", "2.4 GHz receiver")
        );
        assert_eq!(f.s().log[0].0, "A883-0", "usage 0 first");
    }

    #[test]
    fn asleep_and_invalid() {
        let out = poll_with(&mut fake(vec![reply(0xA0, 0, 0)]), &[coll(0xA880, 0)]);
        assert!(out[0].asleep && out[0].percent.is_none());
        assert!(poll_with(&mut fake(vec![reply(0xA2, 0, 101)]), &[coll(0xA880, 0)]).is_empty());
    }

    #[test]
    fn a_cable_that_answers_covers_its_receiver() {
        let mut f = fake(vec![reply(0xA1, 1, 50), reply(0xA1, 0, 49)]);
        let out = poll_with(&mut f, &[coll(0xA878, 0), coll(0xA879, 0)]);
        assert_eq!(out.len(), 1);
        assert_eq!((out[0].via.as_str(), out[0].charging), ("USB cable", true));
        assert_eq!(f.s().opened.len(), 1);
    }
}

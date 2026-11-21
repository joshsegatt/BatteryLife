//! Pulsar X2 V3 / X2 V3 eS and other Sonix-based Pulsar mice on vendor id 3710: cables 3402 / 3406,
//! 1K dongle 5402 and 8K dongle 5403 (both shared by several models, so a dongle gets a generic name).
//! Feature report 0, 64-byte packet [00][08][81][01][00 ...][sum lo][sum hi] (16-bit sum of bytes
//! 0..61) reads the battery; GET_FEATURE every ~50 ms until the reply echoes the command:
//! [status][08][81][01][..][..][percent]. Status 01 answered, 02 pending, 05 pending or "mouse not
//! linked" (only taken as off if it is still the last answer). Only this read is sent.
//!
//! Ported from BatteryHub (src/drivers/pulsar.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0, which credits PulsarBattery
//! and pulsar-mouse-linux.

use std::time::Duration;

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::report_io::{self, Coll, Dev, Hid, device, find};

pub const VID: u16 = 0x3710;
/// (pid, cable, name); dongles are named after the mouse only when it says so.
const PRODUCTS: &[(u16, bool, &str)] = &[
    (0x3402, true, "Pulsar X2 V3"),
    (0x3406, true, "Pulsar X2 V3 eS"),
    (0x5402, false, ""), // 1K dongle
    (0x5403, false, ""), // 8K dongle
];
pub const PIDS: &[u16] = &[0x3402, 0x3406, 0x5402, 0x5403];

/// The SET_FEATURE buffer: report id 0, then the checksummed 64-byte packet.
pub fn request() -> Vec<u8> {
    let mut req = vec![0u8; 65];
    let p = &mut req[1..];
    p[1] = 0x08;
    p[2] = 0x81;
    p[3] = 0x01;
    let sum: u16 = p[..62].iter().map(|b| u16::from(*b)).sum();
    p[62..64].copy_from_slice(&sum.to_le_bytes());
    req
}

#[derive(Debug, PartialEq)]
pub enum Reply {
    Battery(u8),
    /// FF = no reading, or another bad level.
    Invalid,
    /// Not our reply yet; `offline` when the status was 05.
    Pending {
        offline: bool,
    },
}

/// Classifies a GET_FEATURE buffer (report id first).
pub fn parse(r: &[u8]) -> Reply {
    let w = &r[1..];
    if w[1] != 0x08 || w[2] != 0x81 || w[3] != 0x01 {
        return Reply::Pending { offline: false };
    }
    if w[0] == 0x01 {
        return if w[6] > 100 {
            Reply::Invalid
        } else {
            Reply::Battery(w[6])
        };
    }
    Reply::Pending {
        offline: w[0] == 0x05,
    }
}

#[derive(Debug, PartialEq)]
enum Read {
    Battery(u8),
    Offline,
    None,
}

fn read_battery(hid: &mut dyn Hid, d: &mut dyn Dev) -> Read {
    let req = request();
    let mut saw_offline = false;
    for _ in 0..3 {
        if !d.set_feature(&req) {
            return Read::None;
        }
        for _ in 0..6 {
            hid.sleep(Duration::from_millis(50));
            let Some(r) = d.get_feature(0).filter(|r| r.len() >= 65) else {
                return Read::None;
            };
            match parse(&r) {
                Reply::Battery(p) => return Read::Battery(p),
                Reply::Invalid => return Read::None,
                Reply::Pending { offline } => saw_offline = offline,
            }
        }
    }
    if saw_offline {
        Read::Offline
    } else {
        Read::None
    }
}

fn poll_with(hid: &mut dyn Hid, colls: &[Coll]) -> Vec<Device> {
    let mut out = Vec::new();
    for &(pid, cable, name) in PRODUCTS {
        let mut cs = find(colls, VID, pid, |c| c.feature >= 65);
        // Interface 2 rejects SET_FEATURE: try the higher interfaces first.
        cs.sort_by_key(|c| std::cmp::Reverse(c.iface));
        for c in cs {
            let Some(mut d) = hid.open(c) else { continue };
            let rep = read_battery(hid, d.as_mut());
            let pct = match rep {
                Read::None => continue,
                Read::Battery(p) => Some(p),
                Read::Offline => None,
            };
            let mut s;
            if cable {
                s = device(format!("pulsar:{pid:04X}"), name, Kind::Mouse, "USB cable");
                s.charging = true;
                s.percent = pct;
            } else {
                // 0% from a dongle means the mouse is off or out of range: not listed, the tracker
                // keeps the last level.
                let Some(p) = pct.filter(|p| *p > 0) else {
                    break;
                };
                let generic = c.product.is_empty()
                    || c.product.contains("Dongle")
                    || c.product.contains("Receiver");
                let n = if generic {
                    "Pulsar wireless mouse"
                } else {
                    c.product.as_str()
                };
                s = device(
                    format!("pulsar:{pid:04X}"),
                    n,
                    Kind::Mouse,
                    "2.4 GHz receiver",
                );
                s.grace = Duration::from_secs(30 * 60);
                s.percent = Some(p);
            }
            out.push(s);
            break;
        }
    }
    out
}

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    report_io::run(api, infos, poll_with)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::report_io::fake::{Fake, Op};
    use std::collections::VecDeque;

    fn reply(status: u8, pct: u8) -> Vec<u8> {
        let mut r = vec![0u8; 65];
        r[1] = status;
        r[2] = 0x08;
        r[3] = 0x81;
        r[4] = 0x01;
        r[7] = pct;
        r
    }

    fn fake(replies: Vec<Vec<u8>>) -> Fake {
        let mut q: VecDeque<Vec<u8>> = replies.into();
        Fake::new(move |c, op| match op {
            Op::SetFeature(w) => {
                assert_eq!(w, request());
                (c.iface != 2).then(Vec::new) // interface 2 rejects SET_FEATURE
            }
            Op::GetFeature(0) => q.pop_front(),
            _ => None,
        })
    }

    fn coll(pid: u16, iface: i32, product: &str) -> Coll {
        Coll {
            vid: VID,
            pid,
            iface,
            feature: 65,
            page: 0xFFFF,
            usage: 1,
            product: product.into(),
            ..Default::default()
        }
    }

    #[test]
    fn checksum_is_sum_of_the_packet() {
        let r = request();
        assert_eq!(&r[1..5], &[0x00, 0x08, 0x81, 0x01]);
        assert_eq!(&r[63..65], &[0x8A, 0x00]); // 08 + 81 + 01
    }

    #[test]
    fn dongle_reads_after_pending_replies() {
        let pending = reply(0x02, 0);
        let mut f = fake(vec![pending.clone(), pending, reply(0x01, 64)]);
        let out = poll_with(
            &mut f,
            &[
                coll(0x5402, 2, "Pulsar 1K Dongle"),
                coll(0x5402, 3, "Pulsar 1K Dongle"),
            ],
        );
        assert_eq!(f.opens.get(), 1, "interface 3 first, and it answered");
        let s = &out[0];
        assert_eq!(
            (s.name.as_str(), s.percent),
            ("Pulsar wireless mouse", Some(64))
        );
        assert_eq!(
            (s.key.as_str(), s.via.as_str()),
            ("pulsar:5402", "2.4 GHz receiver")
        );
    }

    #[test]
    fn cable_charges_and_a_silent_or_off_mouse_is_not_listed() {
        let out = poll_with(
            &mut fake(vec![reply(0x01, 88)]),
            &[coll(0x3402, 3, "X2 v3")],
        );
        assert_eq!(
            (out[0].name.as_str(), out[0].charging),
            ("Pulsar X2 V3", true)
        );
        // 0% from the dongle: off.
        assert!(poll_with(&mut fake(vec![reply(0x01, 0)]), &[coll(0x5403, 3, "")]).is_empty());
        // 05 until we give up: off. FF: no reading.
        let off = vec![reply(0x05, 0); 18];
        assert!(poll_with(&mut fake(off), &[coll(0x5403, 3, "")]).is_empty());
        assert!(poll_with(&mut fake(vec![reply(0x01, 0xFF)]), &[coll(0x5403, 3, "")]).is_empty());
    }
}

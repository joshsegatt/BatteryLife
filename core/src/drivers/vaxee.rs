//! VAXEE 4K and 8K receivers (3057:1002 / 2002) and the ZYGEN NP-01S V2 USB cable (3057:1011), on
//! collection FF05:0001 (64-byte feature report 0E). Request [0E][A5][command][01 = READ][01];
//! ~100 ms later GET_FEATURE 0E -> [0E][A5][command][..][01][value]. Only battery (0B, 0..20 steps of
//! 5%) and charging (10) are read; never configuration writes.
//!
//! BatteryHub also ends the sleeping-device grace period at once when the USB device disappears;
//! that needs a "disconnected" state the tracker does not have yet, so it is not ported.
//!
//! Ported from BatteryHub (src/drivers/vaxee.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0, which credits
//! mouse-battery-monitor (docs/VAXEE.md).

use std::time::Duration;

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::report_io::{self, Coll, Dev, Hid, device, find, products};

pub const VID: u16 = 0x3057;
const RECEIVER: u16 = 0x1002;
const RECEIVER_8K: u16 = 0x2002;
const CABLE: u16 = 0x1011;
pub const PIDS: &[u16] = &[RECEIVER, RECEIVER_8K, CABLE];

/// The 64-byte SET_FEATURE buffer for a READ of `command`.
pub fn request(command: u8) -> Vec<u8> {
    let mut r = vec![0u8; 64];
    r[..5].copy_from_slice(&[0x0E, 0xA5, command, 0x01, 0x01]);
    r
}

/// The value of a GET_FEATURE reply to `command`.
pub fn parse(reply: &[u8], command: u8) -> Option<u8> {
    (reply.len() >= 6
        && reply[0] == 0x0E
        && reply[1] == 0xA5
        && reply[2] == command
        && reply[4] == 1)
        .then(|| reply[5])
}

fn query(hid: &mut dyn Hid, d: &mut dyn Dev, command: u8) -> Option<u8> {
    if !d.set_feature(&request(command)) {
        return None;
    }
    hid.sleep(Duration::from_millis(100));
    parse(&d.get_feature(0x0E)?, command)
}

fn poll_with(hid: &mut dyn Hid, colls: &[Coll]) -> Vec<Device> {
    let mut out = Vec::new();
    for (_, pid) in products(colls).into_iter().filter(|p| p.0 == VID) {
        if !PIDS.contains(&pid) {
            continue;
        }
        let cs = find(colls, VID, pid, |c| {
            c.page == 0xFF05 && c.usage == 1 && c.feature == 64
        });
        for c in cs {
            let Some(mut d) = hid.open(c) else { continue };
            let battery = (0..2).find_map(|_| query(hid, d.as_mut(), 0x0B).filter(|b| *b <= 20));
            let Some(battery) = battery else {
                continue; // the tracker keeps the last reading during the grace period
            };
            let (name, via) = match pid {
                CABLE => ("ZYGEN NP-01S V2 Wireless", "USB cable"),
                RECEIVER_8K => ("VAXEE 8K Wireless", "2.4 GHz receiver"),
                _ => ("VAXEE wireless mouse", "2.4 GHz receiver"),
            };
            // Keyed by product id, not the HID path, which changes with the USB port.
            let mut s = device(format!("vaxee:{pid:04X}"), name, Kind::Mouse, via);
            s.percent = Some(battery * 5);
            s.grace = Duration::from_secs(30 * 60);
            if let Some(charging) = query(hid, d.as_mut(), 0x10) {
                s.charging = charging != 0;
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

    fn coll(pid: u16) -> Coll {
        Coll {
            vid: VID,
            pid,
            page: 0xFF05,
            usage: 1,
            feature: 64,
            path: "test-receiver".into(),
            ..Default::default()
        }
    }

    fn reply(command: u8, value: u8) -> Vec<u8> {
        let mut r = vec![0u8; 64];
        r[..6].copy_from_slice(&[0x0E, 0xA5, command, 1, 1, value]);
        r
    }

    fn fake(replies: Vec<Vec<u8>>, send_ok: bool) -> Fake {
        let mut q: VecDeque<Vec<u8>> = replies.into();
        Fake::new(move |_, op| match op {
            Op::SetFeature(w) => {
                assert!(w[2] == 0x0B || w[2] == 0x10);
                assert_eq!(w, request(w[2]));
                send_ok.then(Vec::new)
            }
            Op::GetFeature(id) => {
                assert_eq!(id, 0x0E);
                q.pop_front()
            }
            _ => None,
        })
    }

    fn poll(replies: Vec<Vec<u8>>, pid: u16) -> (Vec<Device>, usize) {
        let mut f = fake(replies, true);
        let out = poll_with(&mut f, &[coll(pid)]);
        let queries = f.count(|o| matches!(o, Op::SetFeature(_)));
        (out, queries)
    }

    #[test]
    fn receivers_and_cable() {
        let (out, queries) = poll(vec![reply(0x0B, 13), reply(0x10, 0)], 0x1002);
        assert_eq!(
            (out[0].percent, out[0].charging, queries),
            (Some(65), false, 2)
        );
        assert_eq!(
            (out[0].via.as_str(), out[0].key.as_str()),
            ("2.4 GHz receiver", "vaxee:1002")
        );
        let (out, _) = poll(vec![reply(0x0B, 12), reply(0x10, 0)], 0x2002);
        assert_eq!(
            (out[0].percent, out[0].name.as_str()),
            (Some(60), "VAXEE 8K Wireless")
        );
        let (out, _) = poll(vec![reply(0x0B, 14), reply(0x10, 1)], 0x1011);
        assert_eq!((out[0].percent, out[0].charging), (Some(70), true));
        assert_eq!(
            (out[0].name.as_str(), out[0].via.as_str()),
            ("ZYGEN NP-01S V2 Wireless", "USB cable")
        );
        let (out, _) = poll(vec![reply(0x0B, 20), reply(0x10, 1)], 0x1002);
        assert_eq!((out[0].percent, out[0].charging), (Some(100), true));
        let (out, _) = poll(vec![reply(0x0B, 0)], 0x1002);
        assert_eq!(out[0].percent, Some(0));
    }

    #[test]
    fn invalid_levels_and_headers() {
        // 21 is out of range: asked again.
        let (out, queries) = poll(
            vec![reply(0x0B, 21), reply(0x0B, 7), reply(0x10, 0)],
            0x1002,
        );
        assert_eq!((out[0].percent, queries), (Some(35), 3));
        let (out, _) = poll(vec![reply(0x0B, 21), reply(0x10, 10)], 0x1002);
        assert!(out.is_empty());
        let mut bad_header = reply(0x0B, 10);
        bad_header[1] = 0;
        assert!(poll(vec![bad_header, vec![0; 5]], 0x1002).0.is_empty());
        for offset in [0, 2, 4] {
            let mut bad = reply(0x0B, 10);
            bad[offset] = 0;
            let (out, queries) = poll(vec![bad.clone(), bad], 0x1002);
            assert!(out.is_empty());
            assert_eq!(queries, 2);
        }
        // A stale battery reply is not a charging response.
        let (out, _) = poll(vec![reply(0x0B, 10), reply(0x0B, 10)], 0x1002);
        assert_eq!((out[0].percent, out[0].charging), (Some(50), false));
    }

    #[test]
    fn failures_and_filtering() {
        assert!(poll(vec![], 0x1002).0.is_empty());
        let mut f = fake(vec![reply(0x0B, 10)], false);
        assert!(poll_with(&mut f, &[coll(0x1002)]).is_empty());
        let mut f = fake(vec![reply(0x0B, 10)], true);
        f.open_ok.set(false);
        assert!(poll_with(&mut f, &[coll(0x1002)]).is_empty());
        assert_eq!(f.count(|o| matches!(o, Op::SetFeature(_))), 0);
        let mut wrong_pid = coll(0x0002); // do not send VAXEE commands to other mice
        wrong_pid.path = "x".into();
        let mut wrong_page = coll(0x1002);
        wrong_page.page = 1;
        let mut wrong_usage = coll(0x1002);
        wrong_usage.usage = 2;
        let mut wrong_len = coll(0x1002);
        wrong_len.feature = 63;
        let mut f = fake(vec![], true);
        assert!(poll_with(&mut f, &[wrong_pid, wrong_page, wrong_usage, wrong_len]).is_empty());
        assert_eq!(f.opens.get(), 0);
    }
}

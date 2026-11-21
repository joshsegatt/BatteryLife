//! Redragon M602-KS / M690 Pro on the Sinowealth 2.4 GHz receiver (258A:002F): link state (feature
//! report 05, command 80) and battery level (command 90), 8-byte feature reports on interface 1,
//! FF00:0001. The USB id is shared by other receivers, so the collection shape is matched exactly.
//!
//! Ported from BatteryHub (src/drivers/sinowealth_receiver.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0, which cites
//! github.com/OpenMouse-Project/mouse-protocol (docs/redragon-m690-pro-testing.md).

use std::time::Duration;

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::hid::{self, Coll, Dev, Hid, find};
use crate::registry::Match;

const VID: u16 = 0x258A;
const PID: u16 = 0x002F;

pub static MATCHES: &[Match] = &[Match {
    vid: VID,
    pids: &[PID],
}];

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    hid::run(api, infos, |hid, colls| poll_with(colls, hid))
}

/// SET_FEATURE [05][cmd][0 x6], 100 ms, GET_FEATURE 05; the reply must echo the command.
fn query(d: &mut dyn Dev, io: &mut dyn Hid, cmd: u8) -> Option<Vec<u8>> {
    if !d.set_feature(&[5, cmd, 0, 0, 0, 0, 0, 0]) {
        return None;
    }
    io.sleep(Duration::from_millis(100));
    d.get_feature(5)
        .filter(|r| r.len() == 8 && r[0] == 5 && r[1] == cmd)
}

/// Battery percent when the link reply says the mouse is up and the battery reply is valid.
pub fn parse(link: &[u8], battery: &[u8]) -> Option<u8> {
    (link[2] == 1 && battery[2] == 0x11 && battery[3] <= 100).then_some(battery[3])
}

fn poll_with(colls: &[Coll], io: &mut dyn Hid) -> Vec<Device> {
    for c in find(colls, VID, PID, |c| {
        c.iface == 1
            && c.page == 0xFF00
            && c.usage == 1
            && c.feat_len == 8
            && c.in_len == 0
            && c.out_len == 0
    }) {
        let Some(mut d) = io.open(c) else { continue };
        let Some(link) = query(d.as_mut(), io, 0x80) else {
            continue;
        };
        if link[2] != 1 {
            continue;
        }
        let Some(pct) = query(d.as_mut(), io, 0x90).and_then(|b| parse(&link, &b)) else {
            continue;
        };
        let mut s = Device::new(
            "sinowealth:258A:002F",
            "Sinowealth wireless mouse",
            Kind::Mouse,
            "2.4 GHz receiver",
        );
        s.percent = Some(pct);
        // BatteryHub sets no grace here; a receiver mouse that sleeps keeps its last value 30 min.
        s.grace = Duration::from_secs(1800);
        return vec![s];
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::hid::fake::{self, Fake, Op, iface_coll as coll};

    fn receiver() -> Coll {
        coll(VID, PID, 1, 0xFF00, 1, 0, 0, 8)
    }

    /// Answers each command with [05][cmd][link or marker][level][0..], as in sinowealth_receiver_test.
    fn device(link: u8, level: u8, marker: u8, corrupt: bool) -> Fake {
        let f = Fake::with(move |s, fc, op| {
            if let Some(q) = op.sent() {
                assert!(matches!(op, Op::SetFeature(_)));
                assert!(q.len() == 8 && q[0] == 5 && (q[1] == 0x80 || q[1] == 0x90));
                assert!(q[2..].iter().all(|b| *b == 0));
                let mut r = vec![
                    5,
                    q[1],
                    if q[1] == 0x80 { link } else { marker },
                    level,
                    0,
                    0,
                    0,
                    0,
                ];
                if corrupt {
                    r[1] ^= 1;
                }
                s.push_feature("", r);
            }
            fake::queued(s, fc, op)
        });
        f.s().pad_features = true;
        f
    }

    #[test]
    fn reads_level_from_valid_replies() {
        for n in [0u8, 94, 100, 101] {
            let mut f = device(1, n, 0x11, false);
            let out = poll_with(&[receiver()], &mut f);
            assert_eq!(out.len(), usize::from(n <= 100));
            if n <= 100 {
                assert_eq!(out[0].percent, Some(n));
                assert!(!out[0].charging);
                assert_eq!(out[0].key, "sinowealth:258A:002F");
            }
        }
    }

    #[test]
    fn rejects_link_down_and_bad_replies() {
        let mut f = device(0, 94, 0x11, false);
        assert!(poll_with(&[receiver()], &mut f).is_empty());
        assert_eq!(f.s().sends(), 1, "no battery query when the link is down");
        assert!(poll_with(&[receiver()], &mut device(1, 94, 0x10, false)).is_empty());
        assert!(poll_with(&[receiver()], &mut device(1, 94, 0x11, true)).is_empty());
        let mut f = device(1, 94, 0x11, false);
        f.s().refuse_open = true;
        assert!(poll_with(&[receiver()], &mut f).is_empty());
        assert!(f.s().sends() == 0);
    }

    #[test]
    fn other_collections_are_never_opened() {
        for fault in 0..8 {
            let mut c = receiver();
            match fault {
                0 => c.vid += 1,
                1 => c.pid += 1,
                2 => c.iface += 1,
                3 => c.page += 1,
                4 => c.usage += 1,
                5 => c.feat_len += 1,
                6 => c.in_len += 1,
                _ => c.out_len += 1,
            }
            let mut f = device(1, 94, 0x11, false);
            assert!(poll_with(&[c], &mut f).is_empty());
            assert_eq!(f.s().opened.len(), 0);
        }
    }
}

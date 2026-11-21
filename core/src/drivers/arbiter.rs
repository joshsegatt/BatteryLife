//! Arbiter Studio Akitsu mice on the shared DriverCore vendor id 373E: Akitsu (cable 0009, 8K
//! receiver 000A), Akitsu 3950 (0038 / 0039) and Akitsu Medium (0066 / 0067). Vendor collection
//! FFFF with a 65-byte feature report 0:
//!   SET_FEATURE [00][00][00][02][02][00][83][00 ...], ~100 ms, GET_FEATURE 0 ->
//!   [00][status][..][..][02][..][83][charging][percent] (or without the leading id byte).
//!   status A1 / A2 = answered, A0 = asleep; percent 0 from a receiver = mouse off.
//!
//! 373E is shared with LAMZU, so only the Akitsu ids are declared. Cable and receiver share one key,
//! and an answering cable hides its paired receiver.
//!
//! Ported from BatteryHub (src/drivers/arbiter.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0, from Arbiter Web HUB
//! (webapp.arbiterstudio.com, env-models.json and getBatPer() in the DriverCore bundle).

use std::time::{Duration, Instant};

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::hid::{self, BUDGET, Coll, Dev, Hid, find, hex4, left};
use crate::registry::Match;

const VID: u16 = 0x373E;

/// (pid, receiver, name, paired receiver pid). Cables come first: an answering cable claims its
/// paired receiver so the mouse is listed once.
const MODELS: [(u16, bool, &str, u16); 6] = [
    (0x0009, false, "Arbiter Studio Akitsu", 0x000A),
    (0x0038, false, "Arbiter Studio Akitsu 3950", 0x0039),
    (0x0066, false, "Arbiter Studio Akitsu Medium", 0x0067),
    (0x000A, true, "Arbiter Studio Akitsu", 0),
    (0x0039, true, "Arbiter Studio Akitsu 3950", 0),
    (0x0067, true, "Arbiter Studio Akitsu Medium", 0),
];

pub static MATCHES: &[Match] = &[Match {
    vid: VID,
    pids: &[0x0009, 0x000A, 0x0038, 0x0039, 0x0066, 0x0067],
}];

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    hid::run(api, infos, |hid, colls| poll_with(colls, hid))
}

#[derive(Debug, PartialEq)]
pub enum Reply {
    /// percent, charging
    Battery(u8, bool),
    Asleep,
}

pub fn request() -> [u8; 65] {
    let mut q = [0u8; 65];
    q[3] = 0x02;
    q[4] = 0x02;
    q[6] = 0x83;
    q
}

pub fn decode(r: &[u8]) -> Option<Reply> {
    let (status, chg, level) = if r.len() >= 9 && r[4] == 0x02 && r[6] == 0x83 {
        (r[1], r[7], r[8])
    } else if r.len() >= 8 && r[3] == 0x02 && r[5] == 0x83 {
        (r[0], r[6], r[7])
    } else {
        return None;
    };
    match status {
        0xA0 => Some(Reply::Asleep),
        0xA1 | 0xA2 if level <= 100 => Some(Reply::Battery(level, chg == 0x01)),
        _ => None,
    }
}

fn query(d: &mut dyn Dev, io: &mut dyn Hid) -> Option<Reply> {
    for _ in 0..2 {
        if !d.set_feature(&request()) {
            return None;
        }
        for read in 0..3 {
            io.sleep(Duration::from_millis(if read == 0 { 100 } else { 60 }));
            if let Some(rep) = decode(&d.get_feature(0)?) {
                return Some(rep);
            }
        }
    }
    None
}

fn is_channel(c: &Coll) -> bool {
    c.page == 0xFFFF && c.feat_len == 65
}

fn poll_with(colls: &[Coll], io: &mut dyn Hid) -> Vec<Device> {
    let end = Instant::now() + BUDGET;
    let mut out = Vec::new();
    let mut claimed: Vec<u16> = Vec::new();
    for (pid, receiver, name, receiver_pid) in MODELS {
        if claimed.contains(&pid) {
            continue;
        }
        let mut cs = find(colls, VID, pid, is_channel);
        cs.sort_by_key(|c| c.usage != 0);
        for c in cs {
            if left(end).is_zero() {
                return out;
            }
            let Some(mut d) = io.open(c) else { continue };
            let Some(rep) = query(d.as_mut(), io) else {
                continue;
            };
            // One key for cable and receiver, so plugging in the cable updates the entry in place.
            let key_pid = if receiver_pid != 0 { receiver_pid } else { pid };
            let mut s = Device::new(
                format!("arbiter:{}:{}", hex4(VID), hex4(key_pid)),
                name,
                Kind::Mouse,
                if receiver {
                    "2.4 GHz receiver"
                } else {
                    "USB cable"
                },
            );
            if receiver || !find(colls, VID, receiver_pid, is_channel).is_empty() {
                s.grace = Duration::from_secs(1800);
            }
            match rep {
                Reply::Asleep => s.asleep = true,
                Reply::Battery(pct, charging) => {
                    s.charging = charging;
                    // 0 from a dongle = mouse off or out of range (BatteryHub hides it).
                    if pct > 0 || !receiver {
                        s.percent = Some(pct);
                    }
                }
            }
            if s.percent.is_some() || s.asleep || !receiver {
                out.push(s);
            }
            if receiver_pid != 0 {
                claimed.push(receiver_pid);
            }
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::hid::fake::{self, Fake, Op, iface_coll as coll};

    fn akitsu(pid: u16) -> Coll {
        coll(VID, pid, 2, 0xFFFF, 0, 0, 0, 65)
    }

    fn reply(status: u8, charging: u8, percent: u8, with_id: bool) -> Vec<u8> {
        let mut r = vec![0u8; 65];
        let at = usize::from(with_id);
        r[at] = status;
        r[at + 2] = 0x02;
        r[at + 3] = 0x02;
        r[at + 5] = 0x83;
        r[at + 6] = charging;
        r[at + 7] = percent;
        r
    }

    /// Fake that only accepts the read-only 0x83 status query and returns `replies` in order.
    fn device(replies: Vec<Vec<u8>>) -> Fake {
        let f = Fake::with(|s, fc, op| {
            if let Some(q) = op.sent() {
                assert!(matches!(op, Op::SetFeature(_)));
                assert_eq!(q, request());
            }
            fake::queued(s, fc, op)
        });
        f.s().pad_features = true;
        for r in replies {
            f.s().push_feature("", r);
        }
        f
    }

    // Cases from arbiter_driver_test.cpp.
    #[test]
    fn medium_receiver() {
        let mut f = device(vec![reply(0xA1, 0, 84, true)]);
        let out = poll_with(&[akitsu(0x0067)], &mut f);
        assert_eq!(out.len(), 1);
        let s = &out[0];
        assert_eq!(s.key, "arbiter:373E:0067");
        assert_eq!(
            (s.name.as_str(), s.via.as_str()),
            ("Arbiter Studio Akitsu Medium", "2.4 GHz receiver")
        );
        assert_eq!((s.percent, s.charging, s.asleep), (Some(84), false, false));
        assert_eq!(s.grace, Duration::from_secs(1800));
        let st = f.s();
        assert_eq!((st.sends(), st.feature_reads()), (1, 1));
    }

    #[test]
    fn every_model_on_receiver_and_cable() {
        for (pid, name, via, headerless) in [
            (0x000A, "Arbiter Studio Akitsu", "2.4 GHz receiver", false),
            (0x0009, "Arbiter Studio Akitsu", "USB cable", true),
            (
                0x0039,
                "Arbiter Studio Akitsu 3950",
                "2.4 GHz receiver",
                false,
            ),
            (0x0038, "Arbiter Studio Akitsu 3950", "USB cable", false),
            (0x0066, "Arbiter Studio Akitsu Medium", "USB cable", true),
        ] {
            let out = poll_with(
                &[akitsu(pid)],
                &mut device(vec![reply(0xA1, 1, 95, !headerless)]),
            );
            assert_eq!(out.len(), 1);
            assert_eq!((out[0].name.as_str(), out[0].via.as_str()), (name, via));
            assert_eq!((out[0].percent, out[0].charging), (Some(95), true));
        }
    }

    #[test]
    fn cable_wins_over_its_receiver() {
        let mut f = device(vec![reply(0xA1, 1, 62, true)]);
        let out = poll_with(&[akitsu(0x0067), akitsu(0x0066)], &mut f);
        assert_eq!(out.len(), 1);
        assert_eq!(
            (out[0].key.as_str(), out[0].via.as_str()),
            ("arbiter:373E:0067", "USB cable")
        );
        assert_eq!(out[0].grace, Duration::from_secs(1800));
        assert_eq!(f.s().opened.len(), 1);
    }

    #[test]
    fn asleep_and_switched_off() {
        let out = poll_with(
            &[akitsu(0x0067)],
            &mut device(vec![reply(0xA0, 0, 0, true)]),
        );
        assert!(out.len() == 1 && out[0].asleep && out[0].percent.is_none());
        let out = poll_with(
            &[akitsu(0x0067)],
            &mut device(vec![reply(0xA1, 0, 0, true)]),
        );
        assert!(out.is_empty(), "receiver present, mouse off: hidden");
    }

    #[test]
    fn echoed_request_gives_up_quickly() {
        let echo = request().to_vec();
        let mut f = device(vec![echo; 6]);
        let start = Instant::now();
        let out = poll_with(&[akitsu(0x0067)], &mut f);
        assert!(out.is_empty() && start.elapsed() < Duration::from_secs(1));
        let st = f.s();
        assert_eq!((st.sends(), st.feature_reads()), (2, 6));
    }

    #[test]
    fn strict_matching() {
        let mut f = device(vec![]);
        let out = poll_with(
            &[
                coll(0x1234, 0x0067, 2, 0xFFFF, 0, 0, 0, 65), // wrong VID
                akitsu(0x001E),                               // LAMZU PID
                coll(VID, 0x0067, 2, 0x0001, 2, 0, 0, 65),    // standard mouse page
                coll(VID, 0x0067, 2, 0xFFFF, 0, 0, 0, 64),    // wrong feature length
            ],
            &mut f,
        );
        assert!(out.is_empty());
        assert_eq!(f.s().opened.len(), 0);
    }
}

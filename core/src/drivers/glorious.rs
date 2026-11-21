//! Glorious (Sinowealth 258A), Hitscan (3770) and the WLMouse YING (36A7:A874) wireless mice, on one
//! supplier protocol: SET_FEATURE report 0 (64 bytes) [3]=02 [4]=02 [6]=83, then GET_FEATURE report
//! 0 -> [1] status, [6] 83, [7] charging (1), [8] percent. Status A1 awake, A4 asleep, A0 waking up
//! (asked again once), anything else = not this protocol. Only this read is sent.
//!
//! G-Wolves (33E4) mice speak either this protocol or Kysona's, and BatteryHub tells them apart by
//! probing both; a declarative registry cannot, so 33E4 is left to the Kysona port.
//!
//! Ported from BatteryHub (src/drivers/glorious.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0, which credits
//! glorious-indicator, glorious-ctl and Hitscan's web configurator.

use std::time::Duration;

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind, guess_kind};
use crate::drivers::hid::{self, Coll, Hid, find, products};

/// Sinowealth's vendor id, used by Glorious and other brands on the same chip.
pub const VID_SINOWEALTH: u16 = 0x258A;
pub const VID_HITSCAN: u16 = 0x3770;
pub const HITSCAN_PIDS: &[u16] = &[0x0136, 0x0194];
pub const VID_WLMOUSE: u16 = 0x36A7;
pub const WLMOUSE_YING_PIDS: &[u16] = &[0xA874];

struct Vendor {
    vid: u16,
    key_prefix: &'static str,
    reply_delay_ms: u64,
    name: fn(u16) -> Option<&'static str>,
    is_cable: fn(u16) -> bool,
    matches: fn(&Coll) -> bool,
}

fn glorious_name(pid: u16) -> Option<&'static str> {
    Some(match pid {
        0x2022 | 0x2011 => "Glorious Model O Wireless",
        0x2023 | 0x2012 => "Glorious Model D Wireless",
        0x2024 | 0x2013 => "Glorious Model O- Wireless",
        0x2025 | 0x2014 => "Glorious Model D- Wireless",
        0x2033 => "Glorious Model O 2 Wireless",
        0x2034 | 0x2036 | 0x201A => "Glorious Model D 2 PRO",
        _ => return None, // another brand on the same Sinowealth chip (e.g. Redragon keyboards)
    })
}

fn glorious_cable(pid: u16) -> bool {
    matches!(pid, 0x2011 | 0x2012 | 0x2013 | 0x2014 | 0x201A)
}

fn hitscan_name(pid: u16) -> Option<&'static str> {
    Some(match pid {
        0x0136 => "Hitscan Hyperlight X",
        0x0194 => "Hitscan Hyperlight N",
        _ => "Hitscan Hyperlight",
    })
}

fn hitscan_cable(pid: u16) -> bool {
    pid == 0x0136 || pid == 0x0194
}

const VENDORS: &[Vendor] = &[
    Vendor {
        vid: VID_SINOWEALTH,
        key_prefix: "glorious:",
        reply_delay_ms: 50,
        name: glorious_name,
        is_cable: glorious_cable,
        matches: |_| true,
    },
    Vendor {
        vid: VID_HITSCAN,
        key_prefix: "hitscan:",
        reply_delay_ms: 200,
        name: hitscan_name,
        is_cable: hitscan_cable,
        matches: |_| true,
    },
    Vendor {
        vid: VID_WLMOUSE,
        key_prefix: "wlmouse:",
        reply_delay_ms: 100,
        name: |_| Some("WLMouse YING"),
        is_cable: |_| false,
        // The 1K dongle is shared with other models; only the YING 8K receiver's battery collection.
        matches: |c| c.pid == 0xA874 && c.page == 0xFFFF && c.usage == 0,
    },
];

/// The SET_FEATURE buffer: report id 0, then the 64-byte request.
pub fn request() -> Vec<u8> {
    let mut w = vec![0u8; 65];
    w[3] = 0x02;
    w[4] = 0x02;
    w[6] = 0x83;
    w
}

#[derive(Debug, PartialEq)]
pub enum Reply {
    /// Not this protocol.
    Foreign,
    WakingUp,
    /// Answered: (awake, charging bit, percent).
    Status {
        awake: bool,
        charging: bool,
        pct: u8,
    },
}

/// Classifies a GET_FEATURE buffer (report id first).
pub fn parse(r: &[u8]) -> Reply {
    if r.len() < 9 || r[6] != 0x83 || r[1] & 0xF0 != 0xA0 {
        return Reply::Foreign;
    }
    if r[1] == 0xA0 {
        return Reply::WakingUp;
    }
    Reply::Status {
        awake: r[1] == 0xA1,
        charging: r[7] == 0x01,
        pct: r[8],
    }
}

/// 258A is shared by many brands: for an unknown product use its name, and its collections (keyboards
/// expose boot + NKRO keyboard collections) instead of assuming a mouse.
fn sinowealth_kind(colls: &[Coll], vid: u16, pid: u16, name: &str) -> Kind {
    let k = guess_kind(name);
    if k != Kind::Other {
        return k;
    }
    let keyboards = find(colls, vid, pid, |c| c.page == 0x01 && c.usage == 0x06).len();
    if keyboards >= 2 {
        return Kind::Keyboard;
    }
    hid::kind_from_colls(colls, vid, pid)
}

fn poll_with(hid: &mut dyn Hid, colls: &[Coll]) -> Vec<Device> {
    let mut out = Vec::new();
    for v in VENDORS {
        for (vid, pid) in products(colls).into_iter().filter(|p| p.0 == v.vid) {
            for c in find(colls, vid, pid, |c| c.feat_len == 65 && (v.matches)(c)) {
                let Some(mut d) = hid.open(c) else { continue };
                let mut reply = None;
                for attempt in 0..2 {
                    if !d.set_feature(&request()) {
                        break;
                    }
                    hid.sleep(Duration::from_millis(v.reply_delay_ms));
                    let Some(r) = d.get_feature(0) else { break };
                    match parse(&r) {
                        Reply::Foreign => break,
                        Reply::WakingUp if attempt == 0 => {
                            hid.sleep(Duration::from_millis(400)); // ask again
                        }
                        Reply::WakingUp => {
                            reply = Some((false, false, 0));
                            break;
                        }
                        Reply::Status {
                            awake,
                            charging,
                            pct,
                        } => {
                            reply = Some((awake, charging, pct));
                            break;
                        }
                    }
                }
                let Some((awake, charging, pct)) = reply else {
                    continue;
                };
                let cable = (v.is_cable)(pid);
                let (name, kind) = match (v.name)(pid) {
                    Some(n) => (n.to_string(), Kind::Mouse),
                    None => {
                        let n = if c.product.is_empty() {
                            "Wireless device".to_string()
                        } else {
                            c.product.clone()
                        };
                        let k = sinowealth_kind(colls, vid, pid, &n);
                        (n, k)
                    }
                };
                let mut s = Device::new(
                    format!("{}{pid:04X}", v.key_prefix),
                    name,
                    kind,
                    if cable {
                        "USB cable"
                    } else {
                        "2.4 GHz receiver"
                    },
                );
                if !cable {
                    s.grace = Duration::from_secs(30 * 60);
                }
                s.charging = cable || charging;
                if awake && pct <= 100 {
                    s.percent = Some(pct.max(1));
                } else {
                    s.asleep = true;
                }
                out.push(s);
                break;
            }
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

    fn reply(status: u8, charging: u8, pct: u8) -> Vec<u8> {
        let mut r = vec![0u8; 65];
        r[1] = status;
        r[3] = 0x02;
        r[4] = 0x02;
        r[6] = 0x83;
        r[7] = charging;
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

    fn coll(vid: u16, pid: u16) -> Coll {
        Coll {
            vid,
            pid,
            feat_len: 65,
            page: 0xFFFF,
            ..Default::default()
        }
    }

    #[test]
    fn model_o_on_its_receiver() {
        let out = poll_with(&mut fake(vec![reply(0xA1, 0, 72)]), &[coll(0x258A, 0x2022)]);
        let s = &out[0];
        assert_eq!(s.name, "Glorious Model O Wireless");
        assert_eq!((s.percent, s.charging, s.asleep), (Some(72), false, false));
        assert_eq!(
            (s.key.as_str(), s.via.as_str()),
            ("glorious:2022", "2.4 GHz receiver")
        );
    }

    #[test]
    fn waking_up_is_asked_again_then_asleep_keeps_last_value() {
        let mut f = fake(vec![reply(0xA0, 0, 0), reply(0xA1, 1, 0)]);
        let out = poll_with(&mut f, &[coll(0x258A, 0x2033)]);
        assert_eq!(f.count(|o| matches!(o, Op::SetFeature(_))), 2);
        // 0% while awake is shown as 1%, and charging comes from [7].
        assert_eq!((out[0].percent, out[0].charging), (Some(1), true));
        let out = poll_with(&mut fake(vec![reply(0xA4, 0, 50)]), &[coll(0x258A, 0x2033)]);
        assert!(out[0].asleep && out[0].percent.is_none());
    }

    #[test]
    fn cable_charges_and_unknown_sinowealth_uses_its_own_name() {
        let out = poll_with(&mut fake(vec![reply(0xA1, 0, 90)]), &[coll(0x258A, 0x2011)]);
        assert_eq!((out[0].via.as_str(), out[0].charging), ("USB cable", true));
        let mut c = coll(0x258A, 0x0049);
        c.product = "Redragon Keyboard".into();
        let out = poll_with(&mut fake(vec![reply(0xA1, 0, 40)]), &[c]);
        assert_eq!(
            (out[0].name.as_str(), out[0].kind),
            ("Redragon Keyboard", Kind::Keyboard)
        );
    }

    #[test]
    fn foreign_reply_and_wrong_collections_are_ignored() {
        let mut r = reply(0xA1, 0, 50);
        r[6] = 0x84;
        assert!(poll_with(&mut fake(vec![r]), &[coll(0x258A, 0x2022)]).is_empty());
        let mut f = fake(vec![]);
        let mut short = coll(0x258A, 0x2022);
        short.feat_len = 64;
        let mut ying = coll(0x36A7, 0xA874);
        ying.usage = 1;
        assert!(poll_with(&mut f, &[short, ying]).is_empty());
        assert_eq!(f.s().opened.len(), 0);
        let out = poll_with(&mut fake(vec![reply(0xA1, 0, 61)]), &[coll(0x36A7, 0xA874)]);
        assert_eq!(
            (out[0].name.as_str(), out[0].key.as_str()),
            ("WLMouse YING", "wlmouse:A874")
        );
    }
}

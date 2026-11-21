//! Razer wireless mice, keyboards and their dongles (VID 1532), over the 90-byte feature report.
//!
//! Report (report id 0): [0] status [1] transaction id [2..3] remaining packets [4] protocol type
//! [5] data size [6] command class [7] command id [8..87] arguments [88] crc (xor of 2..87).
//! Battery: class 07 id 80 -> args[1] = 0..255. Charging: class 07 id 84 -> args[1] = 1.
//! Reply status: 01 busy, 02 ok, 03 failed, 04 timeout (device offline), 05 not supported.
//! Only these two reads are sent. Product ids are OpenRazer's keyboard and mouse lists; Razer
//! headsets share the vendor id and have their own driver.
//!
//! Ported from BatteryHub (src/drivers/razer.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0, which credits OpenRazer.

use std::time::Duration;

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind, guess_kind};
use crate::drivers::hid::{self, Coll, Dev, Hid, find, products};

pub const VID: u16 = 0x1532;

/// OpenRazer keyboards, keypads and laptops (razerkbd_driver.h).
pub const KEYBOARD_PIDS: &[u16] = &[
    0x010D, 0x010E, 0x010F, 0x0111, 0x0113, 0x0118, 0x011A, 0x011B, 0x011C, 0x0201, 0x0202, 0x0203,
    0x0204, 0x0205, 0x0207, 0x0208, 0x0209, 0x020F, 0x0210, 0x0211, 0x0214, 0x0216, 0x0217, 0x021A,
    0x021E, 0x021F, 0x0220, 0x0221, 0x0224, 0x0225, 0x0226, 0x0227, 0x0228, 0x022A, 0x022B, 0x022C,
    0x022D, 0x022F, 0x0232, 0x0233, 0x0234, 0x0235, 0x0237, 0x0239, 0x023A, 0x023B, 0x023F, 0x0240,
    0x0241, 0x0243, 0x0244, 0x0245, 0x0246, 0x024A, 0x024B, 0x024C, 0x024D, 0x024E, 0x0252, 0x0253,
    0x0255, 0x0256, 0x0257, 0x0258, 0x0259, 0x025A, 0x025C, 0x025D, 0x025E, 0x0266, 0x0268, 0x0269,
    0x026A, 0x026B, 0x026C, 0x026D, 0x026E, 0x026F, 0x0270, 0x0271, 0x0276, 0x0279, 0x027A, 0x0282,
    0x0287, 0x028A, 0x028B, 0x028C, 0x028D, 0x028F, 0x0290, 0x0292, 0x0293, 0x0294, 0x0295, 0x0296,
    0x0298, 0x029D, 0x029E, 0x029F, 0x02A0, 0x02A1, 0x02A2, 0x02A3, 0x02A5, 0x02A6, 0x02A7, 0x02B0,
    0x02B6, 0x02B8, 0x02B9, 0x02BA, 0x02C5, 0x02C6, 0x02C7, 0x02C9, 0x02CD, 0x02CF, 0x02D2, 0x02D4,
    0x02D5, 0x02D7, 0x02E0, 0x02E1, 0x02EA, 0x0A24,
];

/// OpenRazer mice and mouse receivers (razermouse_driver.h).
pub const MOUSE_PIDS: &[u16] = &[
    0x0013, 0x0015, 0x0016, 0x001F, 0x0020, 0x0024, 0x0025, 0x0029, 0x002E, 0x002F, 0x0032, 0x0034,
    0x0036, 0x0037, 0x0038, 0x0039, 0x003E, 0x003F, 0x0040, 0x0041, 0x0042, 0x0043, 0x0044, 0x0045,
    0x0046, 0x0048, 0x004C, 0x004F, 0x0050, 0x0053, 0x0054, 0x0059, 0x005A, 0x005B, 0x005C, 0x005E,
    0x005F, 0x0060, 0x0062, 0x0064, 0x0065, 0x0067, 0x006A, 0x006B, 0x006C, 0x006E, 0x006F, 0x0070,
    0x0071, 0x0072, 0x0073, 0x0077, 0x0078, 0x007A, 0x007B, 0x007C, 0x007D, 0x0080, 0x0083, 0x0084,
    0x0085, 0x0086, 0x0088, 0x008A, 0x008C, 0x008D, 0x008F, 0x0090, 0x0091, 0x0094, 0x0095, 0x0096,
    0x0098, 0x0099, 0x009A, 0x009C, 0x009E, 0x009F, 0x00A1, 0x00A3, 0x00A5, 0x00A6, 0x00A7, 0x00A8,
    0x00AA, 0x00AB, 0x00AF, 0x00B0, 0x00B2, 0x00B3, 0x00B4, 0x00B6, 0x00B7, 0x00B8, 0x00B9, 0x00BE,
    0x00BF, 0x00C0, 0x00C1, 0x00C2, 0x00C3, 0x00C4, 0x00C5, 0x00C7, 0x00C8, 0x00CB, 0x00CC, 0x00CD,
    0x00D0, 0x00D1, 0x00D3, 0x00D4, 0x00D6, 0x00D7, 0x00DA, 0x00DB, 0x00DE, 0x00DF, 0x00E0, 0x00E5,
    0x00E6,
];

/// Transaction id OpenRazer documents for a keyboard's battery reads; 0 = probe the known ids.
fn keyboard_battery_tid(pid: u16) -> u8 {
    match pid {
        0x025C | 0x0271 | 0x0290 | 0x0296 | 0x02BA | 0x02C9 | 0x02D2 | 0x02D5 => 0x9F,
        0x0258 | 0x0292 | 0x0298 | 0x02B9 | 0x02CD | 0x02D4 | 0x02D7 => 0x1F,
        0x025A => 0x3F,
        _ => 0,
    }
}

fn crc(report: &[u8]) -> u8 {
    report[2..88].iter().fold(0, |a, b| a ^ b)
}

/// The 91-byte SET_FEATURE buffer: report id 0, then the 90-byte report.
pub fn request(tid: u8, class: u8, id: u8, size: u8) -> Vec<u8> {
    let mut rep = vec![0u8; 91];
    let r = &mut rep[1..];
    r[1] = tid;
    r[5] = size;
    r[6] = class;
    r[7] = id;
    r[88] = crc(r);
    rep
}

#[derive(Debug, PartialEq)]
pub enum Reply {
    /// Not the reply to this request (Synapse shares the mailbox): ask again.
    Other,
    Busy,
    Ok(Vec<u8>),
    Offline,
    Unsupported,
}

/// Classifies a GET_FEATURE buffer (report id first) against the request it should answer.
pub fn parse_reply(resp: &[u8], tid: u8, class: u8, id: u8, size: u8) -> Reply {
    if resp.len() < 91 {
        return Reply::Other;
    }
    let a = &resp[1..];
    if a[1] != tid
        || a[2] != 0
        || a[3] != 0
        || a[4] != 0
        || a[5] != size
        || a[6] != class
        || a[7] != id
        || a[88] != crc(a)
    {
        return Reply::Other;
    }
    match a[0] {
        0x01 => Reply::Busy,
        0x02 => Reply::Ok(a[8..88].to_vec()),
        0x04 => Reply::Offline,
        0x03 | 0x05 => Reply::Unsupported,
        _ => Reply::Other,
    }
}

#[derive(Debug, PartialEq)]
enum Rz {
    Ok(Vec<u8>),
    Offline,
    Unsupported,
    NoReply,
}

fn command(hid: &mut dyn Hid, d: &mut dyn Dev, pid: u16, tid: u8, class: u8, id: u8) -> Rz {
    let (size, req) = (0x02, request(tid, class, id, 0x02));
    let mut send = true;
    let mut res = Rz::NoReply;
    for attempt in 0..8 {
        if send && !d.set_feature(&req) {
            return Rz::NoReply;
        }
        // Wireless keyboards need about 5 ms (OpenRazer); a long wait lets Synapse replace the reply.
        let wait = if keyboard_battery_tid(pid) != 0 {
            5
        } else if attempt == 0 {
            15
        } else {
            40
        };
        hid.sleep(Duration::from_millis(wait));
        send = true;
        res = Rz::NoReply;
        let Some(resp) = d.get_feature(0) else {
            continue;
        };
        match parse_reply(&resp, tid, class, id, size) {
            Reply::Other => {}
            Reply::Busy => send = false, // wait for this request to finish
            Reply::Ok(args) => return Rz::Ok(args),
            Reply::Offline => res = Rz::Offline, // retry a transient receiver timeout
            Reply::Unsupported => return Rz::Unsupported,
        }
    }
    res
}

/// Product name without the connection suffix, so cable and dongle of one device match.
pub fn clean_name(name: &str) -> String {
    let mut n = name.to_string();
    for suffix in [
        " Dongle",
        " (Wired)",
        " (Wireless)",
        " Wireless Receiver",
        " Receiver",
        " HyperSpeed Dongle",
    ] {
        if n.len() > suffix.len() && n.ends_with(suffix) {
            n.truncate(n.len() - suffix.len());
        }
    }
    if n.is_empty() {
        n = "Razer device".into();
    }
    n
}

/// args[1] (0..255) to percent, rounded.
pub fn percent(raw: u8) -> u8 {
    ((u32::from(raw) * 100 + 127) / 255).min(100) as u8
}

fn kind(colls: &[Coll], vid: u16, pid: u16, name: &str) -> Kind {
    // Both keyboards and mice can expose keyboard AND mouse collections for macros / buttons.
    if vid == VID && KEYBOARD_PIDS.contains(&pid) {
        return Kind::Keyboard;
    }
    if vid == VID && MOUSE_PIDS.contains(&pid) {
        return Kind::Mouse;
    }
    match hid::kind_from_colls(colls, vid, pid) {
        Kind::Other => guess_kind(name),
        k => k,
    }
}

fn poll_with(hid: &mut dyn Hid, colls: &[Coll]) -> Vec<Device> {
    // An offline receiver (device switched off or out of range) is listed as off, unless a live
    // reading of the same device (its cable) replaces it.
    let mut found: Vec<Device> = Vec::new();
    for (vid, pid) in products(colls) {
        for c in find(colls, vid, pid, |c| c.feat_len == 91) {
            let Some(mut d) = hid.open(c) else { continue };
            let kb_tid = keyboard_battery_tid(pid);
            let tids: &[u8] = if kb_tid != 0 {
                &[kb_tid]
            } else {
                &[0x1F, 0x3F, 0xFF, 0x08, 0x9F]
            };
            let mut ok: Option<(u8, Vec<u8>)> = None;
            let mut saw_offline = false;
            for &t in tids {
                match command(hid, d.as_mut(), pid, t, 0x07, 0x80) {
                    Rz::Ok(args) => {
                        ok = Some((t, args));
                        break;
                    }
                    // May also mean "wrong transaction id": keep trying.
                    r => saw_offline |= r == Rz::Offline,
                }
            }
            if ok.is_none() && !saw_offline {
                continue;
            }

            let name = clean_name(&c.product);
            let mut s = Device::new(
                format!("razer:{pid:04X}"),
                name.as_str(),
                kind(colls, vid, pid, &name),
                "Razer receiver",
            );
            // Keep the last reading briefly when Synapse prevents a valid reply.
            s.grace = Duration::from_secs(120);
            s.off = ok.is_none();
            if let Some((tid, args)) = ok {
                s.percent = Some(percent(args[1]));
                if let Rz::Ok(ch) = command(hid, d.as_mut(), pid, tid, 0x07, 0x84) {
                    s.charging = ch[1] == 1;
                }
            }
            // Cable + receiver can report the same device; keep a live reading over an offline one.
            match found.iter_mut().find(|o| o.name == s.name) {
                None => found.push(s),
                Some(existing) => {
                    if existing.off && !s.off {
                        existing.off = false;
                        existing.percent = s.percent;
                    }
                    existing.charging |= s.charging;
                }
            }
            break;
        }
    }
    found
}

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    hid::run(api, infos, poll_with)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::hid::fake::{Fake, Op};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Copy)]
    struct State {
        online: bool,
        raw: u8,
        charging: bool,
    }

    fn coll(pid: u16) -> Coll {
        Coll {
            vid: VID,
            pid,
            feat_len: 91,
            product: "Razer Test Keyboard".into(),
            path: format!("razer-{pid:04X}"),
            ..Default::default()
        }
    }

    /// The feature-report mailbox of razer_connection_test.cpp: the reply echoes the last request
    /// with the status and args[1] filled in and the crc recomputed.
    fn mailbox(states: HashMap<u16, State>) -> Fake {
        let request = Arc::new(Mutex::new(Vec::new()));
        Fake::with(move |_, c, op| match op.clone() {
            Op::SetFeature(q) => {
                assert!(q.len() == 91 && q[7] == 7 && (q[8] == 0x80 || q[8] == 0x84));
                *request.lock().unwrap() = q;
                Some(vec![])
            }
            Op::GetFeature(_, _) => {
                let mut r = request.lock().unwrap().clone();
                let s = states[&c.pid];
                r[1] = if s.online { 2 } else { 4 };
                r[10] = if r[8] == 0x80 {
                    s.raw
                } else {
                    s.charging as u8
                };
                r[89] = r[3..89].iter().fold(0, |a, b| a ^ b);
                Some(r)
            }
            _ => None,
        })
    }

    #[test]
    fn live_reading_wins_over_an_offline_receiver_of_the_same_device() {
        for reverse in [false, true] {
            for first_online in [false, true] {
                for raw in [0u8, 38, 255] {
                    let states = HashMap::from([
                        (
                            0x02D5,
                            State {
                                online: first_online,
                                raw: if first_online { raw } else { 0 },
                                charging: first_online,
                            },
                        ),
                        (
                            0x02D7,
                            State {
                                online: !first_online,
                                raw: if first_online { 0 } else { raw },
                                charging: !first_online,
                            },
                        ),
                    ]);
                    let colls = if reverse {
                        vec![coll(0x02D7), coll(0x02D5)]
                    } else {
                        vec![coll(0x02D5), coll(0x02D7)]
                    };
                    let out = poll_with(&mut mailbox(states), &colls);
                    assert_eq!(out.len(), 1);
                    let s = &out[0];
                    assert!(!s.off);
                    assert_eq!(s.percent, Some(((raw as u32 * 100 + 127) / 255) as u8));
                    assert!(s.charging);
                    assert_eq!(s.key, "razer:02D5");
                    assert_eq!(s.kind, Kind::Keyboard);
                }
            }
        }
    }

    #[test]
    fn all_offline_lists_one_switched_off_device() {
        let off = State {
            online: false,
            raw: 0,
            charging: false,
        };
        let states = HashMap::from([(0x02D5, off), (0x02D7, off)]);
        let out = poll_with(&mut mailbox(states), &[coll(0x02D5), coll(0x02D7)]);
        assert_eq!(out.len(), 1);
        // The same key the device has when it answers, with no level.
        assert_eq!((out[0].key.as_str(), out[0].off), ("razer:02D5", true));
        assert_eq!((out[0].percent, out[0].charging), (None, false));
    }

    #[test]
    fn parses_battery_reply_and_rejects_a_foreign_one() {
        let mut r = request(0x1F, 0x07, 0x80, 0x02);
        r[1] = 0x02;
        r[10] = 128;
        r[89] = crc(&r[1..]);
        assert_eq!(
            parse_reply(&r, 0x1F, 0x07, 0x80, 0x02),
            Reply::Ok(r[9..89].to_vec())
        );
        assert_eq!(percent(128), 50);
        // Another app's request in the mailbox (other transaction id).
        assert_eq!(parse_reply(&r, 0x3F, 0x07, 0x80, 0x02), Reply::Other);
        r[1] = 0x05;
        r[89] = crc(&r[1..]);
        assert_eq!(parse_reply(&r, 0x1F, 0x07, 0x80, 0x02), Reply::Unsupported);
    }

    #[test]
    fn names_lose_connection_suffix() {
        assert_eq!(
            clean_name("Razer Viper V3 Pro (Wireless)"),
            "Razer Viper V3 Pro"
        );
        assert_eq!(
            clean_name("Razer DeathAdder V3 HyperSpeed Dongle"),
            "Razer DeathAdder V3 HyperSpeed"
        );
        assert_eq!(clean_name(""), "Razer device");
    }
}

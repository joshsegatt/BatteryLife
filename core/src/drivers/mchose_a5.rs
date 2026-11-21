//! MCHOSE first-generation A5 Pro / A5 Pro Max (VID 2023): receivers F013 / F015 (shared) and F018,
//! cables F017 (A5 Pro) and F019 (A5 Pro Max). 64-byte feature reports with report id 0 on the
//! FFFF vendor collection (interface 2). Only the battery read is sent:
//!   request [00][00 00][route 02][len 02][page 00][cmd 83][00 ...]
//!   reply   [00][A1][..][..][..][page 00][cmd 83][charging][level] (id byte may be missing)
//!
//! A silent receiver means the mouse sleeps; it is shown asleep once its mouse has answered.
//!
//! Ported from BatteryHub (src/drivers/mchose_a5.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0, which cites
//! github.com/z750sasr/mchose-a5-pro-max-web-driver, github.com/HeyOkay/HaloBattery and
//! github.com/Klegus/mchose-macos.

use std::collections::HashSet;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::hid::{self, BUDGET, Coll, Hid, find, hex4, left};
use crate::registry::Match;

const VID: u16 = 0x2023;
const OK: u8 = 0xA1;
const CMD_BATTERY: u8 = 0x83;

struct Model {
    pid: u16,
    name: &'static str,
    cable: bool,
}

const MODELS: [Model; 5] = [
    Model {
        pid: 0xF013,
        name: "MCHOSE A5 Pro / Pro Max",
        cable: false,
    },
    Model {
        pid: 0xF015,
        name: "MCHOSE A5 Pro / Pro Max",
        cable: false,
    },
    Model {
        pid: 0xF017,
        name: "MCHOSE A5 Pro",
        cable: true,
    },
    Model {
        pid: 0xF018,
        name: "MCHOSE A5 Pro Max",
        cable: false,
    },
    Model {
        pid: 0xF019,
        name: "MCHOSE A5 Pro Max",
        cable: true,
    },
];

pub static MATCHES: &[Match] = &[Match {
    vid: VID,
    pids: &[0xF013, 0xF015, 0xF017, 0xF018, 0xF019],
}];

/// Receivers whose mouse has answered at least once (a silent one is then shown asleep).
static ANSWERED: Mutex<Option<HashSet<u16>>> = Mutex::new(None);

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    let mut answered = ANSWERED.lock().unwrap_or_else(|e| e.into_inner());
    hid::run(api, infos, |hid, colls| {
        poll_with(colls, hid, answered.get_or_insert_with(HashSet::new))
    })
}

pub fn request(len: usize) -> Vec<u8> {
    let mut q = vec![0u8; len.max(7)];
    q[3] = 0x02; // route: mouse
    q[4] = 2; // payload length
    q[5] = 0x00; // page
    q[6] = CMD_BATTERY;
    q
}

/// (level, charging) from a reply, with or without the report id byte.
pub fn parse(r: &[u8]) -> Option<(u8, bool)> {
    if r.len() < 9 || (r[0] != OK && r[1] != OK) {
        return None;
    }
    let at = usize::from(r[1] == OK);
    if r[at + 4] != 0 || r[at + 5] != CMD_BATTERY || r[at + 7] == 0xFF {
        return None;
    }
    Some((r[at + 7].min(100), r[at + 6] != 0))
}

/// Some(Some(..)) battery read, Some(None) collection works but the mouse is silent, None no channel.
fn query(
    d: &mut dyn crate::drivers::hid::Dev,
    io: &mut dyn Hid,
    len: usize,
) -> Option<Option<(u8, bool)>> {
    let req = request(len);
    if !d.set_feature(&req) {
        return None;
    }
    io.sleep(Duration::from_millis(34));
    for attempt in 0..8 {
        if attempt == 3 {
            if !d.set_feature(&req) {
                return None;
            }
            io.sleep(Duration::from_millis(34));
        }
        if let Some(b) = d.get_feature(0).and_then(|r| parse(&r)) {
            return Some(Some(b));
        }
        io.sleep(Duration::from_millis(30));
    }
    Some(None)
}

fn poll_with(colls: &[Coll], io: &mut dyn Hid, answered: &mut HashSet<u16>) -> Vec<Device> {
    let end = Instant::now() + BUDGET;
    let mut out = Vec::new();
    for m in &MODELS {
        let mut cs = find(colls, VID, m.pid, |c| c.page == 0xFFFF && c.feat_len >= 65);
        // Interface 2 first: that is where the references talk to it.
        cs.sort_by_key(|c| c.iface != 2);
        let mut state: Option<Option<(u8, bool)>> = None;
        for c in cs {
            if left(end).is_zero() {
                return out;
            }
            let Some(mut d) = io.open(c) else { continue };
            let st = query(d.as_mut(), io, c.feat_len as usize);
            if st.is_some() && state.is_none_or(|s| s.is_none()) {
                state = st;
            }
            if matches!(state, Some(Some(_))) {
                break;
            }
        }
        let Some(reading) = state else { continue };
        let mut s = Device::new(
            format!("mchose-a5:{}", hex4(m.pid)),
            m.name,
            Kind::Mouse,
            if m.cable {
                "USB cable"
            } else {
                "2.4 GHz receiver"
            },
        );
        match reading {
            Some((level, charging)) => {
                answered.insert(m.pid);
                s.percent = Some(level);
                s.charging = charging;
            }
            None if m.cable || !answered.contains(&m.pid) => continue,
            None => s.asleep = true,
        }
        if !m.cable {
            // BatteryHub keeps the last value while asleep; 30 min like other receivers.
            s.grace = Duration::from_secs(1800);
        }
        out.push(s);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::hid::fake::{self, Fake, iface_coll as coll};

    fn reply(charging: u8, level: u8, with_id: bool) -> Vec<u8> {
        let mut r = vec![0u8; 65];
        let at = usize::from(with_id);
        r[at] = OK;
        r[at + 5] = CMD_BATTERY;
        r[at + 6] = charging;
        r[at + 7] = level;
        r
    }

    #[test]
    fn parses_with_and_without_report_id() {
        assert_eq!(parse(&reply(0, 72, true)), Some((72, false)));
        assert_eq!(parse(&reply(1, 72, false)), Some((72, true)));
        assert_eq!(
            parse(&reply(0, 120, true)),
            Some((100, false)),
            "clamped like the reference"
        );
        assert!(parse(&reply(0, 0xFF, true)).is_none());
        let mut unsupported = reply(0, 50, true);
        unsupported[1] = 0xA3;
        assert!(parse(&unsupported).is_none());
    }

    #[test]
    fn silent_receiver_is_asleep_only_after_an_answer() {
        let rx = coll(VID, 0xF013, 2, 0xFFFF, 0, 0, 0, 65);
        let mut answered = HashSet::new();
        let mut silent = Fake::new();
        assert!(poll_with(std::slice::from_ref(&rx), &mut silent, &mut answered).is_empty());

        let mut f = Fake::with(|s, fc, op| {
            if let Some(q) = op.sent() {
                assert_eq!(q[..7], [0, 0, 0, 2, 2, 0, 0x83]);
                s.push_feature("", reply(0, 66, true));
            }
            fake::queued(s, fc, op)
        });
        f.s().pad_features = true;
        let out = poll_with(std::slice::from_ref(&rx), &mut f, &mut answered);
        assert_eq!(out[0].percent, Some(66));

        let out = poll_with(&[rx], &mut Fake::new(), &mut answered);
        assert!(out[0].asleep && out[0].percent.is_none());
    }
}

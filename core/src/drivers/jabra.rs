//! Jabra headsets speaking Jabra's GN Protocol (GNP), on their own USB cable or behind a Jabra Link
//! dongle. GNP runs on the vendor collection FF00 usage 05 (Link 390, Evolve3) or 01 (Link 380,
//! Evolve2), report 05, 64 bytes. Every Jabra product with that collection is asked (DECT bases
//! excepted), so the whole vendor id is matched.
//!   query: [05][dst][src 00][seq][40|len][class][op]     reply: [05][00][src][seq][C0|len][class][op][payload]
//!   class FE = NAK (op = reason). Addresses: 08 = headset on its cable, 04 = headset behind a dongle.
//!   Battery = class 12 / op 02, payload [flags][level], flags bit 0 = charging.
//!   Name = class 02 / op 00, payload [n][n bytes UTF-8].
//!
//! Ported from BatteryHub (src/drivers/jabra.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0, which credits
//! Watchdog0x/jabridge and HeadsetControl's GNP driver.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::hid_io::{self, Coll, Io, Open, find, products};

pub const VID: u16 = 0x0B0E;
const DONGLE_HEADSET: u8 = 0x04;
const USB_HEADSET: u8 = 0x08;
const NAK_PEER_OFFLINE: u8 = 0xF3;
const SILENT_RETRY: Duration = Duration::from_secs(5 * 60);

/// DECT bases and dongles (Engage 65 / 75, PRO 9xx, GO 6400, Link 400, ...), from Jabra Direct's
/// DeviceInfo.xml; sorted.
const DECT_BASES: &[u16] = &[
    0x1000, 0x1016, 0x1017, 0x1018, 0x1021, 0x1022, 0x1023, 0x1037, 0x1038, 0x1039, 0x1041, 0x1042,
    0x1043, 0x1110, 0x1112, 0x1113, 0x1118, 0x111A, 0x111B, 0x111D, 0x111E, 0x111F, 0x1120, 0x1121,
    0x1122, 0x1123, 0x1124, 0x1126, 0x1131, 0x1132, 0x1133, 0x1134, 0x1135, 0x1136, 0x1140, 0x1141,
    0x1142, 0x1146, 0x1147, 0x1148, 0x114A, 0x114B, 0x114C, 0x114D, 0x114E, 0x114F, 0x1150, 0x1151,
    0xA700, 0xA701, 0xA702, 0xA703, 0xA704, 0xA705, 0xA706, 0xAE60, 0xAE62, 0xAE63,
];

#[derive(Debug, PartialEq)]
pub enum Reply {
    Ok(Vec<u8>),
    Nak(u8),
    None,
}

/// Kept across polls.
pub struct State {
    seq: u8,
    /// Headset address per product, kept once a headset answered there.
    addresses: Vec<(u16, u8)>,
    /// Products not asked before this time (None = never again: every reply was a NAK).
    skip: Vec<(u16, Option<Instant>)>,
    /// Headset name per product, asked once while connected.
    names: Vec<(u16, String)>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            seq: 0x20,
            addresses: Vec::new(),
            skip: Vec::new(),
            names: Vec::new(),
        }
    }
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn get<T: Clone>(list: &[(u16, T)], pid: u16) -> Option<T> {
    list.iter().find(|(p, _)| *p == pid).map(|(_, v)| v.clone())
}
fn set<T>(list: &mut Vec<(u16, T)>, pid: u16, v: T) {
    list.retain(|(p, _)| *p != pid);
    list.push((pid, v));
}

/// Classifies one input report as the reply to (dst, seq, cls, op).
pub fn parse_reply(r: &[u8], dst: u8, seq: u8, cls: u8, op: u8) -> Option<Reply> {
    if r.len() < 7 || r[0] != 0x05 || r[1] != 0x00 || r[2] != dst || r[3] != seq {
        return None;
    }
    if r[4] & 0xC0 != 0xC0 {
        return None;
    }
    if r[5] == 0xFE {
        return Some(Reply::Nak(r[6]));
    }
    let len = usize::from(r[4] & 0x3F); // counts from r[1]
    if r[5] != cls || r[6] != op || len < 6 || len + 1 > r.len() {
        return None;
    }
    Some(Reply::Ok(r[7..=len].to_vec()))
}

fn query(io: &mut dyn Io, st: &mut State, dst: u8, cls: u8, op: u8, timeout_ms: u64) -> Reply {
    let seq = st.seq;
    st.seq = st.seq.wrapping_add(1);
    io.flush();
    if !io.write(&[0x05, dst, 0x00, seq, 0x46, cls, op]) {
        return Reply::None;
    }
    let end = Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        let left = end.saturating_duration_since(Instant::now()).as_millis() as u64;
        if left == 0 {
            return Reply::None;
        }
        let Some(r) = io.read(left) else {
            return Reply::None;
        };
        if let Some(reply) = parse_reply(&r, dst, seq, cls, op) {
            return reply;
        }
    }
}

fn headset_name(io: &mut dyn Io, st: &mut State, address: u8) -> Option<String> {
    let Reply::Ok(p) = query(io, st, address, 0x02, 0x00, 400) else {
        return None;
    };
    let n = usize::from(*p.first()?);
    if n == 0 || n + 1 > p.len() {
        return None;
    }
    Some(String::from_utf8_lossy(&p[1..=n]).into_owned())
}

pub fn poll_colls(colls: &[Coll], open: &mut Open<'_>, st: &mut State) -> Vec<Device> {
    let now = Instant::now();
    let mut out = Vec::new();
    for (vid, pid) in products(colls, VID) {
        if DECT_BASES.binary_search(&pid).is_ok() {
            continue;
        }
        match get(&st.skip, pid) {
            Some(None) => continue,
            Some(Some(until)) if now < until => continue,
            _ => {}
        }
        let mut found = find(colls, vid, pid, |c| {
            c.page == 0xFF00
                && (c.usage == 0x05 || c.usage == 0x01)
                && c.out_len >= 64
                && c.in_len >= 64
        });
        // Products with both collections (Evolve3 75 on USB) answer on 05.
        found.sort_by_key(|c| c.usage != 0x05);
        // Decided over all collections: whether a full probe ran and found nothing, and whether
        // every reply was a NAK. Dongles and known addresses are never skipped.
        let (mut reported, mut probed, mut skippable, mut all_nak) = (false, false, true, true);
        for c in found {
            let Some(mut io) = open(c) else { continue };
            let io = io.as_mut();
            let known = get(&st.addresses, pid);
            let dongle = known.map_or_else(|| c.product.contains("Link"), |a| a == DONGLE_HEADSET);
            let order: Vec<u8> = match known {
                Some(a) => vec![a],
                None if dongle => vec![DONGLE_HEADSET, USB_HEADSET],
                None => vec![USB_HEADSET, DONGLE_HEADSET],
            };
            if dongle || order.len() == 1 {
                skippable = false;
            }
            let mut answer = None; // (address, battery payload; None = headset offline)
            for a in order {
                match query(io, st, a, 0x12, 0x02, hid_io::MAX_REPLY_MS) {
                    Reply::Ok(p) if p.len() >= 2 => {
                        answer = Some((a, Some(p)));
                        break;
                    }
                    // A dongle reports its headset offline rather than absent (F3); on other
                    // products any other NAK means no headset with a battery at that address.
                    Reply::Nak(reason)
                        if a == DONGLE_HEADSET && (dongle || reason == NAK_PEER_OFFLINE) =>
                    {
                        answer = Some((a, None));
                        break;
                    }
                    Reply::Nak(_) => {}
                    _ => all_nak = false,
                }
            }
            let Some((address, battery)) = answer else {
                st.addresses.retain(|(p, _)| *p != pid);
                probed = true;
                continue;
            };
            set(&mut st.addresses, pid, address);
            reported = true;
            let Some(p) = battery else {
                // Dongle present, headset off or out of range: not listed; the name is asked again
                // when it is back, so a newly paired headset is picked up.
                st.names.retain(|(q, _)| *q != pid);
                break;
            };
            if get(&st.names, pid).is_none() {
                let name = headset_name(io, st, address).unwrap_or_else(|| "Jabra headset".into());
                set(&mut st.names, pid, name);
            }
            let via = if address == DONGLE_HEADSET {
                "Jabra Link dongle"
            } else {
                "USB cable"
            };
            let name = get(&st.names, pid).unwrap_or_default();
            let mut d = Device::new(format!("jabra:{pid:04X}"), name, Kind::Headset, via);
            d.percent = (p[1] <= 100).then_some(p[1]);
            d.charging = p[0] & 0x01 != 0;
            out.push(d);
            break;
        }
        if !reported && probed && skippable {
            set(&mut st.skip, pid, (!all_nak).then(|| now + SILENT_RETRY));
        }
    }
    out
}

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    let mut guard = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let st = guard.get_or_insert_with(State::default);
    hid_io::run(api, infos, |colls, open| poll_colls(colls, open, st))
}

#[cfg(test)]
mod tests {
    // tests/jabra_driver_test.cpp
    use super::*;
    use crate::drivers::hid_io::fake::Fake;
    use std::cell::RefCell;
    use std::ffi::CString;
    use std::rc::Rc;

    #[derive(Clone)]
    struct Peer {
        battery: Vec<u8>, // [flags][level]; empty = NAK with `nak`
        nak: u8,          // 0 = no reply at all
        name: String,
    }

    fn peer(battery: &[u8], nak: u8, name: &str) -> Peer {
        Peer {
            battery: battery.to_vec(),
            nak,
            name: name.into(),
        }
    }

    fn frame(src: u8, seq: u8, cls: u8, op: u8, payload: &[u8]) -> Vec<u8> {
        let mut f = vec![0u8; 64];
        f[0] = 0x05;
        f[2] = src;
        f[3] = seq;
        f[4] = 0xC0 | (6 + payload.len() as u8);
        f[5] = cls;
        f[6] = op;
        f[7..7 + payload.len()].copy_from_slice(payload);
        f
    }

    fn coll(pid: u16, usage: u16, path: &str, product: &str) -> Coll {
        let mut c = Coll::new(VID, pid, 0xFF00, usage, 64, 64);
        c.iface = 3;
        c.path = CString::new(path).unwrap();
        c.product = product.into();
        c
    }

    /// Runs one poll: each collection answers per peer address; `nak_all_on` NAKs everything.
    struct Rig {
        writes: Rc<RefCell<Vec<Vec<u8>>>>,
        opened: RefCell<Vec<String>>,
    }

    fn poll_with(
        colls: Vec<Coll>,
        peers: &[(u8, Peer)],
        nak_all_on: &str,
        st: &mut State,
    ) -> (Vec<Device>, Rig) {
        let rig = Rig {
            writes: Rc::new(RefCell::new(Vec::new())),
            opened: RefCell::new(Vec::new()),
        };
        // The headset's audio collection is left alone.
        let mut audio = colls[0].clone();
        audio.page = 0x000C;
        audio.usage = 1;
        let mut all = vec![audio];
        all.extend(colls);
        let peers = peers.to_vec();
        let nak_all_on = nak_all_on.to_string();
        let mut open: Box<Open<'_>> = Box::new(|c: &Coll| {
            assert_eq!(c.page, 0xFF00);
            let path = c.path.to_string_lossy().into_owned();
            rig.opened.borrow_mut().push(path.clone());
            let f = Fake::new();
            let (peers, writes, nak_all) = (peers.clone(), rig.writes.clone(), path == nak_all_on);
            f.replies(move |w| {
                writes.borrow_mut().push(w.to_vec());
                let (dst, seq, cls, op) = (w[1], w[3], w[5], w[6]);
                if nak_all {
                    return vec![frame(dst, seq, 0xFE, 0xFF, &[])];
                }
                let Some((_, p)) = peers.iter().find(|(a, _)| *a == dst) else {
                    return vec![];
                };
                if cls == 0x02 && op == 0x00 && !p.name.is_empty() {
                    let mut payload = vec![p.name.len() as u8];
                    payload.extend_from_slice(p.name.as_bytes());
                    vec![frame(dst, seq, cls, op, &payload)]
                } else if cls == 0x12 && op == 0x02 && !p.battery.is_empty() {
                    vec![frame(dst, seq, cls, op, &p.battery)]
                } else if p.nak != 0 {
                    vec![frame(dst, seq, 0xFE, p.nak, &[])]
                } else {
                    vec![]
                }
            });
            Some(Box::new(f) as Box<dyn Io>)
        });
        let out = poll_colls(&all, &mut *open, st);
        drop(open);
        (out, rig)
    }

    fn addresses_asked(rig: &Rig) -> u32 {
        rig.writes.borrow().iter().fold(0, |m, w| m | (1 << w[1]))
    }

    #[test]
    fn evolve3_on_its_cable() {
        let mut st = State::default();
        let (out, rig) = poll_with(
            vec![
                coll(0x2548, 0x01, "col01", "Jabra Evolve3 75"),
                coll(0x2548, 0x05, "col05", "Jabra Evolve3 75"),
            ],
            &[(0x08, peer(&[0x01, 95], 0xF5, "Jabra Evolve3 75"))],
            "",
            &mut st,
        );
        assert_eq!(rig.opened.borrow()[0], "col05");
        assert_eq!(addresses_asked(&rig), 1 << 8);
        let d = &out[0];
        assert_eq!((d.percent, d.charging), (Some(95), true));
        assert_eq!(
            (d.name.as_str(), d.key.as_str()),
            ("Jabra Evolve3 75", "jabra:2548")
        );
        assert_eq!((d.via.as_str(), d.kind), ("USB cable", Kind::Headset));
    }

    #[test]
    fn link_dongle_then_headset_off() {
        let mut st = State::default();
        let link = || vec![coll(0x2E51, 0x05, "link", "Jabra Link 390")];
        let (out, rig) = poll_with(
            link(),
            &[
                (0x04, peer(&[0x00, 60], 0xF5, "Jabra Evolve2 65 Flex")),
                (0x08, peer(&[], 0, "")),
            ],
            "",
            &mut st,
        );
        assert_eq!(addresses_asked(&rig), 1 << 4);
        assert_eq!((out[0].percent, out[0].charging), (Some(60), false));
        assert_eq!(out[0].name, "Jabra Evolve2 65 Flex");
        assert_eq!(out[0].via, "Jabra Link dongle");
        // Switched off: the dongle NAKs F3, nothing is listed.
        let (out, _) = poll_with(link(), &[(0x04, peer(&[], 0xF3, ""))], "", &mut st);
        assert!(out.is_empty());
    }

    #[test]
    fn unlisted_dongle_falls_back_to_04_and_keeps_it() {
        let mut st = State::default();
        let peers = [
            (0x04, peer(&[0x00, 40], 0xF5, "Jabra Evolve2 85")),
            (0x08, peer(&[], 0, "")),
        ];
        let dongle = || vec![coll(0x24C9, 0x01, "dongle", "Jabra USB")];
        let (out, rig) = poll_with(dongle(), &peers, "", &mut st);
        assert_eq!(addresses_asked(&rig), (1 << 8) | (1 << 4));
        assert_eq!(
            (out[0].percent, out[0].via.as_str()),
            (Some(40), "Jabra Link dongle")
        );
        let (out, rig) = poll_with(dongle(), &peers, "", &mut st);
        assert_eq!(out.len(), 1);
        assert_eq!(addresses_asked(&rig), 1 << 4);
    }

    #[test]
    fn wired_and_silent_products_are_skipped_later() {
        let mut st = State::default();
        let wired = || vec![coll(0x2510, 0x01, "wired", "Jabra Evolve2 40")];
        let peers = [(0x08, peer(&[], 0xFF, "")), (0x04, peer(&[], 0xF5, ""))];
        assert!(poll_with(wired(), &peers, "", &mut st).0.is_empty());
        let (out, rig) = poll_with(wired(), &peers, "", &mut st);
        assert!(out.is_empty() && rig.opened.borrow().is_empty());

        let silent = || vec![coll(0x2560, 0x01, "silent", "Jabra Speak")];
        let peers = [(0x08, peer(&[], 0, "")), (0x04, peer(&[], 0, ""))];
        let (out, rig) = poll_with(silent(), &peers, "", &mut st);
        assert!(out.is_empty() && rig.opened.borrow().len() == 1);
        assert_eq!(addresses_asked(&rig), (1 << 8) | (1 << 4));
        let (out, rig) = poll_with(silent(), &peers, "", &mut st);
        assert!(out.is_empty() && rig.opened.borrow().is_empty());
    }

    #[test]
    fn one_collection_naks_everything_the_other_answers() {
        let mut st = State::default();
        let colls = || {
            vec![
                coll(0x2550, 0x05, "e85-col05", "Jabra Evolve3 85"),
                coll(0x2550, 0x01, "e85-col01", "Jabra Evolve3 85"),
            ]
        };
        let peers = [(0x08, peer(&[0x00, 70], 0xF5, "Jabra Evolve3 85"))];
        let (out, rig) = poll_with(colls(), &peers, "e85-col05", &mut st);
        assert_eq!((out.len(), rig.opened.borrow().len()), (1, 2));
        assert_eq!(poll_with(colls(), &peers, "e85-col05", &mut st).0.len(), 1);
    }

    #[test]
    fn dect_bases_other_collections_and_bad_levels() {
        let mut st = State::default();
        let peers = [(0x08, peer(&[0x00, 50], 0xF5, "Jabra Engage 75"))];
        let (out, rig) = poll_with(
            vec![coll(0x1112, 0x01, "engage", "Jabra Engage 75")],
            &peers,
            "",
            &mut st,
        );
        assert!(out.is_empty() && rig.opened.borrow().is_empty());
        let (out, rig) = poll_with(
            vec![coll(0x2548, 0x02, "other-usage", "Jabra Evolve3 75")],
            &peers,
            "",
            &mut st,
        );
        assert!(out.is_empty() && rig.opened.borrow().is_empty());

        let peers = [(0x08, peer(&[0x00, 200], 0xF5, "Jabra Evolve2 55"))];
        let (out, _) = poll_with(
            vec![coll(0x2530, 0x01, "e55", "Jabra Evolve2 55")],
            &peers,
            "",
            &mut st,
        );
        assert_eq!((out.len(), out[0].percent), (1, None));
    }
}

//! ASUS ROG wireless keyboards, mice and headsets (VID 0B05), including the ROG Omni Receiver
//! (0B05:1ACE):
//!  * Query [report id][12][sub]: sub 01 = keyboard battery, 07 = mouse battery. Reply
//!    [id][12][sub] ..: keyboard [6] % (Falchion [11]), [9] = 1 charging; mouse [5] level (x25 on
//!    older 0-4 gauges, coarse), [10] charging. [1..2] = FF AA: the device behind the receiver did
//!    not answer (asleep). Offsets count the report id byte.
//!  * Omni Receiver: [01][A0][00] on col01 lists the paired product ids in 4-byte slots from [5];
//!    each is read on its own receiver channel.
//!  * ROG Pelta (1B84) / Delta II (1AFA) headsets: report CC on FF00:0001 of interface 3,
//!    CC 12 07 -> [6] battery % (FE unknown), CC 12 08 -> [5] 1 = charging.
//!  * ROG Cetra Open Wireless (1CAC) earbuds: CC 12 12 -> [5] left %, [6] right %, [7] case %.
//!
//! Only query commands are sent.
//!
//! Ported from BatteryHub (src/drivers/rog.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0, which cites
//! github.com/SteveWang92/peripheral-battery-tray (rog-battery, based on G-Helper) and
//! github.com/1gcat/BatteryTool. Headset replies are awaited 500 ms (BatteryHub: 1.2 s).

use std::collections::HashSet;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::hid::{self, BUDGET, Coll, Dev, Hid, find, hex4, left};
use crate::registry::Match;

const VID: u16 = 0x0B05;
const OMNI: u16 = 0x1ACE;
const CETRA_OPEN: u16 = 0x1CAC;

struct Model {
    pid: u16,
    name: &'static str,
    keyboard: bool,
    /// 0 = unnumbered (receiver / cable); otherwise the Omni channel's report id.
    report_id: u8,
    /// USB interface, -1 = any vendor-page collection.
    iface: i32,
    /// Required collection tag in the path ("col02").
    col: Option<&'static str>,
    offset: usize,
    scale: u8,
}

const fn kb(
    pid: u16,
    name: &'static str,
    rid: u8,
    iface: i32,
    col: Option<&'static str>,
    offset: usize,
) -> Model {
    Model {
        pid,
        name,
        keyboard: true,
        report_id: rid,
        iface,
        col,
        offset,
        scale: 1,
    }
}

const fn ms(
    pid: u16,
    name: &'static str,
    rid: u8,
    iface: i32,
    col: Option<&'static str>,
    offset: usize,
    scale: u8,
) -> Model {
    Model {
        pid,
        name,
        keyboard: false,
        report_id: rid,
        iface,
        col,
        offset,
        scale,
    }
}

const C2: Option<&str> = Some("col02");
const C3: Option<&str> = Some("col03");

#[rustfmt::skip]
const MODELS: &[Model] = &[
    // Keyboards (Omni = col02 report 02, receiver / wired = interface 1 unnumbered)
    kb(0x1A85, "ROG Azoth", 2, 2, C2, 6),
    kb(0x1A85, "ROG Azoth", 0, 1, None, 6),
    kb(0x1A83, "ROG Azoth", 0, 1, None, 6),
    kb(0x1B42, "ROG Azoth Extreme", 2, 2, C2, 6),
    kb(0x1B3F, "ROG Azoth Extreme", 0, 1, None, 6),
    kb(0x1CF1, "ROG Azoth Extreme SE", 2, 2, C2, 6),
    kb(0x1CEF, "ROG Azoth Extreme SE", 0, 1, None, 6),
    kb(0x1C25, "ROG Azoth X", 0, 1, None, 6),
    kb(0x1C24, "ROG Azoth X", 0, 1, None, 6),
    kb(0x193E, "ROG Falchion", 0, 1, None, 11),
    kb(0x193C, "ROG Falchion", 0, 1, None, 11),
    kb(0x1B04, "ROG Falchion RX", 0, -1, None, 11),
    kb(0x1B06, "ROG Falchion RX Low Profile", 2, 2, C2, 11),
    kb(0x1AB0, "ROG Strix Scope II 96", 2, 2, C2, 6),
    kb(0x1AAE, "ROG Strix Scope II 96", 0, 1, None, 6),
    kb(0x1B7A, "ROG Strix Scope II 96 RX", 2, 2, C2, 6),
    kb(0x1B78, "ROG Strix Scope II 96 RX", 0, 1, None, 6),
    kb(0x1A07, "ROG Strix Scope RX TKL", 0, -1, None, 6),
    // Mice (Omni = col03 report 03, receiver / wired = interface 0 unnumbered)
    ms(0x18E5, "ROG Chakram", 0, 0, None, 5, 25),
    ms(0x18E3, "ROG Chakram", 0, 0, None, 5, 25),
    ms(0x1A1A, "ROG Chakram X", 0, 0, None, 5, 1),
    ms(0x1A18, "ROG Chakram X", 0, 0, None, 5, 1),
    ms(0x18A0, "ROG Gladius II Wireless", 0, 2, None, 5, 25),
    ms(0x197F, "ROG Gladius III Wireless", 0, 0, None, 5, 1),
    ms(0x197D, "ROG Gladius III Wireless", 0, 0, None, 5, 1),
    ms(0x1A72, "ROG Gladius III Aimpoint", 3, 2, C3, 5, 1),
    ms(0x1A72, "ROG Gladius III Aimpoint", 0, 0, None, 5, 1),
    ms(0x1A70, "ROG Gladius III Aimpoint", 0, 0, None, 5, 1),
    ms(0x1B0C, "ROG Gladius III Aimpoint EVA-02", 0, 0, None, 5, 1),
    ms(0x1B0A, "ROG Gladius III Aimpoint EVA-02", 0, 0, None, 5, 1),
    ms(0x1A94, "ROG Harpe Ace Aim Lab Edition", 3, 2, C3, 5, 1),
    ms(0x1A94, "ROG Harpe Ace Aim Lab Edition", 0, 0, None, 5, 1),
    ms(0x1A92, "ROG Harpe Ace Aim Lab Edition", 0, 0, None, 5, 1),
    ms(0x1B68, "ROG Harpe Ace Extreme", 3, 2, C3, 5, 1),
    ms(0x1B69, "ROG Harpe Ace Extreme", 3, 2, C3, 5, 1),
    ms(0x1B67, "ROG Harpe Ace Extreme", 0, 0, None, 5, 1),
    ms(0x1B65, "ROG Harpe Ace Mini", 3, 2, C3, 5, 1),
    ms(0x1B63, "ROG Harpe Ace Mini", 0, 0, None, 5, 1),
    ms(0x1AD0, "ROG Harpe II Ace", 3, 2, C3, 5, 1),
    ms(0x1C69, "ROG Harpe II Ace", 0, 0, None, 5, 1),
    ms(0x1960, "ROG Keris Wireless", 0, 0, None, 5, 25),
    ms(0x195E, "ROG Keris Wireless", 0, 0, None, 5, 25),
    ms(0x1A59, "ROG Keris Wireless EVA Edition", 0, 0, None, 5, 25),
    ms(0x1A57, "ROG Keris Wireless EVA Edition", 0, 0, None, 5, 25),
    ms(0x1A68, "ROG Keris Wireless Aimpoint", 3, 2, C3, 5, 1),
    ms(0x1A6A, "ROG Keris Wireless Aimpoint", 3, 2, C3, 5, 1),
    ms(0x1A68, "ROG Keris Wireless Aimpoint", 0, 0, None, 5, 1),
    ms(0x1A66, "ROG Keris Wireless Aimpoint", 0, 0, None, 5, 1),
    ms(0x1B1A, "ROG Keris II Ace", 3, 2, C3, 5, 1),
    ms(0x1B18, "ROG Keris II Ace", 3, 2, C3, 5, 1),
    ms(0x1B16, "ROG Keris II Ace", 0, 0, None, 5, 1),
    ms(0x1C0E, "ROG Keris II Origin", 3, 2, C3, 5, 1),
    ms(0x1C0C, "ROG Keris II Origin", 0, 0, None, 5, 1),
    ms(0x1D4E, "ROG Keris II Origin KJP", 3, 2, C3, 5, 1),
    ms(0x1D4C, "ROG Keris II Origin KJP", 0, 0, None, 5, 1),
    ms(0x1908, "ROG Pugio II", 0, 0, None, 5, 25),
    ms(0x1906, "ROG Pugio II", 0, 0, None, 5, 25),
    ms(0x1979, "ROG Spatha X", 0, 0, None, 5, 1),
    ms(0x1977, "ROG Spatha X", 0, 0, None, 5, 1),
    ms(0x18B4, "ROG Strix Carry", 0, 1, None, 7, 25),
    ms(0x1949, "ROG Strix Impact II Wireless", 0, 0, None, 5, 25),
    ms(0x1947, "ROG Strix Impact II Wireless", 0, 0, None, 5, 25),
    ms(0x1AD7, "ROG Strix Impact III Wireless", 3, 2, C3, 5, 1),
];

/// (pid, name) of the headsets with the Pelta battery commands.
const HEADSETS: [(u16, &str); 2] = [(0x1B84, "ROG Pelta"), (0x1AFA, "ROG Delta II")];

#[rustfmt::skip]
pub static MATCHES: &[Match] = &[Match {
    vid: VID,
    pids: &[
        0x1A85, 0x1A83, 0x1B42, 0x1B3F, 0x1CF1, 0x1CEF, 0x1C25, 0x1C24, 0x193E, 0x193C, 0x1B04,
        0x1B06, 0x1AB0, 0x1AAE, 0x1B7A, 0x1B78, 0x1A07, 0x18E5, 0x18E3, 0x1A1A, 0x1A18, 0x18A0,
        0x197F, 0x197D, 0x1A72, 0x1A70, 0x1B0C, 0x1B0A, 0x1A94, 0x1A92, 0x1B68, 0x1B69, 0x1B67,
        0x1B65, 0x1B63, 0x1AD0, 0x1C69, 0x1960, 0x195E, 0x1A59, 0x1A57, 0x1A68, 0x1A6A, 0x1A66,
        0x1B1A, 0x1B18, 0x1B16, 0x1C0E, 0x1C0C, 0x1D4E, 0x1D4C, 0x1908, 0x1906, 0x1979, 0x1977,
        0x18B4, 0x1949, 0x1947, 0x1AD7, OMNI, 0x1B84, 0x1AFA, CETRA_OPEN,
    ],
}];

/// Omni-channel models seen answering at least once (an empty channel also answers "asleep").
static AWAKE: Mutex<Option<HashSet<u16>>> = Mutex::new(None);

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    let mut awake = AWAKE.lock().unwrap_or_else(|e| e.into_inner());
    hid::run(api, infos, |hid, colls| {
        poll_with(colls, hid, awake.get_or_insert_with(HashSet::new))
    })
}

#[derive(Debug, PartialEq)]
pub enum Reply {
    Ok(Vec<u8>),
    Asleep,
}

/// Classifies an input report (report id slot restored) as the answer to (cmd, sub).
pub fn classify(r: &[u8], cmd: u8, sub: u8) -> Option<Reply> {
    if r.len() < 12 {
        return None;
    }
    if r[1] == 0xFF && r[2] == 0xAA {
        return Some(Reply::Asleep);
    }
    (r[1] == cmd && r[2] == sub).then(|| Reply::Ok(r.to_vec()))
}

fn query(d: &mut dyn Dev, rid: u8, cmd: u8, sub: u8, end: Instant) -> Option<Reply> {
    d.flush();
    if !d.write(&[rid, cmd, sub]) {
        return None;
    }
    let end = end.min(Instant::now() + Duration::from_millis(500));
    loop {
        let left = left(end);
        if left.is_zero() {
            return None;
        }
        let mut r = d.read(left).filter(|r| r.len() >= 11)?;
        if rid == 0 {
            r.insert(0, 0); // restore the report id slot so offsets match
        }
        if let Some(rep) = classify(&r, cmd, sub) {
            return Some(rep);
        }
    }
}

/// Fills the device from a battery reply. Returns the device (asleep when it gave no level).
fn read_model(m: &Model, rep: Reply) -> Device {
    let mut s = Device::new(
        format!(
            "rog:{}:{}",
            hex4(m.pid),
            if m.keyboard { "kb" } else { "ms" }
        ),
        m.name,
        if m.keyboard {
            Kind::Keyboard
        } else {
            Kind::Mouse
        },
        if m.report_id != 0 {
            "ROG Omni Receiver"
        } else {
            "2.4 GHz receiver / USB"
        },
    );
    // BatteryHub reports a silent device as asleep (last value kept); 30 min like other receivers.
    s.grace = Duration::from_secs(1800);
    match rep {
        Reply::Asleep => s.asleep = true,
        Reply::Ok(r) if m.keyboard => {
            s.percent = Some(r[m.offset]).filter(|p| *p <= 100);
            s.charging = r[9] == 1;
        }
        Reply::Ok(r) => {
            let v = u16::from(r[m.offset]) * u16::from(m.scale);
            if (1..=100).contains(&v) {
                s.percent = Some(v as u8);
                s.coarse = m.scale > 1;
            } else {
                s.asleep = true; // 0 means unavailable, not empty
            }
            s.charging = r[10] != 0;
        }
    }
    s
}

fn add_once(out: &mut Vec<Device>, s: Device) {
    // One entry per name (wired + receiver are both present while charging).
    if !out.iter().any(|o| o.name == s.name) {
        out.push(s);
    }
}

fn on_omni(c: &Coll, col: &str) -> bool {
    c.iface == 2 && c.out_len >= 12 && c.path_str().contains(col)
}

/// Omni Receiver: asks which devices are paired, then reads each on its own channel.
fn poll_omni(colls: &[Coll], io: &mut dyn Hid, out: &mut Vec<Device>, end: Instant) {
    let Some(list) = find(colls, VID, OMNI, |c| on_omni(c, "col01"))
        .into_iter()
        .next()
    else {
        return;
    };
    let Some(Reply::Ok(r)) = io
        .open(list)
        .and_then(|mut d| query(d.as_mut(), 0x01, 0xA0, 0x00, end))
    else {
        return;
    };
    let mut at = 5;
    while at + 1 < r.len() {
        let pid = u16::from_le_bytes([r[at], r[at + 1]]);
        if pid == 0 {
            break;
        }
        // Only the Omni-channel entry of that model.
        if let Some(m) = MODELS.iter().find(|m| m.pid == pid && m.report_id != 0) {
            for c in find(colls, VID, OMNI, |c| on_omni(c, m.col.unwrap_or(""))) {
                if left(end).is_zero() {
                    return;
                }
                let sub = if m.keyboard { 0x01 } else { 0x07 };
                if let Some(rep) = io
                    .open(c)
                    .and_then(|mut d| query(d.as_mut(), m.report_id, 0x12, sub, end))
                {
                    add_once(out, read_model(m, rep));
                    break;
                }
            }
        }
        at += 4;
    }
}

/// Pelta-style headset exchange: [CC][12][cmd] through the control pipe (or the interrupt pipe).
fn pelta_query(d: &mut dyn Dev, cmd: u8, end: Instant) -> Option<Reply> {
    d.flush();
    let end = end.min(Instant::now() + Duration::from_millis(500));
    let req = [0xCC, 0x12, cmd];
    let mut next = if d.set_output(&req) {
        d.read(left(end).max(Duration::from_millis(1)))
    } else {
        None
    };
    if next.is_none() && !left(end).is_zero() && d.write(&req) {
        next = d.read(left(end).max(Duration::from_millis(1)));
    }
    while let Some(r) = next.take().filter(|r| r.len() >= 8) {
        if r[0] == 0xCC && r[1] == 0xFF && r[2] == 0xAA {
            return Some(Reply::Asleep);
        }
        if r[0] == 0xCC && r[1] == 0x12 {
            if r[5] == 0xFF && r[6] == 0xAA {
                return Some(Reply::Asleep);
            }
            if r[2] == cmd {
                return Some(Reply::Ok(r));
            }
        }
        let left = left(end);
        if left.is_zero() {
            break;
        }
        next = d.read(left);
    }
    None
}

fn headset_colls(colls: &[Coll], pid: u16) -> Vec<&Coll> {
    find(colls, VID, pid, |c| {
        c.iface == 3 && c.page == 0xFF00 && c.usage == 1 && c.in_len >= 8 && c.out_len >= 8
    })
}

fn headset(pid: u16, name: &str) -> Device {
    let mut s = Device::new(
        format!("rog:{}:hs", hex4(pid)),
        name,
        Kind::Headset,
        "2.4 GHz receiver",
    );
    s.grace = Duration::from_secs(1800);
    s
}

fn poll_pelta(colls: &[Coll], io: &mut dyn Hid, out: &mut Vec<Device>, end: Instant) {
    for (pid, name) in HEADSETS {
        for c in headset_colls(colls, pid) {
            if left(end).is_zero() {
                return;
            }
            let Some(mut d) = io.open(c) else { continue };
            let Some(rep) = pelta_query(d.as_mut(), 0x07, end) else {
                continue;
            };
            let mut s = headset(pid, name);
            match rep {
                Reply::Asleep => s.asleep = true,
                Reply::Ok(r) => {
                    if r[6] <= 100 {
                        s.percent = Some(r[6]);
                    } else {
                        s.asleep = true; // FE = level not known yet
                    }
                    if let Some(Reply::Ok(ch)) = pelta_query(d.as_mut(), 0x08, end) {
                        s.charging = ch[5] == 1;
                    }
                }
            }
            out.push(s);
            break;
        }
    }
}

/// Earbud levels (left, right) from a CC 12 12 reply; 0 counts as not reported.
pub fn cetra_levels(r: &[u8]) -> (Option<u8>, Option<u8>) {
    let level = |at: usize| r.get(at).copied().filter(|v| (1..=100).contains(v));
    (level(5), level(6))
}

fn poll_cetra(colls: &[Coll], io: &mut dyn Hid, out: &mut Vec<Device>, end: Instant) {
    for c in headset_colls(colls, CETRA_OPEN) {
        if left(end).is_zero() {
            return;
        }
        let Some(mut d) = io.open(c) else { continue };
        let Some(rep) = pelta_query(d.as_mut(), 0x12, end) else {
            continue;
        };
        let mut s = headset(CETRA_OPEN, "ROG Cetra Open Wireless");
        let (left, right) = match &rep {
            Reply::Ok(r) => cetra_levels(r),
            Reply::Asleep => (None, None),
        };
        match (left, right) {
            (None, None) => s.asleep = true,
            (l, r) => s.percent = [l, r].into_iter().flatten().min(),
        }
        out.push(s);
        break;
    }
}

fn poll_with(colls: &[Coll], io: &mut dyn Hid, awake: &mut HashSet<u16>) -> Vec<Device> {
    let end = Instant::now() + BUDGET;
    let mut out = Vec::new();
    poll_pelta(colls, io, &mut out, end);
    poll_cetra(colls, io, &mut out, end);
    poll_omni(colls, io, &mut out, end);
    let mut claimed: Vec<u16> = Vec::new();
    for m in MODELS {
        if claimed.contains(&m.pid) {
            continue;
        }
        for c in find(colls, VID, m.pid, |c| {
            c.out_len >= 12
                && c.page >= 0xFF00
                && (m.iface < 0 || c.iface == m.iface)
                && m.col.is_none_or(|t| c.path_str().contains(t))
        }) {
            if left(end).is_zero() {
                return out;
            }
            let sub = if m.keyboard { 0x01 } else { 0x07 };
            let Some(rep) = io
                .open(c)
                .and_then(|mut d| query(d.as_mut(), m.report_id, 0x12, sub, end))
            else {
                continue;
            };
            let s = read_model(m, rep);
            // A receiver channel (e.g. the SpeedNova 8K receiver 1AD0) also answers "asleep" when
            // nothing is paired, so it is only shown once it has really answered.
            if m.report_id != 0 && s.asleep && !awake.contains(&m.pid) {
                continue;
            }
            if m.report_id != 0 && !s.asleep {
                awake.insert(m.pid);
            }
            add_once(&mut out, s);
            claimed.push(m.pid);
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::hid::fake::{self, Fake, Op, iface_coll as coll};

    fn with_path(mut c: Coll, path: &str) -> Coll {
        c.path = path.into();
        c
    }

    fn mouse_reply(level: u8, charging: u8) -> Vec<u8> {
        // Unnumbered: the report as read, without the id slot.
        let mut r = vec![0u8; 63];
        r[0] = 0x12;
        r[1] = 0x07;
        r[4] = level; // [5] once the id slot is restored
        r[9] = charging; // [10]
        r
    }

    #[test]
    fn mouse_battery_and_old_gauges() {
        let chakram_x = MODELS.iter().find(|m| m.pid == 0x1A1A).unwrap();
        let mut r = vec![0u8; 64];
        r[1] = 0x12;
        r[2] = 0x07;
        r[5] = 64;
        r[10] = 1;
        let s = read_model(chakram_x, Reply::Ok(r.clone()));
        assert_eq!((s.percent, s.charging, s.coarse), (Some(64), true, false));
        let chakram = MODELS.iter().find(|m| m.pid == 0x18E5).unwrap();
        r[5] = 3;
        let s = read_model(chakram, Reply::Ok(r.clone()));
        assert_eq!((s.percent, s.coarse), (Some(75), true));
        r[5] = 0;
        assert!(
            read_model(chakram, Reply::Ok(r)).asleep,
            "0 is unavailable, not empty"
        );
    }

    #[test]
    fn falchion_reads_offset_11() {
        let falchion = MODELS.iter().find(|m| m.pid == 0x193E).unwrap();
        let mut r = vec![0u8; 64];
        r[11] = 81;
        r[9] = 1;
        let s = read_model(falchion, Reply::Ok(r));
        assert_eq!(
            (s.percent, s.charging, s.kind),
            (Some(81), true, Kind::Keyboard)
        );
        assert_eq!(s.key, "rog:193E:kb");
    }

    #[test]
    fn wired_mouse_query_restores_the_id_slot() {
        let mut f = Fake::with(|s, fc, op| {
            if let Some(q) = op.sent() {
                assert!(matches!(op, Op::Write(_)));
                assert_eq!(q, &[0u8, 0x12, 0x07][..]);
                let mut other = mouse_reply(10, 0);
                other[1] = 0x01; // a keyboard reply is not this answer
                s.push_input("", other);
                s.push_input("", mouse_reply(57, 0));
            }
            fake::queued(s, fc, op)
        });
        let out = poll_with(
            &[coll(VID, 0x1A18, 0, 0xFF01, 1, 64, 64, 0)],
            &mut f,
            &mut HashSet::new(),
        );
        assert_eq!(out.len(), 1);
        assert_eq!(
            (out[0].name.as_str(), out[0].percent),
            ("ROG Chakram X", Some(57))
        );
    }

    #[test]
    fn empty_omni_channel_is_hidden_until_it_answers() {
        let c = with_path(
            coll(VID, 0x1AD0, 2, 0xFF02, 1, 64, 64, 0),
            r"\\?\hid#vid_0b05&pid_1ad0&mi_02&col03#x",
        );
        let mut f = Fake::with(|s, fc, op| {
            if let Some(q) = op.sent() {
                assert_eq!(q, [3, 0x12, 0x07]);
                let mut r = vec![0u8; 64];
                r[0] = 3;
                r[1] = 0xFF;
                r[2] = 0xAA;
                s.push_input("", r);
            }
            fake::queued(s, fc, op)
        });
        let mut awake = HashSet::new();
        assert!(poll_with(std::slice::from_ref(&c), &mut f, &mut awake).is_empty());
        awake.insert(0x1AD0);
        let out = poll_with(&[c], &mut f, &mut awake);
        assert!(out[0].asleep && out[0].via == "ROG Omni Receiver");
    }

    #[test]
    fn pelta_reads_level_then_charging() {
        let mut f = Fake::with(|s, fc, op| {
            if let Some(q) = op.sent() {
                assert!(matches!(op, Op::SetOutput(_)));
                let mut r = vec![0u8; 64];
                r[..3].copy_from_slice(&[0xCC, 0x12, q[2]]);
                if q[2] == 0x07 {
                    r[6] = 73;
                } else {
                    r[5] = 1;
                }
                s.push_input("", r);
            }
            fake::queued(s, fc, op)
        });
        let out = poll_with(
            &[coll(VID, 0x1B84, 3, 0xFF00, 1, 64, 64, 0)],
            &mut f,
            &mut HashSet::new(),
        );
        assert_eq!(
            (out[0].name.as_str(), out[0].percent, out[0].charging),
            ("ROG Pelta", Some(73), true)
        );
        assert_eq!(
            (out[0].key.as_str(), out[0].kind),
            ("rog:1B84:hs", Kind::Headset)
        );
    }

    #[test]
    fn cetra_open_takes_the_lower_earbud() {
        let mut r = vec![0u8; 64];
        r[..3].copy_from_slice(&[0xCC, 0x12, 0x12]);
        r[5] = 99;
        r[6] = 97;
        r[7] = 86;
        assert_eq!(cetra_levels(&r), (Some(99), Some(97)));
        r[6] = 0;
        assert_eq!(cetra_levels(&r), (Some(99), None));
    }
}

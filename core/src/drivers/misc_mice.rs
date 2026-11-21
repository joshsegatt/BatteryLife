//! Smaller mouse brands, each with a community-documented battery protocol:
//!  * Motospeed X6 (Realtek 0BDA, receivers FFE0 / FFF2, interface 4): output report B5 [06], reply
//!    report B4, byte 20 = level (+128 while charging). The cable (FFF1) reports no battery.
//!  * Xenta receivers (Delux M900 Pro, Attack Shark X6 / R1 / X11 / X3 Max / V3 Pro / V6, Inphic,
//!    ARDOR GAMING Fury V2; 1D57): push-only input report 03 on usage page 000A every ~2 s:
//!    [03][model][40 / 41][state][level], state 2 = full, 3 = charging. Nothing is sent; handles
//!    stay open between polls, and a reading counts once two frames agree.
//!  * Ajazz AJ179 family (249A receiver, 248A cable, interface 2): 32-byte report, command 10:
//!    [10][00]..[4..7] model, [12] charging, [13] percent, [14] linked, [31] = sum([4..30]).
//!  * AJAZZ AJ-series on 3151 (AJ159 PRO / APEX ...): SET_FEATURE [00][F7] on FFFF:0002, then
//!    GET_FEATURE 05 -> [3] percent; a non-zero byte 1 or 2 is reconnect garbage.
//!  * AJAZZ AJ139 V2 MC and siblings (A8A5:2255 dongle, A8A4:2255 cable) on FF01:0010:
//!    [55][30][A5][chk][2E]... -> [AA][30].. [8] percent, [9] on USB power (both documented
//!    request variants are tried).
//!  * Incott G23 V2 / G24 (093A:522C dongle, 622C cable) on FF05: SET_FEATURE [09][89], GET_FEATURE 09
//!    -> [09][89][battery] (bit 7 charging).
//!  * Wraith W1 / W1 Freestyle on the same PixArt ids: [09][8F] device info (model 18 / 20), then the
//!    web driver's read-only 86 per-button reads while the status input report 09 is awaited.
//!  * Delux M800 Pro (248A: receiver 5B2F, cable 5B2E): 33-byte feature report 0C on FF00:0001, a
//!    status query with an incrementing sequence number -> [18] percent, [19] state.
//!
//! Whole vendors 248A, 249A and 3151 are scanned for the AJ179 / AJ-series collections, as
//! BatteryHub does (other drivers' exact product ids on those vendors win).
//!
//! Ported from BatteryHub (src/drivers/misc_mice.cpp, xenta_protocol.cpp / .h and
//! ardor_fury_v2_protocol.h, commit fd2267d), https://github.com/krisgfx9/Wireless-Device-Battery-Monitor,
//! GPL-3.0, which cites github.com/Jacobz888/LGSTrayBattery-MotospeedX6,
//! github.com/AeonArcHQ/omarchy-delux-m900, github.com/qu1ncky/BatteryTray,
//! github.com/Rockeyxx/aj179-linux, github.com/Aiacos/ajazz-control-center,
//! github.com/werquess/ajazz-aj139-macos, github.com/nico2525nn/AjazzAJ139V2,
//! github.com/OmarK6/Incott-Manager, github.com/HeHaoren/incott-mouse-app,
//! github.com/FelixNg21/MouseBattery and the Wraith web driver (w1.software). Query replies are
//! awaited 500 ms (BatteryHub: 0.7-1.5 s); push waits are capped by the driver's time budget.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::hid_dev::{
    BUDGET, Coll, Dev, HidIo, Io, colls, find, hex4, ms_left, products, read_until,
};
use crate::registry::Match;

const REPLY_MS: u64 = 500;

fn mouse(key: String, name: &str, via: &str) -> Device {
    Device::new(key, name, Kind::Mouse, via)
}

// ---- Motospeed X6 ------------------------------------------------------------------------------

const MOTOSPEED_VID: u16 = 0x0BDA;

pub static MOTOSPEED: &[Match] = &[Match {
    vid: MOTOSPEED_VID,
    pids: &[0xFFE0, 0xFFF2, 0xFFF1],
}];

/// (percent, charging) from report B4.
pub fn motospeed_parse(r: &[u8]) -> Option<(u8, bool)> {
    if r.len() <= 20 || r[0] != 0xB4 {
        return None;
    }
    let raw = r[20];
    let charging = raw >= 128;
    Some(((if charging { raw - 128 } else { raw }).min(100), charging))
}

pub fn poll_motospeed(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    motospeed_with(&colls(infos), &mut HidIo(api))
}

fn motospeed_with(colls: &[Coll], io: &mut dyn Io) -> Vec<Device> {
    let end = Instant::now() + BUDGET;
    let mut out = Vec::new();
    for pid in [0xFFE0, 0xFFF2, 0xFFF1] {
        for c in find(colls, MOTOSPEED_VID, pid, |c| {
            c.iface == 4 && c.out_len >= 21
        }) {
            let key = format!("motospeed:{}", hex4(pid));
            if pid == 0xFFF1 {
                // Plugged in: charging, no battery reading over the cable.
                let mut s = mouse(key, "Motospeed X6", "USB cable");
                s.charging = true;
                out.push(s);
                break;
            }
            if ms_left(end) == 0 {
                return out;
            }
            let Some(mut d) = io.open(c) else { continue };
            d.flush();
            if !d.write(&[0xB5, 0x06]) {
                continue;
            }
            let until = end.min(Instant::now() + Duration::from_millis(REPLY_MS));
            let Some((pct, charging)) =
                read_until(d.as_mut(), until, |r| r.len() > 20 && r[0] == 0xB4)
                    .and_then(|r| motospeed_parse(&r))
            else {
                continue;
            };
            let mut s = mouse(key, "Motospeed X6", "2.4 GHz receiver");
            s.percent = Some(pct);
            s.charging = charging;
            s.grace = Duration::from_secs(1800);
            out.push(s);
            break;
        }
    }
    out
}

// ---- Xenta receivers ---------------------------------------------------------------------------

const XENTA_VID: u16 = 0x1D57;
const XENTA_PIDS: [u16; 5] = [0xFA60, 0xFA61, 0xFA65, 0xFA55, 0x2155];

pub static XENTA: &[Match] = &[Match {
    vid: XENTA_VID,
    pids: &[0xFA60, 0xFA61, 0xFA65, 0xFA55, 0x2155],
}];

/// (code, name, level reported in tens). From the Inphic HUB model table and captures.
const XENTA_MODELS: [(u8, &str, bool); 10] = [
    (0x73, "Inphic IN10R MAX", false),
    (0x74, "Inphic IN10 MAX", false),
    (0x3D, "Delux M900 Pro", false),
    (0x30, "Delux M900 Pro", false),
    (0x50, "Delux M900 Pro", false),
    (0x55, "Attack Shark X11", false),
    (0x4E, "Attack Shark X3 Max", false),
    (0xE5, "Attack Shark V3 Pro", false),
    (0x24, "Attack Shark V6", false),
    (0x10, "Attack Shark X6 / R1", true),
];

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct XentaReport {
    pub model: u8,
    pub event: u8,
    pub state: u8,
    pub level: u8,
}

/// xenta_protocol::ParseBatteryReport.
pub fn xenta_parse(b: &[u8]) -> Option<XentaReport> {
    (b.len() >= 5 && b[0] == 0x03 && (b[2] == 0x40 || b[2] == 0x41)).then(|| XentaReport {
        model: b[1],
        event: b[2],
        state: b[3],
        level: b[4],
    })
}

/// ARDOR GAMING Fury V2 on 1D57:FA60: the high nibble of byte 3 is a charge tier (25 / 50 / 75 /
/// 100; 25 and 100 inferred). The fifth byte changes between levels and is not a signature.
pub fn ardor_percent(r: &[u8]) -> Option<u8> {
    if r.len() < 5 || r[0] != 0x03 || r[1] != 0x00 || r[2] != 0x40 || r[3] & 0x0F != 0x03 {
        return None;
    }
    match r[3] >> 4 {
        0x1 => Some(25),
        0x3 => Some(50),
        0x7 => Some(75),
        0xF => Some(100),
        _ => None,
    }
}

/// One open status collection and the frames it sent.
pub struct Listener {
    dev: Box<dyn Dev>,
    last: Vec<u8>,
    prev: Vec<u8>,
    last_frame_at: Option<Instant>,
    /// ARDOR: last tier confirmed by two frames, and the candidate being confirmed.
    ardor_confirmed: Option<(u8, Instant)>,
    ardor_candidate: Option<(u8, u8)>,
}

pub type Listeners = HashMap<String, Listener>;
static LISTENERS: Mutex<Option<Listeners>> = Mutex::new(None);

pub fn poll_xenta(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    let mut l = LISTENERS.lock().unwrap_or_else(|e| e.into_inner());
    xenta_with(
        &colls(infos),
        &mut HidIo(api),
        l.get_or_insert_with(HashMap::new),
    )
}

/// Takes the frames queued since the last poll; a new listener waits for two.
fn xenta_listen(l: &mut Listener, pid: u16, fresh: bool, end: Instant) {
    let until = end.min(Instant::now() + Duration::from_millis(if fresh { 5000 } else { 0 }));
    let mut got = 0;
    loop {
        let waiting = fresh && got < 2;
        if waiting && ms_left(until) == 0 {
            break;
        }
        let wait = if waiting { ms_left(until).max(1) } else { 30 };
        let Some(r) = l.dev.read(wait).filter(|r| r.len() >= 5) else {
            break;
        };
        if xenta_parse(&r).is_none() {
            continue;
        }
        l.prev = std::mem::take(&mut l.last);
        l.last = r[..5].to_vec();
        let now = Instant::now();
        l.last_frame_at = Some(now);
        // Only ARDOR's frame shape on FA60 uses the tier decoder; compare the decoded tier, not the
        // five bytes (the fifth changes with the tier).
        if pid == 0xFA60
            && r[1] == 0x00
            && let Some(pct) = ardor_percent(&r)
        {
            let count = match l.ardor_candidate {
                Some((p, n)) if p == pct => (n + 1).min(2),
                _ => 1,
            };
            l.ardor_candidate = Some((pct, count));
            if count >= 2 {
                l.ardor_confirmed = Some((pct, now));
            }
        }
        got += 1;
    }
}

/// The device from a listener's frames, or None when they are stale or unknown.
fn xenta_device(l: &Listener, pid: u16) -> Option<Device> {
    let now = Instant::now();
    // An established ARDOR reading stays usable for 45 s without new frames.
    let ardor_active = pid == 0xFA60
        && l.ardor_confirmed
            .is_some_and(|(_, at)| now.duration_since(at) <= Duration::from_secs(45));
    let at = l.last_frame_at?;
    if !ardor_active && now.duration_since(at) > Duration::from_secs(15) {
        return None; // other Xenta device, or stale
    }
    let cable = pid == 0xFA55 || pid == 0x2155;
    let f = xenta_parse(&l.last)?;
    let model = XENTA_MODELS.iter().find(|m| m.0 == f.model);
    let mut s = mouse(
        format!("xenta:{}", hex4(pid)),
        model.map_or("Wireless mouse", |m| m.1),
        if cable {
            "USB cable"
        } else {
            "2.4 GHz receiver"
        },
    );
    let agreed = l.prev == l.last;
    let ardor = if pid == 0xFA60 && !cable {
        ardor_percent(&l.last)
    } else {
        None
    };
    let delux = pid == 0xFA65 && (f.model == 0x30 || f.model == 0x50);
    let (st, level) = (f.state, f.level);
    if delux {
        if st == 0x02 {
            s.percent = Some(100);
        } else if st == 0x03 {
            s.charging = true;
            s.percent = Some(level).filter(|l| *l <= 100);
        } else if agreed && st == 0x01 && (1..=100).contains(&level) {
            s.percent = Some(level);
        }
    } else if ardor.is_some() {
        s.name = "ARDOR GAMING Fury V2".into();
        if ardor_active {
            s.percent = l.ardor_confirmed.map(|(p, _)| p);
            s.coarse = true;
        }
    } else if cable || st == 0x02 || st == 0x03 {
        s.charging = true; // the level is frozen on external power
        if st == 0x02 && !cable {
            s.percent = Some(100);
        }
    } else if agreed {
        if model.is_some_and(|m| m.2) {
            if (1..=10).contains(&level) {
                s.percent = Some(level * 10);
                s.coarse = true;
            }
        } else if st == 0x01 && (1..=100).contains(&level) {
            s.percent = Some(level);
        }
    }
    if s.percent.is_none() && !s.charging {
        s.asleep = true; // shown with the last known value
    }
    if !cable {
        s.grace = Duration::from_secs(1800);
    }
    Some(s)
}

fn xenta_with(colls: &[Coll], io: &mut dyn Io, listeners: &mut Listeners) -> Vec<Device> {
    let end = Instant::now() + BUDGET;
    let mut present = Vec::new();
    let mut out = Vec::new();
    for pid in XENTA_PIDS {
        for c in find(colls, XENTA_VID, pid, |c| c.page == 0x000A && c.in_len >= 5) {
            let path = c.path_str();
            present.push(path.clone());
            let fresh = !listeners.contains_key(&path);
            if fresh {
                // Push-only: read access is all it needs.
                let Some(dev) = io.open(c) else { continue };
                listeners.insert(
                    path.clone(),
                    Listener {
                        dev,
                        last: Vec::new(),
                        prev: Vec::new(),
                        last_frame_at: None,
                        ardor_confirmed: None,
                        ardor_candidate: None,
                    },
                );
            }
            let Some(l) = listeners.get_mut(&path) else {
                continue;
            };
            xenta_listen(l, pid, fresh, end);
            if let Some(s) = xenta_device(l, pid) {
                out.push(s);
                break;
            }
        }
    }
    // Forget receivers that were unplugged.
    listeners.retain(|p, _| present.contains(p));
    out
}

// ---- Ajazz, AJAZZ AJ-series / AJ139 and Delux M800 ---------------------------------------------

const DELUX_VID: u16 = 0x248A;
const DELUX_CABLE: u16 = 0x5B2E;
const DELUX_RECEIVER: u16 = 0x5B2F;
const AJAZZ_RECEIVER_VID: u16 = 0x249A;
const AJ_SERIES_VID: u16 = 0x3151;
const AJ139_DONGLE_VID: u16 = 0xA8A5;
const AJ139_CABLE_VID: u16 = 0xA8A4;
const AJ139_PID: u16 = 0x2255;

pub static AJAZZ: &[Match] = &[
    Match {
        vid: DELUX_VID,
        pids: &[],
    },
    Match {
        vid: AJAZZ_RECEIVER_VID,
        pids: &[],
    },
    Match {
        vid: AJ_SERIES_VID,
        pids: &[],
    },
    Match {
        vid: AJ139_DONGLE_VID,
        pids: &[AJ139_PID],
    },
    Match {
        vid: AJ139_CABLE_VID,
        pids: &[AJ139_PID],
    },
];

/// The Delux dongle requires incrementing sequence numbers.
static DELUX_SEQ: Mutex<u8> = Mutex::new(0);

pub fn poll_ajazz(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    let mut seq = DELUX_SEQ.lock().unwrap_or_else(|e| e.into_inner());
    ajazz_with(&colls(infos), &mut HidIo(api), &mut seq)
}

#[derive(Debug, PartialEq)]
pub struct Reading {
    pub percent: Option<u8>,
    pub charging: bool,
    /// false: receiver present, mouse off (nothing shown).
    pub connected: bool,
}

/// Delux M800 status: [18] percent, [19] state (0 discharging, 1 charging, 2 full), [6] link.
pub fn delux_parse(r: &[u8]) -> Option<Reading> {
    let pct = r[18];
    if pct > 100 {
        return None;
    }
    let connected = r[6] != 0 || pct > 0;
    let mut out = Reading {
        percent: None,
        charging: false,
        connected,
    };
    match r[19] {
        2 => {
            out.percent = Some(100);
            out.charging = true;
        }
        1 => {
            out.charging = true;
            out.percent = (connected && pct > 0).then_some(pct);
        }
        _ => out.percent = (connected && pct > 0).then_some(pct),
    }
    Some(out)
}

fn delux_valid(r: &[u8]) -> bool {
    r.len() >= 20 && r[0] == 0x0C && r[2] == 0x20
}

/// Returns (device if connected, handled) for one Delux product.
fn poll_delux(
    colls: &[Coll],
    io: &mut dyn Io,
    seq: &mut u8,
    pid: u16,
    end: Instant,
) -> Option<Option<Device>> {
    for c in find(colls, DELUX_VID, pid, |c| {
        c.page == 0xFF00 && c.usage == 1 && c.feat_len >= 33
    }) {
        if ms_left(end) == 0 {
            return None;
        }
        let Some(mut d) = io.open(c) else { continue };
        let receiver = pid == DELUX_RECEIVER;
        let mut req = vec![0u8; c.feat_len as usize];
        req[..3].copy_from_slice(&[0x0C, 0x01, 0x20]);
        *seq = seq.wrapping_add(1);
        req[4] = *seq;
        if !d.set_feature(&req) {
            continue;
        }
        io.sleep(20);
        let Some(mut r) = d.get_feature(0x0C).filter(|r| delux_valid(r)) else {
            continue;
        };
        if receiver && r[6] == 0 {
            *seq = seq.wrapping_add(1);
            req[4] = *seq;
            if d.set_feature(&req) {
                io.sleep(20);
                if let Some(r2) = d.get_feature(0x0C).filter(|r| delux_valid(r)) {
                    r = r2;
                }
            }
        }
        let Some(reading) = delux_parse(&r) else {
            continue;
        };
        if !reading.connected {
            return Some(None);
        }
        let mut s = mouse(
            "delux:m800".into(),
            "Delux M800 Pro",
            if receiver {
                "2.4 GHz receiver"
            } else {
                "USB cable"
            },
        );
        s.percent = reading.percent;
        s.charging = reading.charging;
        s.full = r[19] == 2;
        if receiver {
            s.grace = Duration::from_secs(1800);
        }
        return Some(Some(s));
    }
    None
}

/// AJ179 device-info reply (command 10) with a valid checksum.
pub fn aj179_valid(r: &[u8]) -> bool {
    r.len() >= 32
        && r[0] == 0x10
        && r[1] == 0x00
        && r[4..=30].iter().fold(0u8, |a, b| a.wrapping_add(*b)) == r[31]
}

/// "Ajazz AJ179" from model "M179", else "Ajazz mouse".
pub fn aj179_name(r: &[u8]) -> String {
    let model: String = r[4..8]
        .iter()
        .take_while(|b| (0x20..0x7F).contains(*b))
        .map(|b| *b as char)
        .collect();
    if model.len() == 4 && model.starts_with('M') {
        format!("Ajazz AJ{}", &model[1..])
    } else {
        "Ajazz mouse".into()
    }
}

fn poll_aj179(
    colls: &[Coll],
    io: &mut dyn Io,
    vid: u16,
    pid: u16,
    end: Instant,
) -> Option<Option<Device>> {
    for c in find(colls, vid, pid, |c| {
        c.iface == 2 && c.out_len == 33 && c.in_len == 33
    }) {
        if ms_left(end) == 0 {
            return None;
        }
        let Some(mut d) = io.open(c) else { continue };
        d.flush();
        // Report id 0, command 10, zero payload (checksum 0).
        if !d.write(&[0x00, 0x10]) {
            continue;
        }
        let until = end.min(Instant::now() + Duration::from_millis(REPLY_MS));
        let Some(r) = read_until(d.as_mut(), until, aj179_valid).filter(|r| r[13] <= 100) else {
            continue;
        };
        let receiver = vid == AJAZZ_RECEIVER_VID;
        if receiver && r[14] == 0 {
            return Some(None); // the mouse behind the receiver is off
        }
        let mut s = mouse(
            format!("ajazz:{}:{}", hex4(vid), hex4(pid)),
            &aj179_name(&r),
            if receiver {
                "2.4 GHz receiver"
            } else {
                "USB cable"
            },
        );
        s.charging = r[12] != 0;
        s.percent = Some(r[13]);
        if receiver {
            s.grace = Duration::from_secs(1800);
        }
        return Some(Some(s));
    }
    None
}

/// AJ-series GET_FEATURE 05: percent (0 = link up, mouse off); None for a malformed frame.
pub fn aj_series_parse(r: &[u8]) -> Option<u8> {
    (r.len() >= 4 && r[0] == 0x05 && r[1] == 0 && r[2] == 0 && r[3] <= 100).then(|| r[3])
}

fn poll_aj_series(
    colls: &[Coll],
    io: &mut dyn Io,
    pid: u16,
    end: Instant,
) -> Option<Option<Device>> {
    for c in find(colls, AJ_SERIES_VID, pid, |c| {
        c.page == 0xFFFF && c.usage == 0x02 && c.feat_len >= 65
    }) {
        if ms_left(end) == 0 {
            return None;
        }
        let Some(mut d) = io.open(c) else { continue };
        let mut req = vec![0u8; c.feat_len as usize];
        req[1] = 0xF7; // status poll: brings the 2.4 GHz telemetry link up
        if !d.set_feature(&req) {
            continue;
        }
        io.sleep(30);
        let Some(pct) = d.get_feature(0x05).and_then(|r| aj_series_parse(&r)) else {
            continue;
        };
        if pct == 0 {
            return Some(None);
        }
        let mut s = mouse(
            format!("ajazzaj:{}:{}", hex4(AJ_SERIES_VID), hex4(pid)),
            &c.product_or("AJAZZ mouse"),
            "2.4 GHz receiver",
        );
        s.percent = Some(pct);
        s.grace = Duration::from_secs(1800);
        return Some(Some(s));
    }
    None
}

fn aj139_reply(r: &[u8]) -> bool {
    r.len() > 9 && r[0] == 0xAA && r[1] == 0x30
}

fn poll_aj139(colls: &[Coll], io: &mut dyn Io, vid: u16, end: Instant) -> Option<Option<Device>> {
    for c in find(colls, vid, AJ139_PID, |c| {
        c.page == 0xFF01 && c.usage == 0x0010 && c.out_len >= 65 && c.in_len >= 65
    }) {
        if ms_left(end) == 0 {
            return None;
        }
        let Some(mut d) = io.open(c) else { continue };
        d.flush();
        // werquess' request first, then nico2525nn's variant.
        let mut r = None;
        for req in [
            &[0x00, 0x55, 0x30, 0xA5, 0x2E, 0x2E][..],
            &[0x00, 0x55, 0x30, 0xA5, 0x0B, 0x2E, 0x01, 0x01][..],
        ] {
            if r.is_none() && d.write(req) {
                let until = end.min(Instant::now() + Duration::from_millis(REPLY_MS));
                r = read_until(d.as_mut(), until, aj139_reply);
            }
        }
        let Some(r) = r.filter(|r| r[8] <= 100) else {
            continue;
        };
        let cable = vid == AJ139_CABLE_VID;
        if !cable && r[8] == 0 {
            return Some(None); // the mouse is off or out of range
        }
        let mut s = mouse(
            format!("ajazz139:{}:{}", hex4(vid), hex4(AJ139_PID)),
            "AJAZZ wireless mouse",
            if cable {
                "USB cable"
            } else {
                "2.4 GHz receiver"
            },
        );
        s.charging = r[9] != 0;
        s.percent = Some(r[8]);
        if !cable {
            s.grace = Duration::from_secs(1800);
        }
        return Some(Some(s));
    }
    None
}

fn ajazz_with(colls: &[Coll], io: &mut dyn Io, seq: &mut u8) -> Vec<Device> {
    let end = Instant::now() + BUDGET;
    let mut out = Vec::new();
    let mut handled: Vec<(u16, u16)> = Vec::new();
    // Delux M800 first; an answering cable also stands for its receiver.
    for pid in [DELUX_CABLE, DELUX_RECEIVER] {
        if handled.contains(&(DELUX_VID, pid)) {
            continue;
        }
        if let Some(found) = poll_delux(colls, io, seq, pid, end) {
            handled.push((DELUX_VID, pid));
            if pid == DELUX_CABLE && found.is_some() {
                handled.push((DELUX_VID, DELUX_RECEIVER));
            }
            out.extend(found);
        }
    }
    for vid in [AJAZZ_RECEIVER_VID, DELUX_VID] {
        for pid in products(colls, vid) {
            if !handled.contains(&(vid, pid))
                && let Some(found) = poll_aj179(colls, io, vid, pid, end)
            {
                handled.push((vid, pid));
                out.extend(found);
            }
        }
    }
    for pid in products(colls, AJ_SERIES_VID) {
        out.extend(poll_aj_series(colls, io, pid, end).flatten());
    }
    for vid in [AJ139_DONGLE_VID, AJ139_CABLE_VID] {
        out.extend(poll_aj139(colls, io, vid, end).flatten());
    }
    out
}

// ---- PixArt 093A: Wraith W1 and Incott ---------------------------------------------------------

const PIXART_VID: u16 = 0x093A;
const PIXART_PIDS: [u16; 2] = [0x522C, 0x622C];

pub static PIXART: &[Match] = &[Match {
    vid: PIXART_VID,
    pids: &[0x522C, 0x622C],
}];

/// Last W1 model code seen per product (0 = not a W1 yet): a W1 that is asleep is never handed to
/// the Incott query.
static W1_MODELS: Mutex<[u8; 2]> = Mutex::new([0, 0]);

pub fn poll_pixart(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    let mut known = W1_MODELS.lock().unwrap_or_else(|e| e.into_inner());
    pixart_with(&colls(infos), &mut HidIo(api), &mut known)
}

/// Incott status reply: (percent, charging); stale replies (bytes 2 and 3 both 0) are skipped.
pub fn incott_parse(r: &[u8]) -> Option<(u8, bool)> {
    if r.len() < 4 || r[0] != 0x09 || r[1] != 0x89 || (r[2] | r[3]) == 0 {
        return None;
    }
    let pct = r[2] & 0x7F;
    (pct <= 100).then_some((pct, r[2] & 0x80 != 0))
}

/// W1 device-info reply: Some(model) for a W1 (18 / 20), Some(0) for another PixArt mouse.
pub fn w1_model(r: &[u8]) -> Option<u8> {
    (r.len() >= 4 && r[0] == 0x09 && r[1] == 0x8F)
        .then(|| if r[3] == 18 || r[3] == 20 { r[3] } else { 0 })
}

fn pixart_device(pid: u16, key: &str, name: &str, pct: u8, charging: bool) -> Option<Device> {
    let cable = pid == 0x622C;
    // 0 from the dongle: the mouse is off.
    if !(cable || pct > 0 || charging) {
        return None;
    }
    let mut s = mouse(
        format!("{key}:{}", hex4(pid)),
        name,
        if cable {
            "USB cable"
        } else {
            "2.4 GHz receiver"
        },
    );
    s.percent = Some(pct);
    s.charging = charging;
    if !cable {
        s.grace = Duration::from_secs(1800);
    }
    Some(s)
}

/// The W1's battery: per-button reads (86) as the web driver does them, while listening for the
/// status input report 09 on every FF05 collection.
fn w1_read(
    colls: &[Coll],
    io: &mut dyn Io,
    d: &mut dyn Dev,
    pid: u16,
    end: Instant,
) -> Option<(u8, bool)> {
    let mut ins: Vec<Box<dyn Dev>> = find(colls, PIXART_VID, pid, |c| {
        c.page == 0xFF05 && c.in_len >= 2
    })
    .into_iter()
    .filter_map(|c| io.open(c))
    .collect();
    if ins.is_empty() {
        return None;
    }
    for i in ins.iter_mut() {
        i.flush();
    }
    let read_button = |d: &mut dyn Dev, io: &mut dyn Io, button: u8| {
        if !d.set_feature(&[0x09, 0x86, button]) {
            return false;
        }
        for t in 0..2 {
            io.sleep(if t == 0 { 150 } else { 20 });
            match d.get_feature(0x09) {
                None => return true,
                Some(f) if f.len() >= 4 && f[1] == 0x86 => break,
                _ => {}
            }
        }
        true
    };
    let listen = |ins: &mut [Box<dyn Dev>], ms: u64| -> Option<Vec<u8>> {
        let until = end.min(Instant::now() + Duration::from_millis(ms));
        while ms_left(until) > 0 {
            for i in ins.iter_mut() {
                if let Some(r) = i.read(50)
                    && r.len() >= 2
                    && r[0] == 0x09
                    && (r[1] & 0x7F) <= 100
                {
                    return Some(r);
                }
            }
        }
        None
    };
    io.sleep(200); // the W1 Service waits 200 ms after 8F before its next request
    let mut sent = false;
    for b in [0, 1, 2, 4, 3] {
        sent |= read_button(d, io, b);
    }
    if !sent {
        return None;
    }
    let mut got = listen(&mut ins, 400);
    for _ in 0..4 {
        if got.is_some() || ms_left(end) == 0 || !read_button(d, io, 0) {
            break;
        }
        got = listen(&mut ins, 550);
    }
    got.map(|r| (r[1] & 0x7F, r[1] & 0x80 != 0))
}

/// Some(device or hidden) when the product is a W1, None to leave it to the Incott query.
fn poll_w1(
    colls: &[Coll],
    io: &mut dyn Io,
    pid: u16,
    known: &mut u8,
    end: Instant,
) -> Option<Option<Device>> {
    let mut cs = find(colls, PIXART_VID, pid, |c| {
        c.page == 0xFF05 && c.feat_len >= 9
    });
    if cs.is_empty() {
        return None;
    }
    // On the cable the first FF05 collection rejects SET_FEATURE.
    cs.reverse();
    let mut open: Option<Box<dyn Dev>> = None;
    let mut replied: Option<u8> = None;
    for c in cs {
        if ms_left(end) == 0 {
            break;
        }
        let Some(mut d) = io.open(c) else { continue };
        if !d.set_feature(&[0x09, 0x8F]) {
            continue;
        }
        for _ in 0..10 {
            io.sleep(50);
            let Some(r) = d.get_feature(0x09) else { break };
            replied = w1_model(&r);
            if replied.is_some() {
                break;
            }
        }
        if replied.is_some() {
            open = Some(d);
            break;
        }
    }
    match replied {
        Some(0) => {
            *known = 0; // another PixArt mouse (Incott)
            return None;
        }
        Some(m) => *known = m,
        None => {}
    }
    if *known == 0 {
        return None; // no answer and never seen as a W1
    }
    let name = if *known == 20 {
        "Wraith W1 Freestyle"
    } else {
        "Wraith W1"
    };
    // Handled even when asleep, so the Incott query never goes to a W1.
    let reading = open.and_then(|mut d| w1_read(colls, io, d.as_mut(), pid, end));
    Some(reading.and_then(|(pct, charging)| pixart_device(pid, "wraithw1", name, pct, charging)))
}

fn poll_incott(colls: &[Coll], io: &mut dyn Io, pid: u16, end: Instant) -> Option<Device> {
    let mut cs = find(colls, PIXART_VID, pid, |c| {
        c.page == 0xFF05 && c.feat_len >= 9
    });
    cs.reverse();
    for c in cs {
        if ms_left(end) == 0 {
            return None;
        }
        let Some(mut d) = io.open(c) else { continue };
        if !d.set_feature(&[0x09, 0x89]) {
            continue;
        }
        let mut reading = None;
        for _ in 0..10 {
            io.sleep(50);
            let Some(r) = d.get_feature(0x09) else { break };
            if r.len() >= 4 && r[0] == 0x09 && r[1] == 0x89 && (r[2] | r[3]) != 0 {
                reading = incott_parse(&r);
                break;
            }
        }
        let Some((pct, charging)) = reading else {
            continue;
        };
        // The ids are shared by the G23 V2 and the G24.
        return pixart_device(pid, "incott", "Incott mouse", pct, charging);
    }
    None
}

fn pixart_with(colls: &[Coll], io: &mut dyn Io, known: &mut [u8; 2]) -> Vec<Device> {
    let end = Instant::now() + BUDGET;
    let mut out = Vec::new();
    for (i, pid) in PIXART_PIDS.into_iter().enumerate() {
        match poll_w1(colls, io, pid, &mut known[i], end) {
            Some(found) => out.extend(found),
            None => out.extend(poll_incott(colls, io, pid, end)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::hid_dev::fake::{Fake, Sent, coll};

    #[test]
    fn xenta_protocol() {
        // xenta_protocol_test.cpp
        let r = xenta_parse(&[0x03, 0x50, 0x41, 0x01, 0x45]).unwrap();
        assert_eq!((r.model, r.event, r.state, r.level), (0x50, 0x41, 0x01, 69));
        let r = xenta_parse(&[0x03, 0x30, 0x40, 0x01, 0x64]).unwrap();
        assert_eq!((r.model, r.event, r.level), (0x30, 0x40, 100));
        assert!(xenta_parse(&[0x03, 0x3D, 0x40, 0x01, 0x32]).is_some());
        assert!(xenta_parse(&[0x02, 0x50, 0x41, 0x01, 0x45]).is_none());
        assert!(xenta_parse(&[0x03, 0x50, 0x01, 0x01, 0x45]).is_none());
        assert!(xenta_parse(&[0x03, 0x50, 0x41, 0x01]).is_none());
    }

    #[test]
    fn ardor_tiers() {
        assert_eq!(ardor_percent(&[0x03, 0x00, 0x40, 0x33, 0x07]), Some(50));
        assert_eq!(ardor_percent(&[0x03, 0x00, 0x40, 0x73, 0x08]), Some(75));
        assert_eq!(ardor_percent(&[0x03, 0x00, 0x40, 0x32, 0x07]), None);
        assert_eq!(ardor_percent(&[0x03, 0x24, 0x41, 0x01, 0x64]), None);
    }

    #[test]
    fn xenta_counts_a_level_once_two_frames_agree() {
        let rx = coll(XENTA_VID, 0xFA60, 0, 0x000A, 0, 5, 0, 0);
        let mut listeners = HashMap::new();
        let mut f = Fake::new();
        // Every link-up starts with a false 100%; then two agreeing frames.
        for r in [
            [3, 0x24, 0x41, 1, 100],
            [3, 0x24, 0x41, 1, 64],
            [3, 0x24, 0x41, 1, 64],
        ] {
            f.s().push_input("", r.to_vec());
        }
        let out = xenta_with(std::slice::from_ref(&rx), &mut f, &mut listeners);
        assert_eq!(
            (out[0].name.as_str(), out[0].percent),
            ("Attack Shark V6", Some(64))
        );
        f.s().push_input("", vec![3, 0x24, 0x41, 1, 63]);
        let out = xenta_with(std::slice::from_ref(&rx), &mut f, &mut listeners);
        assert!(
            out[0].asleep && out[0].percent.is_none(),
            "one frame is not enough"
        );
        assert_eq!(f.s().opens, 1, "the handle stays open");
        assert!(f.s().sent.is_empty(), "nothing is ever sent");
    }

    #[test]
    fn xenta_charging_and_tens() {
        let charging = Listener {
            dev: Box::new(NoDev),
            last: vec![3, 0x10, 0x40, 3, 7],
            prev: vec![],
            last_frame_at: Some(Instant::now()),
            ardor_confirmed: None,
            ardor_candidate: None,
        };
        let s = xenta_device(&charging, 0xFA60).unwrap();
        assert!(s.charging && s.percent.is_none());
        let tens = Listener {
            last: vec![3, 0x10, 0x40, 1, 7],
            prev: vec![3, 0x10, 0x40, 1, 7],
            ..charging
        };
        let s = xenta_device(&tens, 0xFA61).unwrap();
        assert_eq!(
            (s.name.as_str(), s.percent, s.coarse),
            ("Attack Shark X6 / R1", Some(70), true)
        );
    }

    struct NoDev;
    impl Dev for NoDev {
        fn write(&mut self, _: &[u8]) -> bool {
            false
        }
        fn set_output(&mut self, _: &[u8]) -> bool {
            false
        }
        fn read(&mut self, _: u64) -> Option<Vec<u8>> {
            None
        }
        fn set_feature(&mut self, _: &[u8]) -> bool {
            false
        }
        fn get_feature(&mut self, _: u8) -> Option<Vec<u8>> {
            None
        }
        fn flush(&mut self) {}
    }

    #[test]
    fn motospeed_level_and_charging() {
        let mut r = vec![0u8; 33];
        r[0] = 0xB4;
        r[20] = 128 + 45;
        assert_eq!(motospeed_parse(&r), Some((45, true)));
        let mut f = Fake::with(|s, _, kind, q| {
            assert_eq!((kind, q), (Sent::Write, &[0xB5, 0x06][..]));
            let mut r = vec![0u8; 33];
            r[0] = 0xB4;
            r[20] = 77;
            s.push_input("", r);
        });
        let out = motospeed_with(
            &[coll(MOTOSPEED_VID, 0xFFE0, 4, 0xFF00, 1, 33, 33, 0)],
            &mut f,
        );
        assert_eq!((out[0].percent, out[0].charging), (Some(77), false));
        let out = motospeed_with(
            &[coll(MOTOSPEED_VID, 0xFFF1, 4, 0xFF00, 1, 33, 33, 0)],
            &mut Fake::new(),
        );
        assert!(out[0].charging && out[0].percent.is_none() && out[0].via == "USB cable");
    }

    fn aj179_reply(pct: u8, charging: u8, linked: u8) -> Vec<u8> {
        let mut r = vec![0u8; 32];
        r[0] = 0x10;
        r[4..8].copy_from_slice(b"M179");
        r[12] = charging;
        r[13] = pct;
        r[14] = linked;
        r[31] = r[4..=30].iter().fold(0u8, |a, b| a.wrapping_add(*b));
        r
    }

    #[test]
    fn aj179_receiver_and_checksum() {
        assert!(aj179_valid(&aj179_reply(80, 0, 1)));
        assert_eq!(aj179_name(&aj179_reply(80, 0, 1)), "Ajazz AJ179");
        let mut bad = aj179_reply(80, 0, 1);
        bad[31] ^= 1;
        assert!(!aj179_valid(&bad));
        let mut f = Fake::with(|s, _, _, q| {
            assert_eq!(q, [0x00, 0x10]);
            s.push_input("", aj179_reply(80, 1, 1));
        });
        let out = ajazz_with(
            &[coll(AJAZZ_RECEIVER_VID, 0x1234, 2, 0xFF00, 1, 33, 33, 0)],
            &mut f,
            &mut 0,
        );
        assert_eq!(
            (out[0].name.as_str(), out[0].percent, out[0].charging),
            ("Ajazz AJ179", Some(80), true)
        );
        assert_eq!(out[0].key, "ajazz:249A:1234");
        let mut f = Fake::with(|s, _, _, _| s.push_input("", aj179_reply(80, 0, 0)));
        let out = ajazz_with(
            &[coll(AJAZZ_RECEIVER_VID, 0x1234, 2, 0xFF00, 1, 33, 33, 0)],
            &mut f,
            &mut 0,
        );
        assert!(out.is_empty(), "receiver without its mouse");
    }

    #[test]
    fn aj_series_rejects_reconnect_garbage() {
        assert_eq!(aj_series_parse(&[5, 0, 0, 77]), Some(77));
        assert_eq!(aj_series_parse(&[5, 1, 0, 77]), None);
        assert_eq!(aj_series_parse(&[5, 0, 0, 101]), None);
        let mut f = Fake::with(|s, _, _, q| {
            assert_eq!(q[..2], [0x00, 0xF7]);
            s.push_feature("", vec![5, 0, 0, 58]);
        });
        let mut c = coll(AJ_SERIES_VID, 0x5007, 2, 0xFFFF, 2, 0, 0, 65);
        c.product = "AJ159 APEX".into();
        let out = ajazz_with(&[c], &mut f, &mut 0);
        assert_eq!(
            (out[0].name.as_str(), out[0].percent),
            ("AJ159 APEX", Some(58))
        );
    }

    #[test]
    fn aj139_tries_the_second_request_variant() {
        let mut f = Fake::with(|s, _, _, q| {
            if q[4] == 0x0B {
                let mut r = vec![0u8; 64];
                r[..2].copy_from_slice(&[0xAA, 0x30]);
                r[8] = 66;
                s.push_input("", r);
            }
        });
        let out = ajazz_with(
            &[coll(
                AJ139_DONGLE_VID,
                AJ139_PID,
                2,
                0xFF01,
                0x10,
                65,
                65,
                0,
            )],
            &mut f,
            &mut 0,
        );
        assert_eq!(out[0].percent, Some(66));
        assert_eq!(f.s().sent.len(), 2);
    }

    #[test]
    fn delux_m800_states() {
        let mut r = vec![0u8; 33];
        r[0] = 0x0C;
        r[2] = 0x20;
        r[6] = 1;
        r[18] = 55;
        assert_eq!(
            delux_parse(&r),
            Some(Reading {
                percent: Some(55),
                charging: false,
                connected: true
            })
        );
        r[19] = 2;
        assert_eq!(delux_parse(&r).unwrap().percent, Some(100));
        r[19] = 0;
        r[6] = 0;
        r[18] = 0;
        assert!(!delux_parse(&r).unwrap().connected);
        let mut seq = 0;
        let mut f = Fake::with(|s, _, _, q| {
            assert_eq!(q[..3], [0x0C, 0x01, 0x20]);
            let mut r = vec![0u8; 33];
            r[0] = 0x0C;
            r[2] = 0x20;
            r[6] = 1;
            r[18] = 42;
            r[19] = 1;
            s.push_feature("", r);
        });
        let out = ajazz_with(
            &[coll(DELUX_VID, DELUX_RECEIVER, 0, 0xFF00, 1, 0, 0, 33)],
            &mut f,
            &mut seq,
        );
        assert_eq!(
            (out[0].percent, out[0].charging, out[0].key.as_str()),
            (Some(42), true, "delux:m800")
        );
        assert_eq!(seq, 1);
    }

    #[test]
    fn incott_status_and_w1_detection() {
        assert_eq!(incott_parse(&[9, 0x89, 0x80 | 61, 0x22]), Some((61, true)));
        assert_eq!(incott_parse(&[9, 0x89, 0, 0]), None, "stale");
        assert_eq!(w1_model(&[9, 0x8F, 0, 18]), Some(18));
        assert_eq!(w1_model(&[9, 0x8F, 0, 2]), Some(0));
        // A dongle that answers 8F with an Incott model code is read with the Incott query.
        let mut f = Fake::with(|s, _, _, q| {
            let r = if q[1] == 0x8F {
                vec![9, 0x8F, 0, 2, 0, 0, 0, 0, 0]
            } else {
                vec![9, 0x89, 73, 0x31, 0, 0, 0, 0, 0]
            };
            s.push_feature("", r);
        });
        let mut known = [0, 0];
        let out = pixart_with(
            &[coll(PIXART_VID, 0x522C, 1, 0xFF05, 1, 9, 0, 9)],
            &mut f,
            &mut known,
        );
        assert_eq!(
            (out[0].name.as_str(), out[0].percent),
            ("Incott mouse", Some(73))
        );
        assert_eq!(known, [0, 0]);
    }
}

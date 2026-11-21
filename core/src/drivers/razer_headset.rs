//! Razer headsets with their own vendor protocols (not the 90-byte Razer feature report), VID 1532,
//! exact product ids only (the Razer mouse driver shares the vendor id):
//! - BlackShark V3 / V3 Pro (0576/0577 V3 Pro, 0579/057A V3, 0A4E/0A55 V3 Pro for Xbox), interface 5:
//!   64-byte output report 02, class 21 = battery [level %][charging], class 2A = cable. The headset
//!   drops its link if asked every few seconds, so it is asked at most every 5 minutes.
//! - BlackShark V3 X (057D dongle, 057C cable): feature report 07, class 07 cmd 80 / 84.
//! - BlackShark V2 HyperSpeed (0565): MediaTek frame [02][00][60|seq]..[80][cmd].., GET 21 / 2A.
//! - BlackShark V2 Pro 2020 (0528): feature report FF on FF00:0001.
//! - BlackShark V2 Pro 2023 (0555): "PA" envelope with remote mode around each query.
//! - Barracuda X 2022 (0550 YS-Tech receiver with CRC-16 framing, 0552 Macronix receiver): Airoha
//!   commands with remote mode, customer-data GET 21 (battery) / 2A (cable).
//!
//! Only the battery / charging queries (and the remote-mode toggles and handshake BatteryHub sends,
//! which only route the query and are not stored on the headset) are ever sent.
//!
//! Ported from BatteryHub (src/drivers/razer_headset.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0, which credits OpenRazer
//! (PRs #2794, #2862, #2899, issue #1280), HaloBattery, ReedME/omarchy-blackshark-v3,
//! justik13/razer-blackshark-v2-hyperspeed-webhid, mehdibouchami/razer-barracuda-x-battery,
//! nightdevx/barracuda-x-battery and KhromotozzDevOut/razer-barracuda-battery-tray.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::hid::{self, Coll, Dev, Hid, crc16_xmodem, find, read_until};

pub const VID: u16 = 0x1532;
pub const PIDS: &[u16] = &[
    0x0565, 0x0550, 0x0552, 0x0528, 0x0555, 0x057D, 0x057C, 0x0576, 0x0577, 0x0579, 0x057A, 0x0A4E,
    0x0A55,
];
const MIN_INTERVAL: Duration = Duration::from_secs(5 * 60);
const REPLY: Duration = hid::MAX_WAIT;

/// Kept across polls.
pub struct State {
    hs_seq: u8,
    v3x_txid: u8,
    /// Last reading per product for the headsets asked at most every 5 minutes.
    cache: Vec<(u16, Instant, Device)>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            hs_seq: 0,
            v3x_txid: 0x40,
            cache: Vec::new(),
        }
    }
}

impl State {
    fn cached(&self, pid: u16) -> Option<Device> {
        self.cache
            .iter()
            .find(|(p, at, _)| *p == pid && at.elapsed() < MIN_INTERVAL)
            .map(|(_, _, d)| d.clone())
    }
    fn remember(&mut self, pid: u16, d: &Device) {
        self.cache.retain(|(p, _, _)| *p != pid);
        self.cache.push((pid, Instant::now(), d.clone()));
    }
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn headset(prefix: &str, pid: u16, name: &str, via: &str) -> Device {
    Device::new(format!("{prefix}:{pid:04X}"), name, Kind::Headset, via)
}

/// Reads until `accept` takes a report; a read shorter than `min` (or a timeout) ends the wait.
fn read_min(
    io: &mut dyn Dev,
    timeout: Duration,
    min: usize,
    accept: impl Fn(&[u8]) -> bool,
) -> Option<Vec<u8>> {
    read_until(io, timeout, |r| r.len() < min || accept(r)).filter(|r| r.len() >= min)
}

// ---- BlackShark V3 / V3 Pro ---------------------------------------------------------------------

/// [02][00][60][00 00][00][size 04][00 00][dir][class].. [62] = XOR of bytes 0..61.
pub fn v3_packet(dir: u8, cls: u8) -> [u8; 64] {
    let mut b = [0u8; 64];
    b[0] = 0x02;
    b[2] = 0x60;
    b[6] = 0x04;
    b[9] = dir;
    b[10] = cls;
    b[62] = b[..62].iter().fold(0, |x, v| x ^ v);
    b
}

fn v3_get(io: &mut dyn Dev, cls: u8) -> Option<Vec<u8>> {
    if !io.write(&v3_packet(0x80, cls)) {
        return None;
    }
    read_min(io, REPLY, 15, |r| {
        r[0] == 0x02 && r[2] == 0x60 && r[10] == cls
    })
}

fn v3_name(pid: u16) -> &'static str {
    match pid {
        0x0A4E | 0x0A55 => "Razer BlackShark V3 Pro for Xbox",
        0x0576 | 0x0577 => "Razer BlackShark V3 Pro",
        _ => "Razer BlackShark V3",
    }
}

fn poll_v3(colls: &[Coll], hid: &mut dyn Hid, st: &mut State, out: &mut Vec<Device>) {
    for pid in [0x0576, 0x0577, 0x0579, 0x057A, 0x0A4E, 0x0A55] {
        let colls = find(colls, VID, pid, |c| {
            c.iface == 5 && c.out_len >= 64 && c.in_len >= 15
        });
        let Some(c) = colls.first() else { continue };
        if let Some(d) = st.cached(pid) {
            out.push(d);
            continue;
        }
        let Some(mut io) = hid.open(c) else { continue };
        let io = io.as_mut();
        // Handshake the headset expects before reads (init, charging state on both links).
        io.write(&v3_packet(0x00, 0x02));
        io.write(&v3_packet(0x00, 0x2A));
        io.write(&v3_packet(0x80, 0x2A));
        io.sleep(Duration::from_millis(250));
        io.flush();
        let Some(r) = v3_get(io, 0x21).filter(|r| r[13] <= 100) else {
            continue;
        };
        let cable = v3_get(io, 0x2A).is_some_and(|c| c[13] == 1);
        // The lower id of each pair is the USB cable, the higher one the wireless receiver.
        let via = if matches!(pid, 0x0576 | 0x0579 | 0x0A4E) {
            "USB cable"
        } else {
            "2.4 GHz receiver"
        };
        let mut d = headset("blackshark", pid, v3_name(pid), via);
        d.percent = Some(r[13]);
        d.charging = r[14] != 0 || cable;
        st.remember(pid, &d);
        out.push(d);
    }
}

// ---- BlackShark V3 X ----------------------------------------------------------------------------

/// Feature report 07: [07][status][txid][00 00 00][argc 02][class 07][cmd].. [62] = XOR of 3..60.
pub fn v3x_request(txid: u8, cmd: u8) -> [u8; 64] {
    let mut req = [0u8; 64];
    req[0] = 0x07;
    req[2] = txid;
    req[6] = 0x02;
    req[7] = 0x07;
    req[8] = cmd;
    req[62] = req[3..=60].iter().fold(0, |x, v| x ^ v);
    req
}

#[derive(Debug, PartialEq)]
pub enum V3xReply {
    Value(u8),
    /// Not the reply to this request.
    Other,
    /// Failure status (e.g. headset off).
    Failed,
}

pub fn v3x_reply(r: &[u8], txid: u8, cmd: u8) -> V3xReply {
    if r.len() < 63 || r[2] != txid || r[7] != 0x07 || r[8] != cmd || r[6] < 2 {
        return V3xReply::Other;
    }
    if r[1] != 0x02 && r[1] != 0x01 {
        return V3xReply::Failed;
    }
    V3xReply::Value(r[10])
}

fn v3x_get(io: &mut dyn Dev, st: &mut State, cmd: u8) -> Option<u8> {
    for _ in 0..4 {
        st.v3x_txid = st.v3x_txid.wrapping_add(1);
        let txid = st.v3x_txid;
        if !io.set_feature(&v3x_request(txid, cmd)) {
            return None;
        }
        io.sleep(Duration::from_millis(60));
        let Some(r) = io.get_feature(0x07) else {
            continue;
        };
        match v3x_reply(&r, txid, cmd) {
            V3xReply::Value(v) => return Some(v),
            V3xReply::Other => {}
            V3xReply::Failed => return None,
        }
    }
    None
}

fn poll_v3x(colls: &[Coll], hid: &mut dyn Hid, st: &mut State, out: &mut Vec<Device>) {
    for pid in [0x057D, 0x057C] {
        let colls = find(colls, VID, pid, |c| c.iface == 0 && c.feat_len >= 64);
        let Some(c) = colls.first() else { continue };
        if let Some(d) = st.cached(pid) {
            out.push(d);
            continue;
        }
        let Some(mut io) = hid.open(c) else { continue };
        let Some(level) = v3x_get(io.as_mut(), st, 0x80).filter(|l| *l <= 100) else {
            continue;
        };
        let charging = v3x_get(io.as_mut(), st, 0x84);
        let via = if pid == 0x057D {
            "2.4 GHz receiver"
        } else {
            "USB cable"
        };
        let mut d = headset("blackshark", pid, "Razer BlackShark V3 X", via);
        d.percent = Some(level);
        d.charging = charging == Some(1);
        st.remember(pid, &d);
        out.push(d);
    }
}

// ---- BlackShark V2 HyperSpeed -------------------------------------------------------------------

/// [02][00][seq][00 00 00][04][00][00][80][cmd]..[62] = XOR of bytes 0..61.
pub fn hs_frame(cmd: u8, seq: u8) -> [u8; 64] {
    let mut b = [0u8; 64];
    b[0] = 0x02;
    b[2] = seq;
    b[6] = 0x04;
    b[9] = 0x80;
    b[10] = cmd;
    b[62] = b[..62].iter().fold(0, |x, v| x ^ v);
    b
}

/// Value of a reply to (cmd, seq); the reply's checksum covers bytes 1..61.
pub fn hs_reply(r: &[u8], cmd: u8, seq: u8) -> Option<u8> {
    if r.len() < 64 || r[0] != 0x02 || r[2] != seq || r[10] != cmd || r[12] < 1 {
        return None;
    }
    (r[1..62].iter().fold(0, |x, v| x ^ v) == r[62]).then_some(r[13])
}

fn hs_query(io: &mut dyn Dev, st: &mut State, cmd: u8) -> Option<u8> {
    let seq = 0x60 | (st.hs_seq & 0x1F);
    st.hs_seq = st.hs_seq.wrapping_add(1);
    io.flush();
    if !io.write(&hs_frame(cmd, seq)) {
        return None;
    }
    let r = read_until(io, REPLY, |r| hs_reply(r, cmd, seq).is_some())?;
    hs_reply(&r, cmd, seq)
}

fn poll_v2_hyperspeed(colls: &[Coll], hid: &mut dyn Hid, st: &mut State, out: &mut Vec<Device>) {
    const PID: u16 = 0x0565;
    let colls = find(colls, VID, PID, |c| {
        (c.page == 0xFF14 || c.page == 0xFF00)
            && c.usage == 0x01
            && c.out_len >= 64
            && c.in_len >= 64
    });
    // The specific vendor control interface first, then firmware exposing it on FF00.
    for page in [0xFF14, 0xFF00] {
        for c in colls.iter().filter(|c| c.page == page) {
            let Some(mut io) = hid.open(c) else { continue };
            let Some(level) = hs_query(io.as_mut(), st, 0x21).filter(|l| *l <= 100) else {
                continue;
            };
            let charging = hs_query(io.as_mut(), st, 0x2A);
            let mut d = headset(
                "blackshark",
                PID,
                "Razer BlackShark V2 HyperSpeed",
                "2.4 GHz receiver",
            );
            d.percent = Some(level);
            d.charging = charging.is_some_and(|c| c > 0);
            out.push(d);
            return;
        }
    }
}

// ---- BlackShark V2 Pro (2020) -------------------------------------------------------------------

const V2PRO2020_REPLY: [u8; 9] = [0xFF, 0x0F, 0x05, 0xFE, 0x12, 0x04, 0x1F, 0x08, 0x05];
const V2PRO2020_STALE: [u8; 9] = [0xFF, 0x01, 0x00, 0xFE, 0x12, 0x04, 0x1F, 0x08, 0x05];

/// Device from a fresh feature reply: Off when the headset is not linked (no voltage).
pub fn v2pro2020_device(r: &[u8], fresh: bool) -> Option<Device> {
    let mv = if fresh {
        u16::from_be_bytes([r[12], r[13]])
    } else {
        0
    };
    if mv == 0 {
        return None;
    }
    let mut d = headset(
        "blackshark",
        0x0528,
        "Razer BlackShark V2 Pro (2020)",
        "2.4 GHz receiver",
    );
    d.millivolts = Some(mv);
    d.charging = r[11] == 0x09;
    // The receiver can jump to 80% as soon as charging starts, even from 10%.
    if !d.charging && r[14] <= 100 {
        d.percent = Some(r[14]);
    }
    Some(d)
}

fn poll_v2_pro_2020(colls: &[Coll], hid: &mut dyn Hid, out: &mut Vec<Device>) {
    const LEN: usize = 64;
    const READS: usize = 5;
    for c in find(colls, VID, 0x0528, |c| {
        c.iface == 3 && c.page == 0xFF00 && c.usage == 0x0001 && c.feat_len as usize == LEN
    }) {
        let Some(mut io) = hid.open(c) else { continue };
        if !io.set_feature(&[0xFF, 0x0A, 0x00, 0xFD, 0x04, 0x12, 0xF1, 0x02, 0x05]) {
            continue;
        }
        for read in 0..READS {
            io.sleep(Duration::from_millis(100)); // Synapse waits about 100 ms for the receiver to fetch headset status.
            let Some(r) = io.get_feature(0xFF).filter(|r| r.len() == LEN) else {
                continue;
            };
            let fresh = r.starts_with(&V2PRO2020_REPLY);
            // Give an in-flight request time to replace the old payload before reporting offline.
            let offline = read == READS - 1 && r.starts_with(&V2PRO2020_STALE);
            if !fresh && !offline {
                continue;
            }
            out.extend(v2pro2020_device(&r, fresh));
            return;
        }
    }
}

// ---- BlackShark V2 Pro (2023): "PA" envelope ----------------------------------------------------

pub fn pa_frame(total: u8, inner: u8, kind: u8, cmd: u8, flag: u8) -> [u8; 64] {
    let mut b = [0u8; 64];
    b[..3].copy_from_slice(&[0x02, 0x80, total]);
    b[5] = b'P';
    b[6] = b'A';
    b[7] = inner;
    b[9] = kind;
    b[10] = cmd;
    b[11] = flag;
    b
}

fn pa_remote(io: &mut dyn Dev, on: bool) {
    io.write(&pa_frame(0x07, 0x0E, 0x02, 0xE1, u8::from(on)));
    io.sleep(Duration::from_millis(35));
}

/// First payload byte of the reply to `cmd`, or None when the headset does not answer.
fn pa_query(io: &mut dyn Dev, cmd: u8) -> Option<u8> {
    io.flush();
    pa_remote(io, true); // sacrificial: wakes a dozing link
    let mut value = None;
    for _ in 0..3 {
        pa_remote(io, true);
        if !io.write(&pa_frame(0x08, 0x08, 0x03, cmd, 0)) {
            break;
        }
        value = read_min(io, Duration::from_millis(150), 16, |r| {
            r[0] == 0x02 && r[12] == cmd && r[13] == 0x01 && r[14] >= 1
        })
        .map(|r| r[15]);
        if value.is_some() {
            break;
        }
    }
    pa_remote(io, false);
    value
}

fn poll_v2_pro_2023(colls: &[Coll], hid: &mut dyn Hid, out: &mut Vec<Device>) {
    const PID: u16 = 0x0555;
    for c in find(colls, VID, PID, |c| {
        c.page == 0xFF00 && c.out_len >= 64 && c.in_len >= 16
    }) {
        let Some(mut io) = hid.open(c) else { continue };
        let Some(level) = pa_query(io.as_mut(), 0x21).filter(|l| *l <= 100) else {
            continue;
        };
        let charging = pa_query(io.as_mut(), 0x2A);
        let mut d = headset(
            "blackshark",
            PID,
            "Razer BlackShark V2 Pro (2023)",
            "2.4 GHz receiver",
        );
        d.percent = Some(level);
        d.charging = charging.is_some_and(|c| c > 0);
        out.push(d);
        return;
    }
}

// ---- Barracuda X (2022) -------------------------------------------------------------------------

/// Output report carrying one Airoha command: Macronix (0552) [01][80][len][payload..], YS-Tech
/// (0550) [02][80][len][crc lo][crc hi][payload..] with the CRC over the report, CRC bytes zero.
pub fn barracuda_frame(payload: &[u8], macronix: bool) -> [u8; 64] {
    let mut f = [0u8; 64];
    f[1] = 0x80;
    f[2] = payload.len() as u8;
    if macronix {
        f[0] = 0x01;
        f[3..3 + payload.len()].copy_from_slice(payload);
    } else {
        f[0] = 0x02;
        f[5..5 + payload.len()].copy_from_slice(payload);
        let crc = crc16_xmodem(&f);
        f[3..5].copy_from_slice(&crc.to_le_bytes());
    }
    f
}

const MAGIC: [u8; 2] = [0x50, 0x49];

/// Reassembles the Airoha frames [50 49][..][len lo][len hi][payload] carried in input reports.
#[derive(Default)]
pub struct Stream(Vec<u8>);

impl Stream {
    /// Appends the stream bytes of one input report.
    pub fn push(&mut self, r: &[u8], macronix: bool) {
        if macronix {
            if r.len() >= 3 && r[0] == 0x01 && r[1] == 0x80 && r[2] as usize + 3 <= r.len() {
                self.0.extend_from_slice(&r[3..3 + r[2] as usize]);
            }
        } else if r.len() >= 2 && r[0] == 0x02 && r[1] as usize + 2 <= r.len() {
            self.0.extend_from_slice(&r[2..2 + r[1] as usize]);
        }
    }

    /// Next complete frame, skipping bytes before a frame start.
    pub fn next_frame(&mut self) -> Option<Vec<u8>> {
        if self.0.len() < 2 {
            return None;
        }
        let start = self
            .0
            .windows(2)
            .position(|w| w == MAGIC)
            .unwrap_or(self.0.len());
        self.0.drain(..start);
        if self.0.len() < 10 {
            return None;
        }
        let len = 10 + usize::from(u16::from_le_bytes([self.0[8], self.0[9]]));
        if self.0.len() < len {
            return None;
        }
        Some(self.0.drain(..len).collect())
    }
}

struct BarracudaLink<'a> {
    io: &'a mut dyn Dev,
    stream: Stream,
    seq: u8,
    macronix: bool,
}

impl BarracudaLink<'_> {
    /// The Macronix receiver has to be switched to app mode first: [01 40] -> [01 40 01 01].
    fn start(&mut self) -> bool {
        if !self.macronix {
            return true;
        }
        self.io.flush();
        if !self.io.write(&[0x01, 0x40]) {
            return false;
        }
        read_min(self.io, REPLY, 4, |r| r[0] == 0x01 && r[1] == 0x40)
            .is_some_and(|r| r[2] == 0x01 && r[3] == 0x01)
    }

    /// Sends one Airoha command [50 41][group][seq][body..].
    fn command(&mut self, group: u8, body: &[u8]) -> bool {
        self.seq = self.seq % 127 + 1;
        let mut payload = vec![0x50, 0x41, group, self.seq];
        payload.extend_from_slice(body);
        let f = barracuda_frame(&payload, self.macronix);
        if self.macronix {
            return self.io.write(&f);
        }
        // The YS-Tech bridge answers each write on input report 02: 'O' taken, 'F' busy.
        for attempt in 0..5 {
            if attempt > 0 {
                self.io.sleep(Duration::from_millis(50));
            }
            if !self.io.write(&f) {
                return false;
            }
            match self.io.get_input(0x02) {
                Some(s) if s.len() >= 2 && s[1] == b'O' => return true,
                Some(s) if s.len() >= 2 && s[1] == b'F' => {}
                _ => return false,
            }
        }
        false
    }

    /// First reply frame `accept` takes; unrelated frames are skipped.
    fn wait(&mut self, accept: impl Fn(&[u8]) -> bool, timeout: Duration) -> Option<Vec<u8>> {
        let end = Instant::now() + timeout;
        loop {
            while let Some(f) = self.stream.next_frame() {
                if accept(&f) {
                    return Some(f);
                }
            }
            let left = end.saturating_duration_since(Instant::now()).as_millis() as u64;
            if left == 0 {
                return None;
            }
            let r = self
                .io
                .read(Duration::from_millis(left))
                .filter(|r| r.len() >= 2)?;
            self.stream.push(&r, self.macronix);
        }
    }

    /// Remote mode on/off; true when the headset acknowledged it.
    fn remote(&mut self, on: bool) -> bool {
        if !self.command(0x0E, &[0x02, 0xE1, u8::from(on)]) {
            return false;
        }
        let seq = 0x80 | self.seq;
        self.wait(|r| r.len() >= 13 && r[10] == 0x0E && r[11] == seq, REPLY)
            .is_some_and(|f| f[12] == 0)
    }

    /// Customer-data GET: the first value byte.
    fn get(&mut self, cmd: u8) -> Option<u8> {
        if !self.command(0x08, &[0x03, cmd, 0x00, 0x00]) {
            return None;
        }
        let f = self.wait(
            |r| r.len() >= 13 && r[2] == 0x08 && r[10] == cmd && r[11] == 0x01,
            REPLY,
        )?;
        (f[12] >= 1 && f.len() >= 14).then(|| f[13])
    }
}

fn poll_barracuda_x(colls: &[Coll], hid: &mut dyn Hid, out: &mut Vec<Device>) {
    for pid in [0x0550, 0x0552] {
        let macronix = pid == 0x0552;
        let usage = if macronix { 0x0000 } else { 0x0001 };
        for c in find(colls, VID, pid, |c| {
            c.page == 0xFF00 && c.usage == usage && c.out_len >= 64 && c.in_len >= 64
        }) {
            let Some(mut io) = hid.open(c) else { continue };
            let mut link = BarracudaLink {
                io: io.as_mut(),
                stream: Stream::default(),
                seq: 0,
                macronix,
            };
            // No ack: receiver without a connected headset (off, out of range, Bluetooth mode).
            if !link.start() || !link.remote(true) {
                continue;
            }
            let level = link.get(0x21);
            let cable = if level.is_some() {
                link.get(0x2A)
            } else {
                None
            };
            link.remote(false);
            let Some(level) = level.filter(|l| *l <= 100) else {
                continue;
            };
            let mut d = headset(
                "barracuda",
                pid,
                "Razer Barracuda X (2022)",
                "2.4 GHz receiver",
            );
            d.percent = Some(level);
            d.charging = cable == Some(1);
            out.push(d);
            break;
        }
    }
}

pub fn poll_colls(colls: &[Coll], hid: &mut dyn Hid, st: &mut State) -> Vec<Device> {
    let mut out = Vec::new();
    poll_v2_hyperspeed(colls, hid, st, &mut out);
    poll_barracuda_x(colls, hid, &mut out);
    poll_v2_pro_2020(colls, hid, &mut out);
    poll_v2_pro_2023(colls, hid, &mut out);
    poll_v3x(colls, hid, st, &mut out);
    poll_v3(colls, hid, st, &mut out);
    out
}

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    let mut guard = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let st = guard.get_or_insert_with(State::default);
    hid::run(api, infos, |hid, colls| poll_colls(colls, hid, st))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::hid::fake::{Fake, Op, queued};

    fn run(colls: &[Coll], f: &Fake, st: &mut State) -> Vec<Device> {
        poll_colls(colls, &mut f.clone(), st)
    }

    #[test]
    fn v3_packet_and_reply() {
        let p = v3_packet(0x80, 0x21);
        assert_eq!(p[..11], [2, 0, 0x60, 0, 0, 0, 4, 0, 0, 0x80, 0x21]);
        assert_eq!(p[62], 2 ^ 0x60 ^ 4 ^ 0x80 ^ 0x21);

        let mut c = Coll::new(VID, 0x057A, 0xFF00, 1, 64, 64);
        c.iface = 5;
        let f = Fake::replies(|w| {
            if w[9] == 0x80 && w[10] == 0x21 {
                let mut r = vec![0u8; 64];
                r[..3].copy_from_slice(&[0x02, 0, 0x60]);
                r[10] = 0x21;
                r[13] = 64;
                vec![r]
            } else {
                vec![]
            }
        });
        let mut st = State::default();
        let out = run(std::slice::from_ref(&c), &f, &mut st);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].key, "blackshark:057A");
        assert_eq!(out[0].name, "Razer BlackShark V3");
        assert_eq!(out[0].via, "2.4 GHz receiver");
        assert_eq!((out[0].percent, out[0].charging), (Some(64), false));
        // Three handshake writes, the battery and the cable query.
        assert_eq!(f.s().writes().len(), 5);
        // Asked again within 5 minutes: the cached reading, no I/O.
        let again = run(std::slice::from_ref(&c), &f, &mut st);
        assert_eq!(again[0].percent, Some(64));
        assert_eq!(f.s().writes().len(), 5);
    }

    #[test]
    fn v3x_request_and_reply() {
        let req = v3x_request(0x41, 0x80);
        assert_eq!(req[..9], [7, 0, 0x41, 0, 0, 0, 2, 7, 0x80]);
        assert_eq!(req[62], 2 ^ 7 ^ 0x80);
        let mut r = vec![0u8; 64];
        r[..11].copy_from_slice(&[7, 2, 0x41, 0, 0, 0, 2, 7, 0x80, 0, 77]);
        assert_eq!(v3x_reply(&r, 0x41, 0x80), V3xReply::Value(77));
        assert_eq!(v3x_reply(&r, 0x42, 0x80), V3xReply::Other);
        r[1] = 0x03; // headset off
        assert_eq!(v3x_reply(&r, 0x41, 0x80), V3xReply::Failed);
    }

    #[test]
    fn v2_hyperspeed_frame_round_trip() {
        let f = hs_frame(0x21, 0x63);
        assert_eq!(f[..11], [2, 0, 0x63, 0, 0, 0, 4, 0, 0, 0x80, 0x21]);
        let mut r = f;
        r[12] = 1;
        r[13] = 58;
        r[62] = r[1..62].iter().fold(0, |x, v| x ^ v);
        assert_eq!(hs_reply(&r, 0x21, 0x63), Some(58));
        assert_eq!(hs_reply(&r, 0x21, 0x64), None);
        r[62] ^= 1;
        assert_eq!(hs_reply(&r, 0x21, 0x63), None);
    }

    #[test]
    fn v2_pro_2020_fresh_and_offline() {
        let mut r = vec![0u8; 64];
        r[..9].copy_from_slice(&V2PRO2020_REPLY);
        r[12..15].copy_from_slice(&[0x0F, 0x3C, 67]);
        let d = v2pro2020_device(&r, true).unwrap();
        assert_eq!(
            (d.percent, d.millivolts, d.charging),
            (Some(67), Some(3900), false)
        );
        r[11] = 0x09; // charging: the level is not trusted
        let d = v2pro2020_device(&r, true).unwrap();
        assert_eq!((d.percent, d.charging), (None, true));
        assert!(v2pro2020_device(&r, false).is_none());

        let mut c = Coll::new(VID, 0x0528, 0xFF00, 1, 0, 0);
        c.iface = 3;
        c.feat_len = 64;
        let reply = r.clone();
        // Each feature report sent makes this reply the one GetFeature returns.
        let f = Fake::with(move |s, c, op| {
            if let Op::SetFeature(_) = op {
                s.feature.insert(String::new(), vec![reply.clone()].into());
            }
            queued(s, c, op)
        });
        let out = run(&[c], &f, &mut State::default());
        assert_eq!(out[0].key, "blackshark:0528");
        assert!(out[0].charging);
    }

    #[test]
    fn v2_pro_2023_pa_query() {
        let c = Coll::new(VID, 0x0555, 0xFF00, 1, 64, 64);
        let f = Fake::replies(|w| {
            if w[9] == 0x03 {
                let mut r = vec![0u8; 64];
                r[0] = 0x02;
                r[12] = w[10];
                r[13] = 0x01;
                r[14] = 1;
                r[15] = if w[10] == 0x21 { 45 } else { 1 };
                vec![r]
            } else {
                vec![]
            }
        });
        let out = run(&[c], &f, &mut State::default());
        assert_eq!((out[0].percent, out[0].charging), (Some(45), true));
        assert_eq!(out[0].name, "Razer BlackShark V2 Pro (2023)");
        // Every query is wrapped in remote on (twice) / off.
        let writes = f.s().writes().clone();
        assert_eq!(writes[0][..3], [0x02, 0x80, 0x07]);
        assert_eq!(writes.last().unwrap()[11], 0);
    }

    /// An Airoha reply frame [50 49][group][..][len lo][len hi][payload].
    fn airoha(group: u8, b10: u8, b11: u8, payload: &[u8]) -> Vec<u8> {
        let mut f = vec![0x50, 0x49, group, 0, 0, 0, 0, 0];
        let len = 2 + payload.len() as u16;
        f.extend_from_slice(&len.to_le_bytes());
        f.extend_from_slice(&[b10, b11]);
        f.extend_from_slice(payload);
        f
    }

    #[test]
    fn barracuda_stream_reassembles_split_frames() {
        let frame = airoha(0x08, 0x21, 0x01, &[1, 72]);
        let mut s = Stream::default();
        let mut first = vec![0x02, 6, 0xAA];
        first.extend_from_slice(&frame[..5]);
        let mut second = vec![0x02, (frame.len() - 5) as u8];
        second.extend_from_slice(&frame[5..]);
        s.push(&first, false);
        assert!(s.next_frame().is_none());
        s.push(&second, false);
        assert_eq!(s.next_frame(), Some(frame));
    }

    #[test]
    fn barracuda_x_ystech_reads_level_and_cable() {
        let c = Coll::new(VID, 0x0550, 0xFF00, 1, 64, 64);
        let reply = |w: &[u8]| {
            // CRC over the report with its CRC bytes zero.
            let mut z = w.to_vec();
            z[3] = 0;
            z[4] = 0;
            assert_eq!(crc16_xmodem(&z).to_le_bytes(), [w[3], w[4]]);
            let (group, seq) = (w[7], w[8]);
            let frame = match group {
                0x0E => airoha(0x0E, 0x0E, 0x80 | seq, &[0]),
                0x08 if w[10] == 0x21 => airoha(0x08, 0x21, 0x01, &[1, 72]),
                0x08 => airoha(0x08, 0x2A, 0x01, &[1, 1]),
                _ => return vec![],
            };
            let mut r = vec![0x02, frame.len() as u8];
            r.extend_from_slice(&frame);
            vec![r]
        };
        // Every input report read over the control pipe says 'O'; writes queue `reply`.
        let f = Fake::with(move |s, c, op| match op {
            Op::GetInput(_) => Some(vec![0x02, b'O']),
            Op::Write(w) => {
                s.input.entry(String::new()).or_default().extend(reply(w));
                queued(s, c, op)
            }
            _ => queued(s, c, op),
        });
        let out = run(&[c], &f, &mut State::default());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].key, "barracuda:0550");
        assert_eq!((out[0].percent, out[0].charging), (Some(72), true));
    }
}

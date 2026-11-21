//! Keyboards on the shared "Wireless 2.4G Dongle" 36B0:3002, and PMO Hub keyboards on their USB cable
//! (36B0:3009 WAVE 75, 3004 Aurora 80, 302B PMO P75, 3012 PMO 80).
//!
//! Raw HID channel on interface 1, FF60:0061, 33-byte reports on the dongle and 65-byte on the cable
//! (report id 0 + payload). Two firmware families answer there:
//! 1. PMO Hub: frames [00] AA <cmd> <offset LE> <len> 00 00 ..., replies echo AA <cmd> <offset> with
//!    55 (OK) at payload byte 7. START_COMM (10, len = payload - 8), GET_BATTERY (30, len 18:
//!    reply byte 8 = percent), STOP_COMM (11) always closes the session. The dongle's START reply
//!    names the paired keyboard (36B0:<pid> LE at bytes 9..12).
//! 2. VIA (URX Core75, EVO80): custom get value 08 00 07 (channel 0, value 7 = battery);
//!    reply 08 00 07 <level>. FF ("unhandled") is ignored.
//!
//! The dongle tries the likely protocol first, then the other, and from then on only the one that
//! answered; a dongle answering neither is retried with a growing back-off. Both only read.
//!
//! Ported from BatteryHub (src/drivers/via_dongle.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0, which credits pmohub.cn's
//! web driver (issue 343) and HeyOkay/HaloBattery (providers/urx.py).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::hidport::{
    BUDGET, CABLE, Coll, Opener, Port, RECEIVER, RECEIVER_GRACE, colls, hid_opener,
};

pub const VID: u16 = 0x36B0;
const DONGLE: u16 = 0x3002;

/// pmohub.cn's device list: the keyboard's cable pid (also what the dongle reports as paired).
const PMO_BOARDS: [(u16, &str); 4] = [
    (0x3009, "WAVE 75"),
    (0x3004, "Aurora 80"),
    (0x302B, "PMO P75"),
    (0x3012, "PMO 80"),
];

pub const PIDS: &[u16] = &[DONGLE, 0x3009, 0x3004, 0x302B, 0x3012];

fn pmo_name(pid: u16) -> Option<&'static str> {
    PMO_BOARDS.iter().find(|b| b.0 == pid).map(|b| b.1)
}

fn is_raw_hid(c: &Coll) -> bool {
    c.iface == 1
        && c.page == 0xFF60
        && c.usage == 0x0061
        && c.in_len == c.out_len
        && (c.out_len == 33 || c.out_len == 65)
}

/// Sends a request and waits for the reply starting with `echo` (skipping status pushes).
fn ask(port: &mut dyn Port, req: &[u8], echo: &[u8], ms: u64) -> Option<Vec<u8>> {
    port.flush();
    if !port.write(req) {
        return None;
    }
    crate::drivers::hidport::wait_for(port, Duration::from_millis(ms), |r| {
        r.len() >= echo.len() + 6 && r.starts_with(echo)
    })
}

pub fn pmo_frame(payload: usize, cmd: u8, len: u8) -> Vec<u8> {
    let mut f = vec![0u8; 1 + payload];
    f[1] = 0xAA;
    f[2] = cmd;
    f[5] = len;
    f
}

fn pmo_ok(r: &Option<Vec<u8>>) -> bool {
    r.as_ref().is_some_and(|r| r.len() > 8 && r[7] == 0x55)
}

/// PMO Hub session: (percent or None, keyboard pid the dongle reported as paired).
fn pmo_query(port: &mut dyn Port, out_len: u16) -> (Option<u8>, Option<u16>) {
    let payload = usize::from(out_len) - 1; // 32 or 64
    let len = (payload - 8) as u8; // 0x18 or 0x38, as pmohub.cn uses
    let start_echo = [0xAA, 0x10, 0x00, 0x00];
    let stop = pmo_frame(payload, 0x11, len);
    let stop_echo = [0xAA, 0x11, 0x00, 0x00];
    let start = ask(port, &pmo_frame(payload, 0x10, len), &start_echo, 600)
        .or_else(|| ask(port, &pmo_frame(payload, 0x10, len), &start_echo, 600)); // link waking up
    if !pmo_ok(&start) {
        // Close the session anyway in case the START was taken; don't wait when nothing answered.
        if start.is_none() {
            port.write(&stop);
        } else {
            ask(port, &stop, &stop_echo, 300);
        }
        return (None, None);
    }
    let start = start.unwrap_or_default();
    let paired = (start.len() > 12 && u16::from_le_bytes([start[9], start[10]]) == VID)
        .then(|| u16::from_le_bytes([start[11], start[12]]));
    let bat = ask(
        port,
        &pmo_frame(payload, 0x30, 0x18),
        &[0xAA, 0x30, 0x00, 0x00],
        600,
    );
    let level = bat
        .as_ref()
        .filter(|_| pmo_ok(&bat))
        .map(|b| b[8])
        .filter(|l| *l <= 100);
    ask(port, &stop, &stop_echo, 600); // always end the session
    (level, paired)
}

/// VIA custom value 7: the level (1..100), or None.
fn via_query(port: &mut dyn Port, out_len: u16) -> Option<u8> {
    let mut req = vec![0u8; usize::from(out_len)];
    req[1..4].copy_from_slice(&[0x08, 0x00, 0x07]);
    for _ in 0..2 {
        // The first request can be missed while the link wakes up.
        if let Some(r) = ask(port, &req, &[0x08, 0x00, 0x07], 600) {
            return Some(r[3]).filter(|l| (1..=100).contains(l));
        }
    }
    None
}

/// "EVO80 2.4G" -> "EVO80", "URX Core75 Wireless" -> "URX Core75"; the generic dongle name -> fallback.
fn via_name(product: &str) -> String {
    if product.contains("Dongle") {
        return "Wireless keyboard".into();
    }
    let mut name = product;
    for suffix in [" 2.4G", " Wireless"] {
        if name.len() > suffix.len() && name.ends_with(suffix) {
            name = &name[..name.len() - suffix.len()];
        }
    }
    if name.is_empty() {
        "Wireless keyboard".into()
    } else {
        name.into()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum Proto {
    #[default]
    Unknown,
    Pmo,
    Via,
}

/// Per-product state across polls.
#[derive(Clone, Debug, Default)]
pub struct State {
    proto: Proto,
    misses: u32,
    next_try: Option<Instant>,
    paired: Option<u16>,
}

impl State {
    fn missed(&mut self, now: Instant) {
        self.misses += 1;
        // A known protocol that stops answering most likely means a sleeping keyboard (or one moved to
        // its cable): retry soon. After several misses, forget it in case another keyboard was paired.
        if self.misses >= 6 {
            self.proto = Proto::Unknown;
        }
        let sec = if self.proto != Proto::Unknown {
            20
        } else {
            (30u64 << (self.misses - 1).min(4)).min(300)
        };
        self.next_try = Some(now + Duration::from_secs(sec));
    }

    fn answered(&mut self, p: Proto) {
        self.proto = p;
        self.misses = 0;
        self.next_try = None;
    }

    fn waiting(&self, now: Instant) -> bool {
        self.next_try.is_some_and(|t| now < t)
    }
}

fn make(key_pid: u16, name: String, via: &'static str, level: u8) -> Device {
    // Dongle and cable share the keyboard's pid, so the entry follows the keyboard.
    let mut d = Device::new(
        format!("via:{VID:04X}:{key_pid:04X}"),
        name,
        Kind::Keyboard,
        via,
    );
    d.percent = Some(level);
    d.grace = RECEIVER_GRACE; // keep the last value if a sleeping keyboard stops answering
    d
}

fn poll_cable(
    colls: &[Coll],
    open: &mut Opener,
    states: &mut HashMap<u16, State>,
    now: Instant,
    pid: u16,
) -> Option<Device> {
    let found: Vec<&Coll> = colls
        .iter()
        .filter(|c| c.is(VID, pid) && is_raw_hid(c))
        .collect();
    if found.is_empty() {
        states.remove(&pid); // unplugged: start fresh when it comes back
        return None;
    }
    let st = states.entry(pid).or_default();
    if st.waiting(now) {
        return None;
    }
    for c in found {
        let Some(mut port) = open(c) else { continue };
        let (Some(level), _) = pmo_query(port.as_mut(), c.out_len) else {
            continue;
        };
        st.answered(Proto::Pmo);
        return Some(make(
            pid,
            pmo_name(pid).unwrap_or_default().into(),
            CABLE,
            level,
        ));
    }
    st.missed(now);
    None
}

fn poll_dongle(
    colls: &[Coll],
    open: &mut Opener,
    states: &mut HashMap<u16, State>,
    now: Instant,
    cable_answered: bool,
) -> Option<Device> {
    let Some(c) = colls
        .iter()
        .find(|c| c.is(VID, DONGLE) && is_raw_hid(c) && c.out_len == 33)
    else {
        states.remove(&DONGLE);
        return None;
    };
    let st = states.entry(DONGLE).or_default();
    // A PMO keyboard on its cable leaves the dongle unanswered (START_COMM times out); skip it.
    if (cable_answered && st.proto != Proto::Via) || st.waiting(now) {
        return None;
    }
    let mut port = open(c)?;
    let mut order = [Proto::Pmo, Proto::Via];
    if st.proto == Proto::Via || (st.proto == Proto::Unknown && c.product.contains("URX")) {
        order.swap(0, 1);
    }
    let tries = if st.proto == Proto::Unknown { 2 } else { 1 }; // once known, only that protocol
    for proto in &order[..tries] {
        if *proto == Proto::Pmo {
            let (Some(level), paired) = pmo_query(port.as_mut(), c.out_len) else {
                continue;
            };
            if paired.is_some() {
                st.paired = paired;
            }
            st.answered(Proto::Pmo);
            let key_pid = st.paired.unwrap_or(DONGLE);
            let name = pmo_name(key_pid).unwrap_or("Wireless keyboard");
            return Some(make(key_pid, name.into(), RECEIVER, level));
        }
        let Some(level) = via_query(port.as_mut(), c.out_len) else {
            continue;
        };
        st.answered(Proto::Via);
        return Some(make(DONGLE, via_name(&c.product), RECEIVER, level));
    }
    st.missed(now);
    None
}

pub fn poll_with(
    colls: &[Coll],
    open: &mut Opener,
    states: &mut HashMap<u16, State>,
    now: Instant,
) -> Vec<Device> {
    let mut out = Vec::new();
    for (pid, _) in PMO_BOARDS {
        out.extend(poll_cable(colls, open, states, now, pid));
    }
    let cable_answered = !out.is_empty();
    out.extend(poll_dongle(colls, open, states, now, cable_answered));
    out
}

static STATES: Mutex<Option<HashMap<u16, State>>> = Mutex::new(None);

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    let mut guard = STATES.lock().unwrap_or_else(|e| e.into_inner());
    let states = guard.get_or_insert_with(HashMap::new);
    let now = Instant::now();
    poll_with(
        &colls(infos),
        &mut hid_opener(api, now + BUDGET),
        states,
        now,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::hidport::fake::{self, coll};

    fn raw(pid: u16, len: u16, product: &str) -> Coll {
        let mut c = coll(VID, pid, 0xFF60, 0x61, len);
        c.iface = 1;
        c.product = product.into();
        c
    }

    /// A PMO keyboard: answers START, GET_BATTERY and STOP; `paired` goes into the START reply.
    fn pmo(level: u8, paired: Option<u16>) -> fake::Shared {
        fake::responder(move |w| {
            if w.get(1) != Some(&0xAA) {
                return vec![]; // the PMO firmware ignores VIA frames
            }
            let mut r = vec![0u8; w.len() - 1];
            r[..4].copy_from_slice(&[0xAA, w[2], 0, 0]);
            r[7] = 0x55;
            match w[2] {
                0x10 => {
                    if let Some(p) = paired {
                        r[9..13].copy_from_slice(&[0xB0, 0x36, p as u8, (p >> 8) as u8]);
                    }
                }
                0x30 => r[8] = level,
                _ => {}
            }
            vec![r]
        })
    }

    /// A VIA keyboard: answers custom value 7 with `level`, FF to anything else.
    fn via(level: u8) -> fake::Shared {
        fake::responder(move |w| {
            let mut r = vec![0u8; 32];
            if w[1..4] == [0x08, 0x00, 0x07] {
                r[..4].copy_from_slice(&[0x08, 0x00, 0x07, level]);
            } else {
                r[0] = 0xFF;
            }
            vec![r]
        })
    }

    #[test]
    fn pmo_frames() {
        let f = pmo_frame(32, 0x10, 0x18);
        assert_eq!((f.len(), &f[..6]), (33, &[0, 0xAA, 0x10, 0, 0, 0x18][..]));
    }

    #[test]
    fn pmo_keyboard_on_the_dongle_names_the_paired_board() {
        let s = pmo(78, Some(0x3009));
        let mut states = HashMap::new();
        let out = poll_with(
            &[raw(DONGLE, 33, "Wireless 2.4G Dongle")],
            &mut fake::opener(&s),
            &mut states,
            Instant::now(),
        );
        let d = &out[0];
        assert_eq!(
            (d.key.as_str(), d.name.as_str(), d.percent),
            ("via:36B0:3009", "WAVE 75", Some(78))
        );
        let w = &s.borrow().writes;
        assert_eq!(
            w.iter().map(|w| w[2]).collect::<Vec<_>>(),
            [0x10, 0x30, 0x11]
        );
        assert_eq!(states[&DONGLE].proto, Proto::Pmo);
    }

    #[test]
    fn pmo_keyboard_on_its_cable_hides_the_dongle() {
        let s = pmo(64, None);
        let mut states = HashMap::new();
        let colls = [
            raw(0x3009, 65, "WAVE 75"),
            raw(DONGLE, 33, "Wireless 2.4G Dongle"),
        ];
        let out = poll_with(&colls, &mut fake::opener(&s), &mut states, Instant::now());
        assert_eq!(out.len(), 1);
        assert_eq!(
            (out[0].key.as_str(), out[0].via.as_str()),
            ("via:36B0:3009", CABLE)
        );
        // START length on the cable is 0x38 (64-byte payload).
        assert_eq!(s.borrow().writes[0][5], 0x38);
        assert_eq!(s.borrow().opened.len(), 1);
    }

    #[test]
    fn urx_dongle_is_asked_via_first() {
        let s = via(55);
        let mut states = HashMap::new();
        let out = poll_with(
            &[raw(DONGLE, 33, "URX Core75 Wireless")],
            &mut fake::opener(&s),
            &mut states,
            Instant::now(),
        );
        assert_eq!(
            (out[0].name.as_str(), out[0].percent),
            ("URX Core75", Some(55))
        );
        assert_eq!(out[0].key, "via:36B0:3002");
        assert_eq!(s.borrow().writes[0][1..4], [0x08, 0x00, 0x07]);
        assert_eq!(states[&DONGLE].proto, Proto::Via);
    }

    #[test]
    fn silent_dongle_backs_off() {
        let s = fake::script();
        let mut states = HashMap::new();
        let now = Instant::now();
        let colls = [raw(DONGLE, 33, "Wireless 2.4G Dongle")];
        assert!(poll_with(&colls, &mut fake::opener(&s), &mut states, now).is_empty());
        let sent = s.borrow().writes.len();
        assert!(
            poll_with(
                &colls,
                &mut fake::opener(&s),
                &mut states,
                now + Duration::from_secs(10)
            )
            .is_empty()
        );
        assert_eq!(s.borrow().writes.len(), sent, "inside the 30 s back-off");
        poll_with(
            &colls,
            &mut fake::opener(&s),
            &mut states,
            now + Duration::from_secs(31),
        );
        assert!(s.borrow().writes.len() > sent);
    }

    #[test]
    fn names() {
        assert_eq!(via_name("EVO80 2.4G"), "EVO80");
        assert_eq!(via_name("URX Core75 Wireless"), "URX Core75");
        assert_eq!(via_name("Wireless 2.4G Dongle"), "Wireless keyboard");
    }
}

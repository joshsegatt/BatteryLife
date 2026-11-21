//! MCHOSE keyboards on the common 2.4 GHz receiver 3837:3033 ("MCHOSE <model> 2.4G": G75 V2, G87 V2,
//! K99 V3; the K87S speaks another protocol and never answers, so it stays hidden).
//!
//! "GLW" frames on the id-less 64-byte collection 0001:0000 (FF31:0074 answers the same frames):
//!   request [55][cmd][00][checksum][size][offset LE][00], checksum = (size + offset + payload) & 0xFF
//!   reply   same header with flag AA, payload from byte 8
//! Battery: func config read (command 05, offset 0, size 56): payload[32] bit 7 charging, bits 0..6
//! percent. The receiver is asked at most every 10 minutes, so the keyboard is not kept awake.
//!
//! Ported from BatteryHub (src/drivers/mchose_keyboard.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0, from MCHOSE's M HUB web driver.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::hid::{self, Coll, Dev, Hid, RECEIVER, RECEIVER_GRACE};

pub const VID: u16 = 0x3837;
const PID: u16 = 0x3033;
pub const PIDS: &[u16] = &[PID];
const ASK_INTERVAL: Duration = Duration::from_secs(10 * 60);
/// Read command, 56 bytes at offset 0: [report id 0] flag, command, 00, checksum, size, offset.
const REQUEST: [u8; 9] = [0x00, 0x55, 0x05, 0x00, 56, 56, 0x00, 0x00, 0x00];

fn display_name(product: &str) -> String {
    let name = match product.strip_suffix(" 2.4G") {
        Some(n) if !n.is_empty() => n,
        _ => product,
    };
    if name.is_empty() {
        "MCHOSE keyboard".into()
    } else {
        name.into()
    }
}

/// (percent, charging) from a func-config reply.
pub fn parse_func_config(r: &[u8]) -> Option<(u8, bool)> {
    const PAYLOAD: usize = 8;
    const BATTERY: usize = 32;
    if r.len() < PAYLOAD + BATTERY + 1 || r[0] != 0xAA || r[1] != 0x05 || r[5] != 0 || r[6] != 0 {
        return None;
    }
    let size = usize::from(r[4]);
    if size < BATTERY + 1 || PAYLOAD + size > r.len() {
        return None;
    }
    let sum = r[4..8]
        .iter()
        .chain(&r[PAYLOAD..PAYLOAD + size])
        .fold(0u8, |a, b| a.wrapping_add(*b));
    if sum != r[3] {
        return None;
    }
    let b = r[PAYLOAD + BATTERY];
    (b & 0x7F <= 100).then_some((b & 0x7F, b & 0x80 != 0))
}

fn query(port: &mut dyn Dev) -> Option<(u8, bool)> {
    port.flush();
    for attempt in 0..2 {
        if attempt > 0 {
            port.sleep(Duration::from_millis(150));
        }
        if !port.write(&REQUEST) {
            continue;
        }
        let end = Instant::now() + Duration::from_millis(500);
        loop {
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            let Some(r) = port.read(left) else { break };
            if let Some(b) = parse_func_config(&r) {
                return Some(b);
            }
        }
    }
    None
}

/// Last reading, kept across polls.
#[derive(Debug, Default)]
pub struct State {
    last_ask: Option<Instant>,
    last: Option<(u8, bool)>,
}

pub fn poll_with(colls: &[Coll], hid: &mut dyn Hid, st: &mut State, now: Instant) -> Vec<Device> {
    let cs: Vec<&Coll> = colls
        .iter()
        .filter(|c| {
            c.is(VID, PID)
                && c.in_len == 65
                && c.out_len == 65
                && ((c.page == 0x0001 && c.usage == 0x0000)
                    || (c.page == 0xFF31 && c.usage == 0x0074))
        })
        .collect();
    let Some(first) = cs.first() else {
        return Vec::new(); // receiver not present
    };
    let ask = st
        .last_ask
        .is_none_or(|t| now.duration_since(t) >= ASK_INTERVAL);
    let reading = if ask {
        st.last_ask = Some(now);
        cs.iter()
            .find_map(|c| hid.open(c).and_then(|mut p| query(p.as_mut())))
    } else {
        st.last // between asks: keep showing the previous reading
    };
    if reading.is_none() && st.last.is_none() {
        return Vec::new(); // never answered: a K87S receiver or a dead link
    }
    if reading.is_some() {
        st.last = reading;
    }
    let mut d = Device::new(
        "mchose:3837:3033",
        display_name(&first.product),
        Kind::Keyboard,
        RECEIVER,
    );
    d.grace = RECEIVER_GRACE;
    match reading {
        Some((p, ch)) => (d.percent, d.charging) = (Some(p), ch),
        None => d.asleep = true, // asked but no reply: keep the last value
    }
    vec![d]
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    let mut guard = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let st = guard.get_or_insert_with(State::default);
    let now = Instant::now();
    hid::run(api, infos, |hid, colls| poll_with(colls, hid, st, now))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::hid::fake::{coll, per_write, script};

    /// A flush drops queued replies, as on the device.
    /// A func-config reply with a correct checksum and `battery` at payload byte 32.
    fn reply(battery: u8) -> Vec<u8> {
        let mut r = vec![0u8; 64];
        r[0] = 0xAA;
        r[1] = 0x05;
        r[4] = 56;
        r[8 + 32] = battery;
        r[3] = r[4..8]
            .iter()
            .chain(&r[8..8 + 56])
            .fold(0u8, |a, b| a.wrapping_add(*b));
        r
    }

    fn receiver() -> Coll {
        let mut c = coll(VID, PID, 0x0001, 0x0000, 65);
        c.product = "MCHOSE G75 V2 2.4G".into();
        c
    }

    #[test]
    fn parses_func_config() {
        assert_eq!(parse_func_config(&reply(85)), Some((85, false)));
        assert_eq!(parse_func_config(&reply(0x80 | 60)), Some((60, true)));
        let mut bad = reply(85);
        bad[3] ^= 1;
        assert_eq!(parse_func_config(&bad), None);
        assert_eq!(parse_func_config(&reply(101)), None);
    }

    #[test]
    fn asks_every_ten_minutes_and_keeps_the_reading() {
        let mut st = State::default();
        let now = Instant::now();
        let s = per_write(vec![vec![reply(85)]]);
        let out = poll_with(&[receiver()], &mut s.clone(), &mut st, now);
        assert_eq!(s.s().writes(), vec![REQUEST.to_vec()]);
        assert_eq!(
            (out[0].name.as_str(), out[0].percent),
            ("MCHOSE G75 V2", Some(85))
        );
        assert_eq!(out[0].key, "mchose:3837:3033");

        let s = script();
        let out = poll_with(
            &[receiver()],
            &mut s.clone(),
            &mut st,
            now + Duration::from_secs(60),
        );
        assert!(s.s().writes().is_empty());
        assert_eq!(out[0].percent, Some(85));

        // Asked again later without an answer: asleep, the tracker keeps the last level.
        let out = poll_with(&[receiver()], &mut s.clone(), &mut st, now + ASK_INTERVAL);
        assert_eq!(s.s().writes().len(), 2);
        assert!(out[0].asleep && out[0].percent.is_none());
    }

    #[test]
    fn never_answering_receiver_stays_hidden() {
        let mut st = State::default();
        let s = script();
        assert!(poll_with(&[receiver()], &mut s.clone(), &mut st, Instant::now()).is_empty());
        let mut other = receiver();
        other.page = 0xFF00;
        let s = script();
        assert!(
            poll_with(
                &[other],
                &mut s.clone(),
                &mut State::default(),
                Instant::now()
            )
            .is_empty()
        );
        assert!(s.s().opened.is_empty());
    }
}

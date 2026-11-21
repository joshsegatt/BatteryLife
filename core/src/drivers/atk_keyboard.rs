//! Evision keyboards on the shared 2.4 GHz receiver (320F:5088) or USB cable (320F:5055): ATK V75X and
//! the VGN / VXE keyboards VGN HUB drives the same way, and boards sold under the generic receiver name
//! "Wireless Gaming Keyboard" (Feker Alice80 and others). Also the ATK Z87 receiver (373B:1012).
//!
//! Output report 04 on FF1C:0092: [04][seq lo][seq hi][1A][00, or 06 for VGN][00 ...];
//! input report 04: [04][seq lo][seq hi][1A][..][00 00][..][percent][0 on battery, 1/2 charging].
//! These ids are shared by many Evision keyboards, so the product name is checked as well. The
//! receiver is asked at most every 10 minutes (every poll on the cable, which powers the keyboard), so
//! a sleeping keyboard is not kept awake. Read-only.
//!
//! Ported from BatteryHub (src/drivers/atk_keyboard.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0, which credits
//! HeyOkay/HaloBattery (providers/atk_keyboard.py, MIT), ATK HUB and VGN HUB.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::hid::{self, CABLE, Coll, Dev, Hid, RECEIVER, RECEIVER_GRACE};
use crate::registry::Match;

const VID: u16 = 0x320F;
const RECEIVER_PID: u16 = 0x5088;
const CABLE_PID: u16 = 0x5055;
const Z87_VID: u16 = 0x373B;
const Z87_RECEIVER: u16 = 0x1012;
const RECEIVER_INTERVAL: Duration = Duration::from_secs(10 * 60);

pub const MATCHES: &[Match] = &[
    Match {
        vid: VID,
        pids: &[RECEIVER_PID, CABLE_PID],
    },
    Match {
        vid: Z87_VID,
        pids: &[Z87_RECEIVER],
    },
];

/// Product string of the receiver or cable, and whether VGN HUB's request variant is used.
const MODELS: &[(&str, bool)] = &[
    ("ATK V75X 2.4G Dongle", false),
    ("ATK V75X", false),
    ("VGN S99 2.4G Dongle", true),
    ("VGN S99", true),
    ("VGN V98pro V2 2.4G Dongle", true),
    ("VGN V98pro V2", true),
    ("VGN V98 PRO 2.4G Dongle", true),
    ("VGN V98pro", true),
    ("VGN N75 PRO 2.4G Dongle", true),
    ("VGN N75 PRO", true),
    ("VGN VXE75 V2 2.4G Dongle", true),
    ("VGN VXE75 V2", true),
    ("VGN VXE75 2.4G Dongle", true),
    ("VGN VXE75", true),
    ("VXE V87 2.4G Dongle", true),
    ("VXE V87", true),
    ("VXE V87 RGB", true),
    ("Wireless Gaming Keyboard", false), // generic receiver name (Feker Alice80 and others)
];

fn find_model(product: &str) -> Option<(&'static str, bool)> {
    let name = product.trim_end_matches(' ');
    MODELS
        .iter()
        .copied()
        .find(|(p, _)| p.eq_ignore_ascii_case(name))
}

fn keyboard_name(product: &str) -> String {
    const SUFFIX: &str = " 2.4G Dongle";
    let n = product.len();
    if n > SUFFIX.len()
        && product.is_char_boundary(n - SUFFIX.len())
        && product[n - SUFFIX.len()..].eq_ignore_ascii_case(SUFFIX)
    {
        product[..n - SUFFIX.len()].into()
    } else {
        product.into()
    }
}

/// The Evision battery request: [04][seq LE][1A][variant] padded to 32 bytes.
pub fn evision_request(seq: u16, variant: u8) -> Vec<u8> {
    let mut req = vec![0u8; 32];
    req[0] = 0x04;
    req[1..3].copy_from_slice(&seq.to_le_bytes());
    req[3] = 0x1A;
    req[4] = variant;
    req
}

#[derive(Debug, PartialEq)]
pub enum Reply {
    /// Not the reply to this request: keep waiting.
    Other,
    /// The reply, without a usable level.
    Bad,
    /// (percent, charging)
    Level(u8, bool),
}

/// Classifies a report against the request with sequence `seq`.
pub fn evision_reply(r: &[u8], seq: u16) -> Reply {
    let [lo, hi] = seq.to_le_bytes();
    if r.len() < 10
        || r[0] != 0x04
        || r[1] != lo
        || r[2] != hi
        || r[3] != 0x1A
        || r[5] != 0
        || r[6] != 0
    {
        return Reply::Other;
    }
    if r[8] > 100 || r[9] > 2 {
        return Reply::Bad;
    }
    Reply::Level(r[8], r[9] == 1 || r[9] == 2)
}

/// Next request sequence, 1..=0xFFFF.
pub fn next_seq(seq: &mut u16) -> u16 {
    *seq = *seq % 0xFFFF + 1;
    *seq
}

/// Sends one Evision request and waits up to `timeout` for its reply.
pub fn evision_read(
    port: &mut dyn Dev,
    seq: u16,
    variant: u8,
    timeout: Duration,
) -> Option<(u8, bool)> {
    port.flush();
    if !port.write(&evision_request(seq, variant)) {
        return None;
    }
    let end = Instant::now() + timeout;
    loop {
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return None;
        }
        match evision_reply(&port.read(left)?, seq) {
            Reply::Other => continue,
            Reply::Bad => return None,
            Reply::Level(p, c) => return Some((p, c)),
        }
    }
}

/// Z87: an FF in place of the command byte is the receiver answering for a sleeping keyboard.
fn read_z87(port: &mut dyn Dev, seq: u16) -> Option<(u8, bool)> {
    port.flush();
    if !port.write(&evision_request(seq, 0)) {
        return None;
    }
    let [lo, hi] = seq.to_le_bytes();
    let end = Instant::now() + Duration::from_secs(1);
    loop {
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return None;
        }
        let r = port.read(left)?;
        if r.len() < 4 || r[0] != 0x04 || r[1] != lo || r[2] != hi {
            continue;
        }
        if r[3] == 0xFF {
            return None; // sleeping keyboard
        }
        match evision_reply(&r, seq) {
            Reply::Other => continue,
            Reply::Bad => return None,
            Reply::Level(p, c) => return Some((p, c)),
        }
    }
}

/// State kept across polls.
#[derive(Debug, Default)]
pub struct State {
    seq: u16,
    last_ask: Option<Instant>,
    last: Option<(u8, bool)>,
    z87_seq: u16,
    z87_next_ask: Option<Instant>,
    z87: Option<(u8, bool)>,
}

fn poll_z87(colls: &[Coll], hid: &mut dyn Hid, st: &mut State, now: Instant) -> Option<Device> {
    let Some(c) = colls.iter().find(|c| {
        c.is(Z87_VID, Z87_RECEIVER)
            && c.page == 0xFF1C
            && c.usage == 0x0092
            && c.in_len == 64
            && c.out_len == 64
            && c.product
                .trim_end_matches(' ')
                .eq_ignore_ascii_case("ATK Z87 Dongle")
    }) else {
        // Unplugged: a reconnected receiver does not inherit an old reading.
        (st.z87_seq, st.z87_next_ask, st.z87) = (0, None, None);
        return None;
    };
    if st.z87_next_ask.is_none_or(|t| now >= t) {
        st.z87_next_ask = Some(now + RECEIVER_INTERVAL);
        let seq = next_seq(&mut st.z87_seq);
        st.z87 = hid.open(c).and_then(|mut p| read_z87(p.as_mut(), seq));
        if st.z87.is_none() {
            st.z87_next_ask = Some(now + Duration::from_secs(30));
        }
    }
    let mut d = Device::new("atk_keyboard:1012", "ATK Z87", Kind::Keyboard, RECEIVER);
    d.grace = RECEIVER_GRACE;
    match st.z87 {
        Some((p, c)) => (d.percent, d.charging) = (Some(p), c),
        None => d.asleep = true,
    }
    Some(d)
}

pub fn poll_with(colls: &[Coll], hid: &mut dyn Hid, st: &mut State, now: Instant) -> Vec<Device> {
    let mut out: Vec<Device> = poll_z87(colls, hid, st, now).into_iter().collect();
    for pid in [CABLE_PID, RECEIVER_PID] {
        // The cable wins when both are connected.
        let cable = pid == CABLE_PID;
        for c in colls
            .iter()
            .filter(|c| c.is(VID, pid) && c.page == 0xFF1C && c.usage == 0x0092)
        {
            let Some((product, vgn)) = find_model(&c.product) else {
                continue;
            };
            let via = if cable { CABLE } else { RECEIVER };
            let mut d = Device::new(
                "atk_keyboard:5088",
                keyboard_name(product),
                Kind::Keyboard,
                via,
            );
            d.grace = RECEIVER_GRACE;
            let ask = cable
                || st
                    .last_ask
                    .is_none_or(|t| now.duration_since(t) >= RECEIVER_INTERVAL);
            let reading = if ask {
                st.last_ask = Some(now);
                let seq = next_seq(&mut st.seq);
                let variant = if vgn { 0x06 } else { 0x00 };
                // The reply arrives up to ~1 s after the request on a VGN receiver.
                let r = hid.open(c).and_then(|mut p| {
                    evision_read(p.as_mut(), seq, variant, Duration::from_millis(2500))
                });
                if r.is_some() {
                    st.last = r;
                }
                r // no reply: asleep, off or busy; the tracker keeps the last value
            } else {
                st.last
            };
            match reading {
                Some((p, ch)) => (d.percent, d.charging) = (Some(p), ch),
                None => d.asleep = true,
            }
            out.push(d);
            return out;
        }
    }
    out
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
    use crate::drivers::hid::fake::{Fake, at, coll, path_responder, script};

    /// After each accepted send, queues `respond(path, data)`; a flush drops queued replies.
    /// A flush drops queued replies, as on the device.
    fn z87() -> Coll {
        let mut c = at(
            coll(Z87_VID, Z87_RECEIVER, 0xFF1C, 0x0092, 64),
            "z87-receiver",
        );
        c.iface = 1;
        c.product = "ATK Z87 Dongle".into();
        c
    }

    fn cable(product: &str) -> Coll {
        let mut c = at(coll(VID, CABLE_PID, 0xFF1C, 0x0092, 64), "v75x-cable");
        c.product = product.into();
        c
    }

    /// The test device of BatteryHub's atk_keyboard test: Evision boards answer 37 % charging; the Z87
    /// answers a truncated report, a stale reply, then `level` / `charge` (or FF when asleep).
    fn device(level: u8, charge: u8, asleep: bool) -> Fake {
        path_responder(move |path, w| {
            assert!(w.len() >= 4 && w[0] == 4 && w[3] == 0x1A);
            assert!(w[5..].iter().all(|b| *b == 0));
            let mut report = w.to_vec();
            report.resize(64, 0);
            if path != "z87-receiver" {
                report[8] = 37;
                report[9] = 1;
                return vec![report];
            }
            if asleep {
                report[3] = 0xFF;
                return vec![report];
            }
            let mut stale = report.clone();
            stale[1] = stale[1].wrapping_sub(1);
            stale[8] = 13;
            report[8] = level;
            report[9] = charge;
            vec![report[..4].to_vec(), stale, report]
        })
    }

    #[test]
    fn evision_reply_parsing() {
        let mut r = evision_request(0x011C, 0x02);
        r[8] = 0x61;
        assert_eq!(evision_reply(&r, 0x011C), Reply::Level(97, false));
        assert_eq!(evision_reply(&r, 0x011D), Reply::Other);
        r[9] = 3;
        assert_eq!(evision_reply(&r, 0x011C), Reply::Bad);
    }

    #[test]
    fn strict_matching_opens_nothing_else() {
        let mut mouse = z87();
        mouse.pid = 0x1216;
        let mut wrong_vendor = z87();
        wrong_vendor.vid = VID;
        let mut other_page = z87();
        other_page.page = 0xFFEF;
        let mut short = z87();
        short.out_len = 32;
        let mut other_name = z87();
        other_name.product = "ATK Keyboard".into();
        let s = device(92, 0, false);
        let mut st = State::default();
        let out = poll_with(
            &[mouse, wrong_vendor, other_page, short, other_name],
            &mut s.clone(),
            &mut st,
            Instant::now(),
        );
        assert!(out.is_empty() && s.s().opened.is_empty());
    }

    #[test]
    fn z87_and_v75x_cable_side_by_side() {
        let s = device(92, 0, false);
        let mut st = State::default();
        let now = Instant::now();
        let out = poll_with(&[cable("ATK V75X"), z87()], &mut s.clone(), &mut st, now);
        assert_eq!(out.len(), 2);
        assert_eq!(s.s().opened.len(), 2);
        let z = out.iter().find(|d| d.key == "atk_keyboard:1012").unwrap();
        assert_eq!(
            (z.name.as_str(), z.percent, z.charging),
            ("ATK Z87", Some(92), false)
        );
        assert_eq!(z.via, RECEIVER);
        let k = out.iter().find(|d| d.key == "atk_keyboard:5088").unwrap();
        assert_eq!(
            (k.percent, k.charging, k.via.as_str()),
            (Some(37), true, CABLE)
        );
        assert_eq!(k.name, "ATK V75X");

        // Within ten minutes the Z87 is not asked again and keeps its cached level.
        let s = device(92, 0, false);
        let out = poll_with(
            &[z87()],
            &mut s.clone(),
            &mut st,
            now + Duration::from_secs(60),
        );
        assert!(s.s().writes().is_empty());
        assert_eq!(out[0].percent, Some(92));
    }

    #[test]
    fn z87_unplug_sleep_and_bad_levels() {
        let mut st = State::default();
        let now = Instant::now();
        poll_with(&[], &mut script(), &mut st, now);
        let s = device(0, 0, true);
        let out = poll_with(&[z87()], &mut s.clone(), &mut st, now);
        assert_eq!(s.s().writes().len(), 1);
        assert!(out[0].asleep && out[0].percent.is_none());
        let s = device(0, 0, true);
        let out = poll_with(
            &[z87()],
            &mut s.clone(),
            &mut st,
            now + Duration::from_secs(1),
        );
        assert!(s.s().writes().is_empty() && out[0].asleep);

        for (level, state) in [(101u8, 0u8), (92, 3), (0, 2)] {
            poll_with(&[], &mut script(), &mut st, now);
            let s = device(level, state, false);
            let d = &poll_with(&[z87()], &mut s.clone(), &mut st, now)[0];
            assert_eq!(s.s().writes().len(), 1);
            if level == 0 {
                assert!(!d.asleep && d.percent == Some(0) && d.charging);
            } else {
                assert!(d.asleep && d.percent.is_none() && !d.charging);
            }
        }
    }

    #[test]
    fn vgn_and_generic_names() {
        for (product, variant) in [("VGN S99", 0x06), ("Wireless Gaming Keyboard", 0x00)] {
            let s = device(0, 0, false);
            let mut st = State::default();
            let out = poll_with(&[cable(product)], &mut s.clone(), &mut st, Instant::now());
            assert_eq!((out[0].name.as_str(), out[0].percent), (product, Some(37)));
            assert_eq!(s.s().writes()[0][4], variant);
        }
        assert_eq!(keyboard_name("VGN S99 2.4G Dongle"), "VGN S99");
    }

    #[test]
    fn receiver_is_asked_at_most_every_ten_minutes() {
        let mut rx = at(coll(VID, RECEIVER_PID, 0xFF1C, 0x0092, 32), "rx");
        rx.product = "ATK V75X 2.4G Dongle".into();
        let mut st = State::default();
        let now = Instant::now();
        let s = device(0, 0, false);
        let d = &poll_with(&[rx.clone()], &mut s.clone(), &mut st, now)[0];
        assert_eq!((d.percent, d.via.as_str()), (Some(37), RECEIVER));
        let s = device(0, 0, false);
        let d = &poll_with(
            &[rx],
            &mut s.clone(),
            &mut st,
            now + Duration::from_secs(60),
        )[0];
        assert!(s.s().writes().is_empty());
        assert_eq!(d.percent, Some(37));
    }
}

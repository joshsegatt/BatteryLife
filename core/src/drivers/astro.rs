//! Logitech / ASTRO wireless headsets.
//! 1. ASTRO A50 X (046D:0B0B) and A50 Gen 5 (046D:0B1C): vendor collection FF32:0074, 64-byte reports,
//!    request [02][0C][03][00][06][0C], reply [02][0C][len][00][06][handle][percent][..][charging].
//! 2. ASTRO A50 Gen 4 / Gen 3 and A20 Wireless (VID 9886) base stations: request [02][cmd], reply
//!    [02][status][len][payload] (status 02 = OK, 01 = error). 0x54 = headset status (bit 0 docked,
//!    bit 1 linked), 0x7C = battery (bit 7 charging, bits 0..6 %). A50 Gen 3 rejects 0x7C and keeps
//!    the battery in the 0x72 "current UI" read (field 04). Some Gen 3 firmware declares no report id,
//!    so the frame is retried behind a 00 byte and the working framing is remembered per product.
//!    While Astro Command Center holds the base exclusively, the last reading is shown.
//!
//! Ported from BatteryHub (src/drivers/astro.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0, which credits
//! Sapd/HeadsetControl, tdryer/eh-fifty, nichtlegacy/AstroBar and BenStein1/OpenAstroCC.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::hid::{self, Coll, Dev, Hid, Outcome, find, stable_key};

pub const VID_LOGITECH: u16 = 0x046D;
pub const LOGITECH_PIDS: &[u16] = &[0x0B0B, 0x0B1C];
pub const VID_ASTRO: u16 = 0x9886;

const LEGACY_MODELS: &[(u16, &str)] = &[
    // A50 Gen 4 (Mimas)
    (0x002C, "ASTRO A50 Gen 4"),
    (0x002D, "ASTRO A50 Gen 4"),
    (0x002A, "ASTRO A50 Gen 4"),
    // A50 Gen 3 (Europa)
    (0x0015, "ASTRO A50 Gen 3"),
    (0x0014, "ASTRO A50 Gen 3"),
    (0x0018, "ASTRO A50 Gen 3"),
    (0x001D, "ASTRO A50 Gen 3"),
    (0x0026, "ASTRO A50 Gen 3"),
    // A20 Wireless (Aquarius)
    (0x0037, "ASTRO A20 Wireless"),
    (0x0038, "ASTRO A20 Wireless"),
    (0x0039, "ASTRO A20 Wireless"),
    (0x0043, "ASTRO A20 Wireless"),
    (0x0044, "ASTRO A20 Wireless"),
    (0x0045, "ASTRO A20 Wireless"),
];

/// Product ids of the VID 9886 base stations.
pub const ASTRO_PIDS: &[u16] = &[
    0x002C, 0x002D, 0x002A, 0x0015, 0x0014, 0x0018, 0x001D, 0x0026, 0x0037, 0x0038, 0x0039, 0x0043,
    0x0044, 0x0045,
];

#[derive(Clone, Copy, PartialEq, Debug)]
enum Framing {
    ReportId02,
    NoReportId,
}

#[derive(PartialEq, Debug)]
enum QResult {
    Ok(Vec<u8>),
    Error,
    NoReply,
    WriteFailed,
}

/// Kept across polls: last reading (percent, charging) and the framing that works, per product.
#[derive(Default)]
pub struct State {
    cache: Vec<(u16, u8, bool)>,
    framing: Vec<(u16, Framing)>,
}

impl State {
    fn framing(&self, pid: u16) -> Option<Framing> {
        self.framing.iter().find(|(p, _)| *p == pid).map(|f| f.1)
    }
    fn set_framing(&mut self, pid: u16, f: Framing) {
        self.framing.retain(|(p, _)| *p != pid);
        self.framing.push((pid, f));
    }
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn legacy_frame(f: Framing, cmd: u8, payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::new();
    if f == Framing::NoReportId {
        v.push(0x00);
    }
    v.extend_from_slice(&[0x02, cmd]);
    if !payload.is_empty() {
        v.push(payload.len() as u8);
        v.extend_from_slice(payload);
    }
    v
}

fn query_once(io: &mut dyn Dev, f: Framing, cmd: u8, payload: &[u8]) -> QResult {
    io.flush();
    if !io.write(&legacy_frame(f, cmd, payload)) {
        return QResult::WriteFailed;
    }
    let end = Instant::now() + hid::MAX_WAIT;
    loop {
        let left = end.saturating_duration_since(Instant::now()).as_millis() as u64;
        if left == 0 {
            return QResult::NoReply;
        }
        let Some(r) = io
            .read(Duration::from_millis(left))
            .filter(|r| r.len() >= 4)
        else {
            return QResult::NoReply;
        };
        match (r[0], r[1]) {
            // Understood, but refused (unknown command / not linked).
            (0x02, 0x01) => return QResult::Error,
            (0x02, 0x02) => return QResult::Ok(r),
            _ => {}
        }
    }
}

fn query_legacy(io: &mut dyn Dev, st: &mut State, pid: u16, cmd: u8, payload: &[u8]) -> QResult {
    let known = st.framing(pid);
    let order = if known == Some(Framing::NoReportId) {
        [Framing::NoReportId, Framing::ReportId02]
    } else {
        [Framing::ReportId02, Framing::NoReportId]
    };
    let mut last = QResult::NoReply;
    for f in order {
        last = query_once(io, f, cmd, payload);
        if matches!(last, QResult::Ok(_) | QResult::Error) {
            st.set_framing(pid, f);
            return last;
        }
        if known.is_some() {
            break; // framing already proven: a miss is a real miss
        }
    }
    last
}

/// A50 Gen 3 battery from the 0x72 "current UI" reply: data[12..15] = [02][04][battery][00].
pub fn gen3_battery(r: &[u8]) -> Option<u8> {
    if (r[2] & 0x3F) < 16 || r.len() < 3 + 16 {
        return None;
    }
    let data = &r[3..];
    (data[12] == 0x02 && data[13] == 0x04 && (data[14] & 0x7F) <= 100).then_some(data[14])
}

fn poll_legacy(c: &Coll, name: &str, io: Option<&mut dyn Dev>, st: &mut State) -> Outcome {
    let mut d = Device::new(
        format!("astro:{:04X}", c.pid),
        name,
        Kind::Headset,
        "USB base station",
    );
    d.grace = Duration::from_secs(1800);
    let Some(io) = io else {
        // Exclusive lock held by Astro Command Center: keep the headset with its cached reading.
        if let Some(&(_, pct, charging)) = st.cache.iter().find(|(p, _, _)| *p == c.pid) {
            d.percent = Some(pct);
            d.charging = charging;
        }
        return Outcome::Found(d);
    };
    // 0x54 (dock / link status) is optional: some Gen 3 firmware may not answer it.
    let status = match query_legacy(io, st, c.pid, 0x54, &[]) {
        QResult::Ok(r) => Some(r[3]),
        _ => None,
    };
    let docked = status.is_some_and(|s| s & 0x01 != 0);
    if let Some(s) = status
        && !docked
        && s & 0x02 == 0
    {
        return Outcome::Off(d);
    }
    let mut battery = query_legacy(io, st, c.pid, 0x7C, &[]);
    if battery == QResult::Error
        && status.is_some()
        && let QResult::Ok(r) = query_legacy(io, st, c.pid, 0x72, &[0x01])
        && let Some(b) = gen3_battery(&r)
    {
        // A50 Gen 3: reuse the 0x7C layout (value in byte 3).
        battery = QResult::Ok(vec![0x02, 0x02, 0x01, b]);
    }
    match battery {
        QResult::Ok(r) if (r[3] & 0x7F) <= 100 => {
            let pct = r[3] & 0x7F;
            d.percent = Some(pct);
            d.charging = r[3] & 0x80 != 0 || (docked && pct < 100);
            st.cache.retain(|(p, _, _)| *p != c.pid);
            st.cache.push((c.pid, pct, d.charging));
            Outcome::Found(d)
        }
        // Base answered without a status: headset off or out of range.
        QResult::Error if status.is_none() => Outcome::Off(d),
        _ if status.is_some() => Outcome::Found(d),
        _ => Outcome::NoAnswer,
    }
}

fn is_legacy_vendor(c: &Coll) -> bool {
    matches!(c.page, 0xFF32 | 0xFF31 | 0xFF00) && c.in_len >= 64 && c.out_len >= 64
}

/// A50 X / Gen 5 battery reply: Some((percent, charging)); unsolicited events and partial frames
/// give None.
pub fn a50x_reply(r: &[u8]) -> Option<(u8, bool)> {
    let ok = r.len() >= 9
        && r[0] == 0x02
        && r[1] == 0x0C
        && r[2] >= 6
        && r[2] as usize + 3 <= r.len()
        && r[3] == 0
        && r[4] == 0x06
        && (r[5] == 0 || r[5] == 0x0C)
        && r[6] <= 100;
    ok.then(|| (r[6], r[8] != 0))
}

fn poll_a50x(colls: &[Coll], hid: &mut dyn Hid, out: &mut Vec<Device>) {
    let mut done: Vec<u16> = Vec::new();
    for c in colls {
        if c.vid != VID_LOGITECH
            || !LOGITECH_PIDS.contains(&c.pid)
            || done.contains(&c.pid)
            || c.page != 0xFF32
            || c.usage != 0x0074
            || c.in_len != 64
            || c.out_len != 64
        {
            continue;
        }
        let Some(mut io) = hid.open(c) else { continue };
        io.flush();
        // No payload: read battery only; never change audio or headset settings.
        let mut req = [0u8; 64];
        req[..6].copy_from_slice(&[0x02, 0x0C, 0x03, 0x00, 0x06, 0x0C]);
        if !io.write(&req) {
            continue;
        }
        let end = Instant::now() + hid::MAX_WAIT;
        for _ in 0..8 {
            let left = end.saturating_duration_since(Instant::now()).as_millis() as u64;
            if left == 0 {
                break;
            }
            let Some(r) = io.read(Duration::from_millis(left)) else {
                break;
            };
            let Some((pct, charging)) = a50x_reply(&r) else {
                continue;
            };
            let name = if c.pid == 0x0B0B {
                "ASTRO A50 X"
            } else {
                "ASTRO A50 Gen 5"
            };
            let key = stable_key(out, &format!("astro:{:04X}", c.pid));
            let mut d = Device::new(key, name, Kind::Headset, "USB base station");
            d.percent = Some(pct);
            d.charging = charging;
            out.push(d);
            done.push(c.pid);
            break;
        }
    }
}

pub fn poll_colls(colls: &[Coll], hid: &mut dyn Hid, st: &mut State) -> Vec<Device> {
    let mut out = Vec::new();
    poll_a50x(colls, hid, &mut out);
    for &(pid, name) in LEGACY_MODELS {
        for c in find(colls, VID_ASTRO, pid, |c| {
            is_legacy_vendor(c) || (c.page == 0 && (c.iface == 6 || c.iface == -1))
        }) {
            let mut io = hid.open(c);
            let io = io.as_mut().map(|b| b.as_mut() as &mut dyn Dev);
            if poll_legacy(c, name, io, st).settle(&mut out) {
                break;
            }
        }
    }
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

    fn a50x(pid: u16) -> Coll {
        Coll::new(VID_LOGITECH, pid, 0xFF32, 0x74, 64, 64)
    }
    fn reply(level: u8, dock: u8) -> Vec<u8> {
        vec![2, 12, 6, 0, 6, 12, level, 100, dock]
    }
    fn run(colls: &[Coll], replies: Vec<Vec<u8>>, st: &mut State) -> (Vec<Device>, Fake, usize) {
        let f = Fake::new();
        f.queue(replies);
        let out = poll_colls(colls, &mut f.clone(), st);
        let n = f.s().opened.len();
        (out, f, n)
    }

    #[test]
    fn a50x_level_and_charging() {
        let (out, f, _) = run(&[a50x(0x0B0B)], vec![reply(99, 0)], &mut State::default());
        assert_eq!((out[0].percent, out[0].charging), (Some(99), false));
        assert_eq!(
            (out[0].name.as_str(), out[0].kind),
            ("ASTRO A50 X", Kind::Headset)
        );
        let mut expected = vec![0u8; 64];
        expected[..6].copy_from_slice(&[2, 12, 3, 0, 6, 12]);
        assert_eq!(f.s().writes(), vec![expected]);

        let (out, _, _) = run(&[a50x(0x0B0B)], vec![reply(0, 1)], &mut State::default());
        assert_eq!((out[0].percent, out[0].charging), (Some(0), true));
        let mut push = reply(100, 0);
        push[5] = 0;
        let (out, _, _) = run(&[a50x(0x0B0B)], vec![push], &mut State::default());
        assert_eq!(out[0].percent, Some(100));
        let mut unrelated = reply(50, 0);
        unrelated[4] = 8;
        let (out, _, _) = run(
            &[a50x(0x0B0B)],
            vec![unrelated, reply(60, 0)],
            &mut State::default(),
        );
        assert_eq!(out[0].percent, Some(60));
        let (out, _, _) = run(&[a50x(0x0B1C)], vec![reply(75, 0)], &mut State::default());
        assert_eq!(out[0].name, "ASTRO A50 Gen 5");
    }

    #[test]
    fn a50x_rejects_malformed_and_foreign_collections() {
        for field in 0..7 {
            let mut bad = reply(50, 0);
            bad[field] = 255;
            assert!(
                run(&[a50x(0x0B0B)], vec![bad], &mut State::default())
                    .0
                    .is_empty()
            );
        }
        let short = vec![2, 12, 6, 0, 6, 12, 50];
        assert!(
            run(&[a50x(0x0B0B)], vec![short], &mut State::default())
                .0
                .is_empty()
        );
        assert!(
            run(&[a50x(0x0B0B)], vec![], &mut State::default())
                .0
                .is_empty()
        );
        for field in 0..5 {
            let mut c = a50x(0x0B0B);
            match field {
                0 => c.vid = 0x1234,
                1 => c.pid = 0x1234,
                2 => c.page = 0xFF00,
                3 => c.usage = 1,
                _ => c.out_len = 20,
            }
            let (out, _, opens) = run(&[c], vec![reply(50, 0)], &mut State::default());
            assert!(out.is_empty() && opens == 0);
        }
    }

    fn legacy(pid: u16) -> Coll {
        let mut c = Coll::new(VID_ASTRO, pid, 0xFF32, 0x74, 64, 64);
        c.iface = 6;
        c
    }

    #[test]
    fn legacy_gen4_status_and_battery() {
        let (out, f, _) = run(
            &[legacy(0x0018)],
            vec![vec![2, 2, 1, 0x02], vec![2, 2, 1, 0x48]],
            &mut State::default(),
        );
        assert_eq!((out[0].percent, out[0].charging), (Some(72), false));
        assert_eq!(f.s().writes(), vec![vec![2, 0x54], vec![2, 0x7C]]);
    }

    #[test]
    fn legacy_framing_without_report_id_is_remembered() {
        let mut st = State::default();
        let run_no_id = |replies: Vec<Vec<u8>>, st: &mut State| {
            // Writes with report id 02 fail.
            let f = Fake::with(|s, c, op| match op {
                Op::Write(w) if w[0] == 0x02 => None,
                _ => queued(s, c, op),
            });
            f.queue(replies);
            let out = poll_colls(&[legacy(0x0015)], &mut f.clone(), st);
            (out, f)
        };
        let (out, f) = run_no_id(vec![vec![2, 2, 1, 0x03], vec![2, 2, 1, 0x80 | 90]], &mut st);
        assert_eq!((out[0].percent, out[0].charging), (Some(90), true));
        assert_eq!(
            f.s().writes(),
            vec![vec![2, 0x54], vec![0, 2, 0x54], vec![0, 2, 0x7C]]
        );
        let (out, f) = run_no_id(vec![vec![2, 2, 1, 0x02], vec![2, 2, 1, 40]], &mut st);
        assert_eq!(out[0].percent, Some(40));
        assert_eq!(f.s().writes().len(), 2);
        assert_eq!(f.s().writes()[0][0], 0);
    }

    #[test]
    fn legacy_status_variants() {
        // Ignores 0x54 but answers 0x7C.
        let (out, _, _) = run(
            &[legacy(0x0014)],
            vec![vec![], vec![], vec![2, 2, 1, 0x80 | 5]],
            &mut State::default(),
        );
        assert_eq!((out[0].percent, out[0].charging), (Some(5), true));
        // Headset off, or 0x7C refused without a status: listed as switched off, same key.
        let (out, _, _) = run(
            &[legacy(0x001D)],
            vec![vec![2, 2, 1, 0]],
            &mut State::default(),
        );
        assert_eq!(out.len(), 1);
        assert!(out[0].off && out[0].percent.is_none());
        assert_eq!(out[0].key, "astro:001D");
        let (out, _, _) = run(
            &[legacy(0x0026)],
            vec![vec![], vec![], vec![2, 1, 0, 0]],
            &mut State::default(),
        );
        assert_eq!((out.len(), out[0].off), (1, true));
        assert_eq!(out[0].key, "astro:0026");
        // Nothing answers: not listed.
        assert!(
            run(&[legacy(0x002C)], vec![], &mut State::default())
                .0
                .is_empty()
        );
    }

    #[test]
    fn legacy_gen3_battery_from_ui_read() {
        let ui = vec![
            2,
            2,
            16,
            2,
            1,
            1,
            0,
            2,
            2,
            1,
            0,
            2,
            3,
            0x80,
            0,
            2,
            4,
            0x80 | 63,
            0,
        ];
        let refused = vec![2, 1, 0x1F, 1, 0, 0, 0, b'H', b'I', b'D', b'_', b'E'];
        // An unknown product is not polled.
        let (out, _, _) = run(
            &[legacy(0x0115)],
            vec![vec![2, 2, 1, 2], refused.clone(), ui.clone()],
            &mut State::default(),
        );
        assert!(out.is_empty());
        let (out, f, _) = run(
            &[legacy(0x0015)],
            vec![vec![2, 2, 1, 2], refused.clone(), ui.clone()],
            &mut State::default(),
        );
        assert_eq!((out[0].percent, out[0].charging), (Some(63), true));
        assert_eq!(f.s().writes().last().unwrap(), &vec![2, 0x72, 1, 1]);
        // Wrong field marker: still listed as linked, without a level.
        let mut bad = ui;
        bad[16] = 3;
        let (out, _, _) = run(
            &[legacy(0x0015)],
            vec![vec![2, 2, 1, 2], refused, bad],
            &mut State::default(),
        );
        assert_eq!((out.len(), out[0].percent), (1, None));
    }

    #[test]
    fn locked_base_shows_the_cached_reading() {
        let mut st = State::default();
        run(
            &[legacy(0x0018)],
            vec![vec![2, 2, 1, 0x02], vec![2, 2, 1, 55]],
            &mut st,
        );
        let mut locked = legacy(0x0018);
        locked.page = 0;
        locked.in_len = 0;
        locked.out_len = 0;
        let mut hid = Fake::new();
        hid.s().refuse_open = true;
        let out = poll_colls(&[locked], &mut hid, &mut st);
        assert_eq!(out[0].percent, Some(55));
    }
}

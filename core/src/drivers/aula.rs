//! AULA, MCHOSE and Redragon Horus Pro wireless keyboards on the "BY" 2.4 GHz receivers (3554:FA09 /
//! FB40, MCHOSE's 41E4:2001, and MCHOSE K87 / K99 receivers on 05AC:024F), the AULA HERO 68 Air on the
//! "8K Wireless Receiver" (372E:106C), and the AULA SC580 mouse (cable 088D:062E, receiver 089D:062F).
//!
//! - BY receivers: 20-byte vendor collection (FF02 or FF04, usage 02). Request 13 <cmd> <packets>
//!   ... <sum>; battery is command 4A, the reply (13, command & 7F = 4A) carries the level at byte 5
//!   and the state at byte 6 (high nibble 1 = charging). MCHOSE firmware leaves the trailer at 0 on a
//!   single-packet reply. Generic receiver names are first asked for their six-byte model code
//!   (command 05), as M HUB does. A dozing keyboard can miss a request, so it is resent every 1.3 s
//!   within one deadline. A receiver that never answered is asked less and less often (some keyboards
//!   on the same ids wake up on every request). 05AC:024F is shared with the SONiX dongles: only the
//!   MCHOSE layout is probed here, anything else goes to `sonix.rs`.
//! - HERO 68 Air: acknowledged 20-byte frames on FF60:0061 (09 66, a 3-bit counter in the top bits of
//!   bytes 2..4, 4 data frames + trailer per message, every frame acknowledged). Battery request
//!   message 09 87 00 00 01 00; reply 87 00 00 01 00 02 <level> <flags: 10 = charging>.
//! - SC580: FF02:0000, 33-byte reports; GetDeviceInfo (30 00 01 00 ..., read-only). Status 0 OK,
//!   1 busy, 2 mouse offline, 3 data error; data: identity, firmware, sensor, ..., [8] charge state
//!   (1 charging: the level then reads a fixed 100), [9] level, [10] online.
//!
//! Ported from BatteryHub (src/drivers/aula.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0, which credits
//! PyFlat/Device-Battery-Info, deepan-alve/womier-l65-linux, ushineko/sanshoku, MCHOSE's M HUB,
//! wushuai0802/myfinder-device-battery and Crucialjun/omarchy-aula-SC580 (MIT).

use std::sync::Mutex;
use std::time::{Duration, Instant};

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::hid::{self, CABLE, Coll, Dev, Hid, RECEIVER, RECEIVER_GRACE, sum};
use crate::drivers::sonix;
use crate::registry::Match;

const RECEIVERS: [(u16, u16); 4] = [
    (0x3554, 0xFA09),
    (0x3554, 0xFB40),
    (0x41E4, 0x2001),
    (0x05AC, 0x024F),
];
const HERO: (u16, u16) = (0x372E, 0x106C);
const SC580: [(u16, u16, bool); 2] = [(0x088D, 0x062E, true), (0x089D, 0x062F, false)];

/// Exact ids only: 3554, 372E and 05AC are shared with other drivers' devices.
pub const MATCHES: &[Match] = &[
    Match {
        vid: 0x3554,
        pids: &[0xFA09, 0xFB40],
    },
    Match {
        vid: 0x41E4,
        pids: &[0x2001],
    },
    Match {
        vid: 0x05AC,
        pids: &[0x024F],
    },
    Match {
        vid: 0x372E,
        pids: &[0x106C],
    },
    Match {
        vid: 0x088D,
        pids: &[0x062E],
    },
    Match {
        vid: 0x089D,
        pids: &[0x062F],
    },
];

/// A read that fails this much sooner than its timeout is an I/O error (device unplugged).
fn failed_fast(started: Instant, wait: Duration) -> bool {
    started.elapsed() + Duration::from_millis(20) < wait
}

// --- BY receivers ----------------------------------------------------------------------------

fn is_apple_id(vid: u16, pid: u16) -> bool {
    (vid, pid) == (0x05AC, 0x024F)
}

fn is_by_collection(c: &Coll) -> bool {
    if !is_apple_id(c.vid, c.pid) {
        return (c.page == 0xFF02 || c.page == 0xFF04)
            && c.usage == 0x0002
            && c.out_len >= 20
            && c.in_len >= 20;
    }
    c.iface == 1 && c.page == 0xFF02 && c.usage == 0x0002 && c.in_len == 20 && c.out_len == 20
}

/// The MCHOSE 05AC:024F receiver also has an FF04:0002 feature collection on interface 1.
fn has_mchose_layout(colls: &[Coll], vid: u16, pid: u16) -> bool {
    colls.iter().any(|c| {
        c.is(vid, pid)
            && c.iface == 1
            && c.page == 0xFF04
            && c.usage == 0x0002
            && c.feat_len > 0
            && c.in_len == 0
            && c.out_len == 0
    })
}

/// M HUB's keyboardPwds table: the six-byte model code returned by command 05.
const MODELS: &[([u8; 6], &str)] = &[
    ([0x03, 0, 0, 0, 0x02, 0x1C], "MCHOSE G87"),
    ([0x03, 0, 0, 0, 0x00, 0x77], "MCHOSE K99"),
    ([0x03, 0, 0, 0, 0x01, 0xDF], "MCHOSE K99 V2"),
    ([0x03, 0, 0, 0, 0x01, 0x32], "MCHOSE G98"),
    ([0x03, 0, 0, 0, 0x00, 0xD6], "MCHOSE X75"),
    ([0x0A, 0, 0, 0, 0x00, 0x03], "MCHOSE G98 Pro"),
    ([0x03, 0, 0, 0, 0x01, 0xC6], "MCHOSE X75 V2"),
    ([0x03, 0, 0, 0, 0x01, 0x15], "MCHOSE G75 Pro"),
    ([0x0A, 0, 0, 0, 0x00, 0x18], "MCHOSE G98 V2"),
    ([0x0A, 0, 0, 0, 0x00, 0x2B], "MCHOSE UT98"),
    ([0x0A, 0, 0, 0, 0x00, 0x01], "MCHOSE Z75"),
    ([0x0A, 0, 0, 0, 0x00, 0x09], "MCHOSE Z75S"),
    ([0x03, 0, 0, 0, 0x01, 0xC7], "MCHOSE KX75"),
    ([0x03, 0, 0, 0, 0x01, 0x74], "MCHOSE GX87"),
    ([0x03, 0, 0, 0, 0x01, 0xED], "MCHOSE GX87 V2"),
    ([0x03, 0, 0, 0, 0x01, 0x35], "MCHOSE K87"),
    ([0x03, 0, 0, 0, 0x03, 0x35], "MCHOSE G75 V2"),
    ([0x0A, 0, 0, 0, 0x00, 0x34], "MCHOSE G87 V2"),
    ([0x03, 0, 0, 0, 0x00, 0x6A], "Redragon Horus Pro"),
    ([0x03, 0, 0, 0, 0x03, 0xDA], "Redragon Horus Pro V2"),
];

pub fn by_request(cmd: u8, packets: u8) -> Vec<u8> {
    let mut req = vec![0u8; 20];
    req[0] = 0x13;
    req[1] = cmd;
    req[2] = packets;
    req[19] = sum(&req[..19]);
    req
}

/// Whether a report is the reply to `cmd` (not an echo or an unrelated 13 frame).
pub fn is_by_reply(r: &[u8], cmd: u8) -> bool {
    if r.len() < 20 || r[0] != 0x13 || r[1] & 0x7F != cmd || r[4..19].iter().all(|b| *b == 0) {
        return false;
    }
    let checksum = r[19] == sum(&r[..19]);
    let mchose = r[19] == 0 && r[2] == 1 && r[3] == 0; // MCHOSE leaves the trailer at 0
    checksum || mchose
}

/// Sends 13 <cmd> and waits for its reply, resending every 1.3 s (byte 2 alternating 00 / 01).
fn by_exchange(port: &mut dyn Dev, cmd: u8, total: Duration) -> Option<Vec<u8>> {
    const RESEND: Duration = Duration::from_millis(1300);
    let start = Instant::now();
    let end = start + total;
    let mut sent = 0u32;
    port.flush();
    loop {
        let now = Instant::now();
        if now >= end {
            return None;
        }
        if now >= start + RESEND * sent {
            if !port.write(&by_request(cmd, (sent & 1) as u8)) {
                return None;
            }
            sent += 1;
        }
        let wait = (start + RESEND * sent)
            .min(end)
            .saturating_duration_since(now);
        let wait = wait.max(Duration::from_millis(1));
        let before = Instant::now();
        match port.read(wait) {
            None if failed_fast(before, wait) => return None,
            Some(r) if is_by_reply(&r, cmd) => return Some(r),
            _ => {}
        }
    }
}

/// M HUB reads the model code from bytes 5..10; one capture had it a byte earlier.
pub fn model_name(r: &[u8]) -> Option<&'static str> {
    [5, 4].iter().find_map(|&off| {
        MODELS
            .iter()
            .find(|(code, _)| r.get(off..off + 6) == Some(&code[..]))
            .map(|(_, n)| *n)
    })
}

/// Back-off for a receiver that has never answered since it was plugged in: asked less and less often
/// (5, 10, 20, 40 minutes, then hourly) once it stayed silent for 5 minutes. One answer ends it.
#[derive(Clone, Copy, Debug, Default)]
pub struct ReceiverState {
    answered: bool,
    misses: u32,
    backoffs: u32,
    first_miss: Option<Instant>,
    next_try: Option<Instant>,
}

impl ReceiverState {
    fn missed(&mut self, now: Instant) {
        const AFTER: Duration = Duration::from_secs(5 * 60);
        const FIRST: Duration = Duration::from_secs(5 * 60);
        const MAX: Duration = Duration::from_secs(60 * 60);
        self.misses += 1;
        let first = *self.first_miss.get_or_insert(now);
        if self.misses >= 3 && now.duration_since(first) >= AFTER {
            let doublings = self.backoffs.min(4);
            self.backoffs += 1;
            self.next_try = Some(now + (FIRST * (1 << doublings)).min(MAX));
        }
    }
}

fn poll_by(colls: &[Coll], hid: &mut dyn Hid, vid: u16, pid: u16) -> (bool, Option<Device>) {
    let mut tried = false;
    for c in colls
        .iter()
        .filter(|c| c.is(vid, pid) && is_by_collection(c))
    {
        let Some(mut port) = hid.open(c) else {
            continue;
        };
        let port = port.as_mut();
        let apple = is_apple_id(vid, pid);
        // The generic receiver names carry a model code; M HUB asks no other 3554 / 41E4 device.
        let generic = c.product.contains("2.4G Wireless Receiver")
            || c.product.contains("MCHOSE 2.4G Wireless");
        let model = if generic || apple {
            by_exchange(port, 0x05, Duration::from_millis(1300)).and_then(|r| model_name(&r))
        } else {
            None
        };
        let mchose = vid == 0x41E4 || apple || model.is_some();
        tried = true;
        let Some(r) = by_exchange(port, 0x4A, Duration::from_secs(4)) else {
            continue;
        };
        let level = r[5];
        if !(1..=100).contains(&level) {
            continue;
        }
        let name = match model {
            Some(m) => m.to_string(),
            // A product string that names the keyboard, as M HUB's getDevName uses.
            None if apple && !generic && !c.product.is_empty() => c.product.clone(),
            None if mchose => "MCHOSE wireless keyboard".into(),
            None => "AULA wireless keyboard".into(),
        };
        let mut d = Device::new(
            format!("aula:{vid:04X}:{pid:04X}"),
            name,
            Kind::Keyboard,
            RECEIVER,
        );
        d.percent = Some(level);
        d.charging = r[6] >> 4 == 1; // AULA's 0x10 cable flag, M HUB's chargeStatus
        // An idle keyboard sometimes misses the deadline: keep the last value meanwhile.
        d.grace = RECEIVER_GRACE;
        return (tried, Some(d));
    }
    (tried, None)
}

// --- HERO 68 Air ------------------------------------------------------------------------------

const HERO_ID: u8 = 0x09;
const HERO_MAGIC: u8 = 0x66;
const HERO_DATA: u8 = 0x0E;
const HERO_ACK: u8 = 0x00;
const HERO_TRAILER_HOST: u8 = 0x08;
const HERO_TRAILER_DEV: u8 = 0x07;

fn hero_header(counter: u8, index: u8, kind: u8) -> Vec<u8> {
    let mut f = vec![0u8; 20];
    f[0] = HERO_ID;
    f[1] = HERO_MAGIC;
    f[2] = 0x05 | if counter & 4 != 0 { 0x80 } else { 0 };
    f[3] = index | if counter & 2 != 0 { 0x80 } else { 0 };
    f[4] = kind | if counter & 1 != 0 { 0x80 } else { 0 };
    f
}

pub fn hero_data_frame(counter: u8, index: u8, msg: &[u8]) -> Vec<u8> {
    let mut f = hero_header(counter, index, HERO_DATA);
    let from = usize::from(index - 1) * 14;
    f[5..19].copy_from_slice(&msg[from..from + 14]);
    f[19] = sum(&f[1..19]);
    f
}

pub fn hero_trailer(counter: u8, msg: &[u8]) -> Vec<u8> {
    let mut f = hero_header(counter, 5, HERO_TRAILER_HOST);
    f[12] = 0xFF - sum(msg);
    f[13] = sum(&f[1..13]);
    f
}

pub fn hero_ack(r: &[u8]) -> Vec<u8> {
    let mut f = vec![0u8; 20];
    f[0] = HERO_ID;
    f[1] = HERO_MAGIC;
    f[2] = r[2];
    f[3] = r[3];
    f[4] = r[4] & 0x80;
    f[5] = sum(&f[1..5]);
    f
}

fn is_ack_for(r: &[u8], f: &[u8]) -> bool {
    r[4] & 0x7F == HERO_ACK
        && r[2] == f[2]
        && r[3] == f[3]
        && r[4] & 0x80 == f[4] & 0x80
        && r[5] == sum(&r[1..5])
}

/// The battery request message: 09 87 00 00 01 00, zero padded to 56 bytes.
pub fn hero_message() -> Vec<u8> {
    let mut msg = vec![0u8; 56];
    msg[0] = 0x09;
    msg[1] = 0x87;
    msg[4] = 0x01;
    msg
}

struct HeroLink<'a> {
    port: &'a mut dyn Dev,
    end: Instant,
    level: Option<u8>,
    flags: Option<u8>,
    in_reply: bool,
    reply_done: bool,
    io_error: bool,
}

impl<'a> HeroLink<'a> {
    fn new(port: &'a mut dyn Dev) -> Self {
        // The whole exchange takes ~30 ms when the keyboard is awake.
        let end = Instant::now() + Duration::from_secs(1);
        Self {
            port,
            end,
            level: None,
            flags: None,
            in_reply: false,
            reply_done: false,
            io_error: false,
        }
    }

    fn remaining(&self) -> Duration {
        self.end.saturating_duration_since(Instant::now())
    }

    /// The 4 data frames and the trailer, each sent once the previous one was acknowledged.
    fn send_message(&mut self, msg: &[u8]) -> bool {
        let mut counter = 1u8; // AULA Hub starts a session at 1 as well
        for i in 1..=4u8 {
            if !self.send_frame(&hero_data_frame(counter, i, msg)) {
                return false;
            }
            counter = (counter + 1) & 7;
        }
        self.send_frame(&hero_trailer(counter, msg))
    }

    fn send_frame(&mut self, f: &[u8]) -> bool {
        const RESEND: Duration = Duration::from_millis(150);
        while !self.remaining().is_zero() {
            if !self.port.write(f) {
                return false;
            }
            let resend_at = Instant::now() + RESEND;
            loop {
                let wait = resend_at
                    .saturating_duration_since(Instant::now())
                    .min(self.remaining());
                if wait.is_zero() {
                    break;
                }
                match self.read_frame(wait) {
                    Some(r) if is_ack_for(&r, f) => return true,
                    Some(_) => {}
                    None if self.io_error => return false,
                    None => break,
                }
            }
        }
        false
    }

    /// Reads until the reply has arrived in full.
    fn wait_for_battery(&mut self) -> bool {
        while !(self.level.is_some() && self.reply_done) {
            let left = self.remaining();
            if self.read_frame(left).is_none() {
                break;
            }
        }
        self.level.is_some()
    }

    /// One HERO frame; acknowledges the keyboard's data and trailer frames and picks up the battery.
    fn read_frame(&mut self, wait: Duration) -> Option<Vec<u8>> {
        let end = Instant::now() + wait;
        loop {
            let wait = end.saturating_duration_since(Instant::now());
            if wait.is_zero() {
                return None;
            }
            let before = Instant::now();
            let Some(r) = self.port.read(wait) else {
                self.io_error = failed_fast(before, wait);
                return None;
            };
            if r.len() < 20 || r[0] != HERO_ID || r[1] != HERO_MAGIC {
                continue; // unrelated report
            }
            match r[4] & 0x7F {
                HERO_DATA => {
                    if r[19] != sum(&r[1..19]) {
                        continue; // damaged: no ACK, the keyboard resends
                    }
                    self.port.write(&hero_ack(&r));
                    if r[3] & 0x7F == 1 {
                        self.inspect(&r);
                    }
                }
                HERO_TRAILER_DEV => {
                    self.port.write(&hero_ack(&r));
                    if self.in_reply {
                        self.reply_done = true;
                    }
                    self.in_reply = false;
                }
                _ => {}
            }
            return Some(r);
        }
    }

    /// First data frame of a keyboard message: payload starts at byte 5.
    fn inspect(&mut self, r: &[u8]) {
        self.in_reply = false;
        if r[5] == 0x87 && (1..=100).contains(&r[11]) {
            self.level = Some(r[11]); // reply to our request
            self.flags = Some(r[12]);
            self.in_reply = true;
        } else if r[5] == 0xFE && r[6] == 0x05 && (1..=100).contains(&r[7]) && self.level.is_none()
        {
            self.level = Some(r[7]); // pushed by the keyboard; keep waiting for the real reply
            self.flags = Some(r[8]);
        }
    }
}

fn query_hero(port: &mut dyn Dev) -> Option<(u8, bool)> {
    port.flush();
    let mut link = HeroLink::new(port);
    if !link.send_message(&hero_message()) || !link.wait_for_battery() {
        return None;
    }
    Some((link.level?, link.flags.is_some_and(|f| f & 0x10 != 0)))
}

fn poll_hero(colls: &[Coll], hid: &mut dyn Hid) -> Option<Device> {
    let (vid, pid) = HERO;
    for c in colls.iter().filter(|c| {
        c.is(vid, pid) && c.page == 0xFF60 && c.usage == 0x0061 && c.out_len >= 20 && c.in_len >= 20
    }) {
        let Some(mut port) = hid.open(c) else {
            continue;
        };
        let Some((level, charging)) = query_hero(port.as_mut()) else {
            continue;
        };
        let mut d = Device::new(
            format!("aula:{vid:04X}:{pid:04X}"),
            "AULA wireless keyboard",
            Kind::Keyboard,
            RECEIVER,
        );
        d.percent = Some(level);
        d.charging = charging;
        d.grace = RECEIVER_GRACE;
        return Some(d);
    }
    None
}

// --- SC580 ------------------------------------------------------------------------------------

#[derive(Debug, PartialEq)]
enum ScResult {
    Ok(Vec<u8>),
    Offline,
    Failed,
}

/// The read-only device-info request: report id 0, 30 00 01 00, zero data, zero checksum.
pub fn sc580_request() -> Vec<u8> {
    let mut req = vec![0u8; 33];
    req[1] = 0x30;
    req[3] = 0x01;
    req
}

fn sc580_info(port: &mut dyn Dev) -> ScResult {
    const RESEND: Duration = Duration::from_millis(250);
    let end = Instant::now() + Duration::from_secs(1);
    let left = || end.saturating_duration_since(Instant::now());
    port.flush();
    while !left().is_zero() {
        if !port.write(&sc580_request()) {
            return ScResult::Failed;
        }
        let resend_at = Instant::now() + RESEND;
        loop {
            let wait = resend_at
                .saturating_duration_since(Instant::now())
                .min(left());
            if wait.is_zero() {
                break;
            }
            let before = Instant::now();
            let Some(r) = port.read(wait) else {
                if failed_fast(before, wait) {
                    return ScResult::Failed; // device unplugged
                }
                continue;
            };
            if r.len() < 32 || r[0] == 0xD1 || r[0] == 0xD2 {
                continue; // short, or a pushed power / DPI notification
            }
            if r[0] != 0x30 {
                break; // the receiver's reply to an earlier request: send again
            }
            match r[1] {
                0 if r[31] == sum(&r[4..31]) => return ScResult::Ok(r[4..31].to_vec()),
                0 => continue,
                2 => return ScResult::Offline,
                1 => {
                    port.sleep(Duration::from_millis(300).min(left())); // busy
                    break;
                }
                _ => break, // data error: send again
            }
        }
    }
    ScResult::Failed
}

fn poll_sc580(colls: &[Coll], hid: &mut dyn Hid) -> Option<Device> {
    // One entry when the mouse charges on the cable next to its receiver: the cable is asked first.
    'ids: for (vid, pid, cable) in SC580 {
        for c in colls.iter().filter(|c| {
            c.is(vid, pid)
                && c.page == 0xFF02
                && c.usage == 0x0000
                && c.out_len >= 33
                && c.in_len >= 33
        }) {
            let Some(mut port) = hid.open(c) else {
                continue;
            };
            let data = match sc580_info(port.as_mut()) {
                ScResult::Failed => continue,
                ScResult::Offline => None,
                ScResult::Ok(d) => Some(d),
            };
            // The receiver says offline as soon as the mouse dozes; listed as asleep so the tracker
            // keeps the last level.
            let data = data.filter(|d| d[10] != 0);
            if data.as_ref().is_some_and(|d| d[9] > 100) {
                continue 'ids; // unusable level on this link: try the next one
            }
            let mut d = Device::new(
                "aula:sc580",
                "AULA SC580",
                Kind::Mouse,
                if cable { CABLE } else { RECEIVER },
            );
            match data {
                None => d.asleep = true,
                // While charging the mouse reports a fixed 100: the level is unknown, the tracker shows
                // the last real one with the charging mark. Charge state 2 (full) keeps its 100.
                Some(data) if data[8] == 1 => d.charging = true,
                Some(data) => d.percent = Some(data[9]),
            }
            d.grace = RECEIVER_GRACE; // only used when the receiver stops answering altogether
            return Some(d);
        }
    }
    None
}

// --- driver -----------------------------------------------------------------------------------

pub fn poll_with(
    colls: &[Coll],
    hid: &mut dyn Hid,
    states: &mut [ReceiverState; 4],
    now: Instant,
) -> Vec<Device> {
    let mut out: Vec<Device> = poll_sc580(colls, hid).into_iter().collect();
    for (i, &(vid, pid)) in RECEIVERS.iter().enumerate() {
        let st = &mut states[i];
        if !colls.iter().any(|c| c.is(vid, pid)) {
            *st = ReceiverState::default(); // unplugged
            continue;
        }
        let mchose_layout = !is_apple_id(vid, pid) || has_mchose_layout(colls, vid, pid);
        let waiting = !st.answered && st.next_try.is_some_and(|t| now < t);
        let mut found = None;
        if mchose_layout && !waiting {
            let (tried, d) = poll_by(colls, hid, vid, pid);
            if d.is_some() {
                st.answered = true;
            } else if tried && !st.answered {
                st.missed(now);
            }
            found = d;
        }
        // A SONiX dongle on Apple's id (or an MCHOSE one that did not answer): sonix.rs reads it.
        if found.is_none() && is_apple_id(vid, pid) {
            found = sonix::poll_product(colls, hid, vid, pid);
        }
        out.extend(found);
    }
    out.extend(poll_hero(colls, hid));
    out
}

static STATES: Mutex<[ReceiverState; 4]> = Mutex::new(
    [ReceiverState {
        answered: false,
        misses: 0,
        backoffs: 0,
        first_miss: None,
        next_try: None,
    }; 4],
);

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    let mut states = STATES.lock().unwrap_or_else(|e| e.into_inner());
    let now = Instant::now();
    hid::run(api, infos, |hid, colls| {
        poll_with(colls, hid, &mut states, now)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::hid::fake::{Fake, at, coll, per_write, responder, script};

    /// A flush drops queued replies, as on the device.
    fn run(colls: &[Coll], s: &Fake) -> Vec<Device> {
        poll_with(
            colls,
            &mut s.clone(),
            &mut [ReceiverState::default(); 4],
            Instant::now(),
        )
    }

    // --- SC580 (replies captured from an SC580, firmware 1.6.01) ---

    fn sc(head: &[u8], checksum: u8) -> Vec<u8> {
        let mut r = head.to_vec();
        r.resize(32, 0);
        r[31] = checksum;
        r
    }

    fn awake() -> Vec<u8> {
        sc(
            &[
                0x30, 0, 0x01, 0x0B, 0x4D, 0x31, 0x33, 0x37, 0x01, 0x16, 0x11, 0xAA, 0x00, 0x5A,
                0x01,
            ],
            0x15,
        )
    }

    fn charging() -> Vec<u8> {
        sc(
            &[
                0x30, 0, 0x01, 0x0B, 0x4D, 0x31, 0x33, 0x37, 0x01, 0x16, 0x11, 0xAA, 0x01, 0x64,
                0x01,
            ],
            0x20,
        )
    }

    fn with_checksum(mut r: Vec<u8>) -> Vec<u8> {
        r[31] = sum(&r[4..31]);
        r
    }

    fn sc580(cable: bool) -> Coll {
        let (vid, pid) = if cable {
            (0x088D, 0x062E)
        } else {
            (0x089D, 0x062F)
        };
        let mut c = at(
            coll(vid, pid, 0xFF02, 0x0000, 33),
            if cable { "cable" } else { "receiver" },
        );
        c.iface = 2;
        c
    }

    /// Replies already queued (BatteryHub's SC580 test device), kept across flushes.
    fn queued(replies: Vec<Vec<u8>>) -> Fake {
        let s = Fake::new();
        s.queue(replies);
        s
    }

    #[test]
    fn sc580_captured_replies() {
        let s = queued(vec![awake()]);
        let out = run(&[sc580(false)], &s);
        assert_eq!(s.s().writes(), vec![sc580_request()]);
        let d = &out[0];
        assert_eq!((d.percent, d.charging, d.asleep), (Some(90), false, false));
        assert_eq!(
            (d.name.as_str(), d.kind, d.via.as_str(), d.key.as_str()),
            ("AULA SC580", Kind::Mouse, RECEIVER, "aula:sc580")
        );

        let d = &run(&[sc580(false)], &queued(vec![sc(&[0x30, 0x02, 0x01], 0)]))[0];
        assert!(d.asleep && d.percent.is_none() && !d.charging);

        let d = &run(&[sc580(true)], &queued(vec![charging()]))[0];
        assert!(d.charging && d.percent.is_none() && !d.asleep && d.via == CABLE);

        let mut full = charging();
        full[12] = 0x02;
        let d = &run(&[sc580(true)], &queued(vec![with_checksum(full)]))[0];
        assert_eq!((d.percent, d.charging), (Some(100), false));
    }

    #[test]
    fn sc580_cable_first_and_retries() {
        let s = queued(vec![charging()]);
        let out = run(&[sc580(false), sc580(true)], &s);
        assert_eq!(out.len(), 1);
        assert_eq!(s.s().opened, vec!["cable".to_string()]);

        let push = sc(&[0xD1, 0x01, 0x5A, 0x00, 0x01, 0x11, 0x01], 0);
        let s = queued(vec![push, sc(&[0x13, 0x00, 0x01], 0), awake()]);
        assert_eq!(run(&[sc580(false)], &s)[0].percent, Some(90));
        assert_eq!(s.s().writes().len(), 2);
        let s = queued(vec![sc(&[0x30, 0x01, 0x01], 0), awake()]);
        assert_eq!(run(&[sc580(false)], &s)[0].percent, Some(90));
        assert_eq!(s.s().writes().len(), 2);
    }

    #[test]
    fn sc580_rejects_bad_checksum_level_and_collections() {
        let mut damaged = awake();
        damaged[31] ^= 0xFF;
        assert!(run(&[sc580(false)], &queued(vec![damaged])).is_empty());
        let mut over = awake();
        over[13] = 101;
        assert!(run(&[sc580(false)], &queued(vec![with_checksum(over)])).is_empty());
        for field in 0..5 {
            let mut c = sc580(false);
            match field {
                0 => c.vid = 0x088E,
                1 => c.pid = 0x0630,
                2 => c.page = 0xFF01,
                3 => c.usage = 0x0002,
                _ => c.in_len = 20,
            }
            let s = queued(vec![awake()]);
            assert!(run(&[c], &s).is_empty());
            assert!(s.s().opened.is_empty());
        }
        let s = script();
        s.s().refuse_send = true;
        assert!(run(&[sc580(false)], &s).is_empty());
        assert_eq!(s.s().writes().len(), 1);
    }

    // --- BY receivers ---

    fn by_coll(vid: u16, pid: u16, product: &str) -> Coll {
        let mut c = coll(vid, pid, 0xFF02, 0x0002, 20);
        c.product = product.into();
        c
    }

    fn by_reply(cmd: u8, payload: &[u8]) -> Vec<u8> {
        let mut r = vec![0u8; 20];
        r[0] = 0x13;
        r[1] = cmd | 0x80;
        r[4..4 + payload.len()].copy_from_slice(payload);
        r[19] = sum(&r[..19]);
        r
    }

    #[test]
    fn by_request_and_reply_checks() {
        let mut expected = vec![0u8; 20];
        expected[..3].copy_from_slice(&[0x13, 0x4A, 0x00]);
        expected[19] = 0x5D;
        assert_eq!(by_request(0x4A, 0), expected);
        assert!(is_by_reply(&by_reply(0x4A, &[0, 77, 0x10]), 0x4A));
        assert!(
            !is_by_reply(&by_request(0x4A, 0), 0x4A),
            "an echo of the request"
        );
        assert!(!is_by_reply(&by_reply(0x0A, &[0, 77, 0x10]), 0x4A));
        let mut mchose = by_reply(0x4A, &[0, 77, 0x01]);
        mchose[19] = 0;
        mchose[2] = 1;
        assert!(is_by_reply(&mchose, 0x4A));
        mchose[2] = 0;
        assert!(!is_by_reply(&mchose, 0x4A));
    }

    #[test]
    fn aula_keyboard_on_the_by_receiver() {
        let s = responder(|w| {
            if w[1] == 0x4A {
                vec![by_reply(0x4A, &[0, 77, 0x10])]
            } else {
                vec![]
            }
        });
        let out = run(&[by_coll(0x3554, 0xFA09, "Compx 2.4G")], &s);
        assert_eq!(s.s().writes(), vec![by_request(0x4A, 0)]);
        let d = &out[0];
        assert_eq!(
            (d.key.as_str(), d.name.as_str()),
            ("aula:3554:FA09", "AULA wireless keyboard")
        );
        assert_eq!(
            (d.percent, d.charging, d.kind),
            (Some(77), true, Kind::Keyboard)
        );
    }

    #[test]
    fn mchose_model_code_names_the_keyboard() {
        let s = responder(|w| match w[1] {
            0x05 => vec![by_reply(0x05, &[0, 0x03, 0, 0, 0, 0x01, 0xDF])],
            0x4A => vec![by_reply(0x4A, &[0, 64, 0x01])],
            _ => vec![],
        });
        let out = run(&[by_coll(0x3554, 0xFA09, "2.4G Wireless Receiver")], &s);
        assert_eq!(
            (out[0].name.as_str(), out[0].percent, out[0].charging),
            ("MCHOSE K99 V2", Some(64), false)
        );
        assert_eq!(
            model_name(&by_reply(0x05, &[0x03, 0, 0, 0, 0x00, 0x6A])),
            Some("Redragon Horus Pro")
        );
    }

    #[test]
    fn apple_id_goes_to_sonix_unless_it_has_the_mchose_layout() {
        // A SONiX dongle (FF60 on interface 3): read by sonix.rs, no BY request sent.
        let mut sonix_coll = coll(0x05AC, 0x024F, 0xFF60, 0x61, 33);
        sonix_coll.iface = 3;
        sonix_coll.product = "AJAZZ AK820 MAX 2.4G".into();
        let s = per_write(vec![vec![vec![0x20, 1, 0, 58]]]);
        let out = run(&[sonix_coll], &s);
        assert_eq!(
            (out[0].key.as_str(), out[0].name.as_str(), out[0].percent),
            ("sonix:05AC:024F", "AJAZZ AK820 MAX", Some(58))
        );

        // The MCHOSE K87 layout: BY collection on interface 1 plus the FF04 feature collection.
        let mut by = by_coll(0x05AC, 0x024F, "K87");
        by.iface = 1;
        let mut feature = coll(0x05AC, 0x024F, 0xFF04, 0x0002, 0);
        feature.iface = 1;
        feature.feat_len = 64;
        let s = responder(|w| match w[1] {
            0x05 => vec![by_reply(0x05, &[0, 0x03, 0, 0, 0, 0x01, 0x35])],
            0x4A => vec![by_reply(0x4A, &[0, 91, 0x00])],
            _ => vec![],
        });
        let out = run(&[by, feature], &s);
        assert_eq!(
            (out[0].key.as_str(), out[0].name.as_str(), out[0].percent),
            ("aula:05AC:024F", "MCHOSE K87", Some(91))
        );
    }

    #[test]
    fn silent_receiver_backs_off_after_five_minutes() {
        let mut st = [ReceiverState::default(); 4];
        let t0 = Instant::now();
        let colls = [by_coll(0x3554, 0xFA09, "Compx 2.4G")];
        let s = script();
        for minutes in [0, 3, 6] {
            poll_with(
                &colls,
                &mut s.clone(),
                &mut st,
                t0 + Duration::from_secs(minutes * 60),
            );
        }
        let sent = s.s().writes().len();
        assert_eq!(sent, 3);
        poll_with(
            &colls,
            &mut s.clone(),
            &mut st,
            t0 + Duration::from_secs(8 * 60),
        );
        assert_eq!(
            s.s().writes().len(),
            sent,
            "inside the first 5-minute back-off"
        );
        poll_with(
            &colls,
            &mut s.clone(),
            &mut st,
            t0 + Duration::from_secs(12 * 60),
        );
        assert_eq!(s.s().writes().len(), sent + 1);
        // Unplugging starts over.
        poll_with(
            &[],
            &mut s.clone(),
            &mut st,
            t0 + Duration::from_secs(13 * 60),
        );
        poll_with(
            &colls,
            &mut s.clone(),
            &mut st,
            t0 + Duration::from_secs(13 * 60),
        );
        assert_eq!(s.s().writes().len(), sent + 2);
    }

    // --- HERO 68 Air ---

    /// The keyboard: acknowledges every host data / trailer frame, and after the host trailer sends
    /// its reply (87 00 00 01 00 02 <level> <flags>) as one data frame plus its trailer.
    fn hero_keyboard(level: u8, flags: u8) -> Fake {
        responder(move |w| {
            assert_eq!((w.len(), w[0], w[1]), (20, HERO_ID, HERO_MAGIC));
            match w[4] & 0x7F {
                HERO_DATA => vec![hero_ack(w)],
                HERO_TRAILER_HOST => {
                    let mut msg = vec![0u8; 56];
                    msg[..8].copy_from_slice(&[0x87, 0, 0, 1, 0, 2, level, flags]);
                    let mut trailer = hero_header(2, 5, HERO_TRAILER_DEV);
                    trailer[13] = sum(&trailer[1..13]);
                    vec![hero_ack(w), hero_data_frame(1, 1, &msg), trailer]
                }
                _ => vec![], // the host's ACKs
            }
        })
    }

    #[test]
    fn hero_frames() {
        let msg = hero_message();
        let f = hero_data_frame(1, 1, &msg);
        assert_eq!(
            f[..11],
            [0x09, 0x66, 0x05, 0x01, 0x8E, 0x09, 0x87, 0, 0, 0x01, 0]
        );
        assert_eq!(f[19], sum(&f[1..19]));
        let t = hero_trailer(5, &msg);
        assert_eq!(t[..5], [0x09, 0x66, 0x85, 0x05, 0x88]);
        assert_eq!(t[12], 0xFF - (0x09 + 0x87 + 0x01));
        let a = hero_ack(&f);
        assert_eq!(
            a[..6],
            [0x09, 0x66, 0x05, 0x01, 0x80, sum(&[0x66, 0x05, 0x01, 0x80])]
        );
        assert!(is_ack_for(&a, &f));
    }

    #[test]
    fn hero_battery_exchange() {
        let s = hero_keyboard(68, 0x10);
        let c = coll(HERO.0, HERO.1, 0xFF60, 0x0061, 20);
        let out = run(std::slice::from_ref(&c), &s);
        let d = &out[0];
        assert_eq!(
            (d.key.as_str(), d.percent, d.charging),
            ("aula:372E:106C", Some(68), true)
        );
        let w = &s.s().writes();
        // 4 data frames + trailer, then ACKs for the keyboard's data frame and trailer.
        assert_eq!(w.len(), 7);
        assert_eq!(w[4][4] & 0x7F, HERO_TRAILER_HOST);
        assert_eq!(w[5][4] & 0x7F, HERO_ACK);

        let out = run(&[c], &hero_keyboard(55, 0x00));
        assert_eq!((out[0].percent, out[0].charging), (Some(55), false));
        assert!(run(&[coll(HERO.0, HERO.1, 0xFF60, 0x0061, 20)], &script()).is_empty());
    }
}

//! RAWM mice (Leviathan V4, ES21 / ER21 / SA-* / G11 families) and the SADES Dagger on their
//! 2.4 GHz receivers or USB cable (Nordic VID 1915), RAWM HUB "Hub Web 2.0" protocol on vendor
//! collection FF00:0001 (64-byte unnumbered reports):
//!  * Packet byte 0: type | payload size; 0x80 = event data, 0x40 = flow control, 0xC0 | n = the
//!    next byte is a packet for ESB channel n (the mouse behind a receiver is channel 0).
//!  * Events start with FF FF FF FF (device to host), then [cmd | ((len >> 4) & F0), len & FF, body].
//!  * The device only answers while the host keeps sending, so the host pings.
//!
//! Only the requests RAWM HUB sends on connect are used: CMD_QUERY (01) and CMD_PING (0E). The
//! QUERY_RESULT (02) JSON carries "battery" and "chr"; NOTIFY (0B) type 17 carries [battery, chr].
//! 1915 is shared with other Nordic products, so only RAWM HUB's ids are declared.
//!
//! Ported from BatteryHub (src/drivers/rawm.cpp and rawm_protocol.h, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0, from the RAWM HUB web app
//! (rawmtech.com/hub.html, hub.miracletek.net/hub/js/library.min.js).

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::hid_dev::{BUDGET, Coll, Dev, HidIo, Io, colls, find, hex4, ms_left, products};
use crate::registry::Match;

const VID: u16 = 0x1915;
const CMD_QUERY: u8 = 0x01;
const CMD_QUERY_RESULT: u8 = 0x02;
const CMD_NOTIFY: u8 = 0x0B;
const CMD_PING: u8 = 0x0E;
const NOTIFY_BATTERY: u8 = 0x17;
const OS_PC: u8 = 0x03;

const RECEIVERS: [u16; 9] = [
    0x232A, 0x232B, 0x2335, 0x2336, 0x233B, 0x233C, 0x2346, 0x2347, 0x234B,
];
const MICE: [u16; 25] = [
    0x2328, 0x2329, 0x232C, 0x232D, 0x232E, 0x232F, 0x2330, 0x2331, 0x2332, 0x2333, 0x2334, 0x2337,
    0x2338, 0x2339, 0x233A, 0x233E, 0x233F, 0x2340, 0x2341, 0x2343, 0x2344, 0x2348, 0x2349, 0x234A,
    0x2352,
];

pub static MATCHES: &[Match] = &[Match {
    vid: VID,
    pids: &[
        0x232A, 0x232B, 0x2335, 0x2336, 0x233B, 0x233C, 0x2346, 0x2347, 0x234B, 0x2328, 0x2329,
        0x232C, 0x232D, 0x232E, 0x232F, 0x2330, 0x2331, 0x2332, 0x2333, 0x2334, 0x2337, 0x2338,
        0x2339, 0x233A, 0x233E, 0x233F, 0x2340, 0x2341, 0x2343, 0x2344, 0x2348, 0x2349, 0x234A,
        0x2352,
    ],
}];

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    poll_with(&colls(infos), &mut HidIo(api))
}

/// Output report (leading 0 report id) carrying one event, optionally for an ESB channel.
pub fn packet(event: &[u8], channel: Option<u8>) -> Vec<u8> {
    let mut p = vec![0x00];
    if let Some(ch) = channel {
        p.push(0xC0 | (ch & 0x3F));
    }
    p.push(0x80 | (event.len() as u8 & 0x3F));
    p.extend_from_slice(event);
    p
}

/// send_event_query: [CMD_QUERY, len, OS_PC, 0, 0, unix seconds (8 bytes, little endian)].
pub fn query_event(unix_seconds: u64) -> Vec<u8> {
    let mut e = vec![CMD_QUERY, 0, OS_PC, 0, 0];
    e.extend_from_slice(&unix_seconds.to_le_bytes());
    e[1] = e.len() as u8;
    e
}

/// send_event_ping as the app actually transmits it.
pub fn ping_event() -> Vec<u8> {
    vec![CMD_PING, 4, 0, 0]
}

/// Integer value of "key": in a flat JSON object (strtol semantics).
pub fn json_int(json: &str, key: &str) -> Option<i64> {
    let k = format!("\"{key}\":");
    let rest = json[json.find(&k)? + k.len()..].trim_start();
    let digits = rest
        .char_indices()
        .take_while(|(i, ch)| ch.is_ascii_digit() || (*i == 0 && (*ch == '-' || *ch == '+')))
        .count();
    rest[..digits].parse().ok()
}

pub fn json_string(json: &str, key: &str) -> String {
    let k = format!("\"{key}\":\"");
    let Some(p) = json.find(&k) else {
        return String::new();
    };
    let rest = &json[p + k.len()..];
    rest.find('"')
        .map_or(String::new(), |e| rest[..e].to_string())
}

#[derive(Debug, Default, Clone)]
pub struct Status {
    /// A QUERY_RESULT was parsed.
    pub have_info: bool,
    pub json: String,
    pub percent: Option<u8>,
    pub charging: bool,
}

/// Reassembles the event stream of one channel and decodes the events of interest.
#[derive(Default)]
pub struct Stream {
    buf: Vec<u8>,
    pub status: Status,
}

impl Stream {
    pub fn append(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
        if self.buf.len() > 8192 {
            self.buf.drain(..self.buf.len() - 4096);
        }
        self.parse();
    }

    fn parse(&mut self) {
        let b = &self.buf;
        let mut o = 0;
        let mut events = Vec::new();
        while o + 6 <= b.len() {
            if b[o..o + 4] != [0xFF; 4] {
                o += 1;
                continue;
            }
            let h = o + 4;
            let cmd = b[h] & 0x0F;
            let len = ((usize::from(b[h]) << 4) & 0xF00) | usize::from(b[h + 1]);
            if len < 2 {
                o = h;
                continue;
            }
            if h + len > b.len() {
                break; // wait for the rest
            }
            events.push((cmd, b[h + 2..h + len].to_vec()));
            o = h + len;
        }
        self.buf.drain(..o);
        for (cmd, body) in events {
            if cmd == CMD_QUERY_RESULT {
                self.on_info(&body);
            } else if cmd == CMD_NOTIFY
                && body.len() >= 3
                && body[0] == NOTIFY_BATTERY
                && body[1] <= 100
            {
                self.status.percent = Some(body[1]);
                self.status.charging = body[2] == 1;
            }
        }
    }

    fn on_info(&mut self, body: &[u8]) {
        let end = body.iter().position(|b| *b == 0).unwrap_or(body.len());
        let s = String::from_utf8_lossy(&body[..end]).to_string();
        if !s.starts_with('{') {
            return;
        }
        if let Some(v) = json_int(&s, "battery").filter(|v| (0..=100).contains(v)) {
            self.status.percent = Some(v as u8);
        }
        if let Some(v) = json_int(&s, "chr") {
            self.status.charging = v == 1;
        }
        self.status.have_info = true;
        self.status.json = s;
    }
}

/// Routes input reports to the USB device's own stream or the mouse's (channel 0).
#[derive(Default)]
pub struct Link {
    pub direct: Stream,
    pub channel0: Stream,
}

impl Link {
    pub fn feed(&mut self, mut r: &[u8]) {
        if r.is_empty() {
            return;
        }
        let mut s = &mut self.direct;
        if r[0] & 0xC0 == 0xC0 {
            if r[0] & 0x3F != 0 {
                return;
            }
            s = &mut self.channel0;
            r = &r[1..];
            if r.is_empty() {
                return;
            }
        }
        if r[0] & 0xC0 != 0x80 {
            return; // flow control / raw packets
        }
        let n = usize::from(r[0] & 0x3F).min(r.len() - 1);
        s.append(&r[1..1 + n]);
    }

    fn stream(&self, channel: Option<u8>) -> &Stream {
        if channel.is_some() {
            &self.channel0
        } else {
            &self.direct
        }
    }
}

/// Reads every report that arrives within `ms`, never past `deadline`.
fn pump(d: &mut dyn Dev, link: &mut Link, ms: u64, deadline: Instant) {
    let end = deadline.min(Instant::now() + Duration::from_millis(ms));
    loop {
        let left = ms_left(end);
        if left == 0 {
            return;
        }
        let Some(r) = d.read(left) else { return };
        link.feed(&r);
    }
}

/// Asks for the device info of `channel` (or of the USB device itself), pinging meanwhile, since
/// the device only returns data while the host sends.
fn query(
    d: &mut dyn Dev,
    link: &mut Link,
    channel: Option<u8>,
    deadline: Instant,
) -> Option<Status> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |t| t.as_secs());
    if !d.write(&packet(&query_event(now), channel)) {
        return None;
    }
    while Instant::now() < deadline {
        if !d.write(&packet(&ping_event(), channel)) {
            return None;
        }
        pump(d, link, 80, deadline);
        if link.stream(channel).status.have_info {
            return Some(link.stream(channel).status.clone());
        }
    }
    None
}

fn poll_with(colls: &[Coll], io: &mut dyn Io) -> Vec<Device> {
    let end = Instant::now() + BUDGET;
    let mut out: Vec<Device> = Vec::new();
    for pid in products(colls, VID) {
        let receiver = RECEIVERS.contains(&pid);
        if !receiver && !MICE.contains(&pid) {
            continue;
        }
        for c in find(colls, VID, pid, |c| {
            c.page == 0xFF00 && c.usage == 1 && c.in_len == 65 && c.out_len == 65
        }) {
            if ms_left(end) == 0 {
                return out;
            }
            let Some(mut d) = io.open(c) else { continue };
            d.flush();
            // Receiver hello (~5 ms), then the mouse's ~550-byte info over ESB (~200 ms on the
            // Leviathan V4). One deadline covers both; a sleeping mouse uses all of it.
            let deadline = end.min(Instant::now() + Duration::from_millis(1200));
            let mut link = Link::default();
            let Some(mut info) = query(d.as_mut(), &mut link, None, deadline) else {
                continue;
            };
            if receiver {
                // The receiver answered; the mouse behind it is ESB channel 0.
                match query(d.as_mut(), &mut link, Some(0), deadline) {
                    Some(i) => info = i,
                    None => break, // mouse off or out of range: nothing to show
                }
            }
            let Some(pct) = info.percent else { continue };
            let mouse_pid = json_int(&info.json, "pi").filter(|p| (1..=0xFFFF).contains(p));
            let mut name = json_string(&info.json, "dn");
            if name.is_empty() {
                name = "wireless mouse".into();
            }
            if !name.contains("RAWM") && !name.contains("SADES") {
                name = format!("RAWM {name}");
            }
            let mut s = Device::new(
                format!("rawm:{}", hex4(mouse_pid.map_or(pid, |p| p as u16))),
                name,
                Kind::Mouse,
                if receiver {
                    "2.4 GHz receiver"
                } else {
                    "USB cable"
                },
            );
            s.percent = Some(pct);
            s.charging = info.charging;
            // A sleeping mouse stops answering through the receiver: keep the last reading.
            s.grace = Duration::from_secs(1800);
            // Reachable through both cable and receiver while charging: keep one, the cable's.
            match out.iter_mut().find(|o| o.key == s.key) {
                None => out.push(s),
                Some(existing) if !receiver => *existing = s,
                Some(_) => {}
            }
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::hid_dev::fake::{Fake, Sent, coll};
    use std::collections::{HashMap, VecDeque};

    // ---- rawm_protocol_test.cpp ----

    /// Splits a stream into input reports as the receiver sends them, padded with stale bytes.
    fn reports(stream: &[u8], chunk: usize, channel: Option<u8>, pad: u8) -> Vec<Vec<u8>> {
        stream
            .chunks(chunk)
            .map(|part| {
                let mut r = Vec::new();
                if let Some(ch) = channel {
                    r.push(0xC0 | ch);
                }
                r.push(0x80 | part.len() as u8);
                r.extend_from_slice(part);
                r.resize(64, pad);
                r
            })
            .collect()
    }

    fn event(cmd: u8, body: &[u8]) -> Vec<u8> {
        let len = body.len() + 2;
        let mut e = vec![
            0xFF,
            0xFF,
            0xFF,
            0xFF,
            cmd | ((len >> 4) as u8 & 0xF0),
            len as u8,
        ];
        e.extend_from_slice(body);
        e
    }

    fn text(s: &str) -> Vec<u8> {
        let mut v = s.as_bytes().to_vec();
        v.push(0);
        v
    }

    #[test]
    fn output_framing_matches_the_capture() {
        assert_eq!(
            packet(&query_event(0x6AFC83D1), None),
            [
                0x00, 0x8D, 0x01, 0x0D, 0x03, 0x00, 0x00, 0xD1, 0x83, 0xFC, 0x6A, 0, 0, 0, 0
            ]
        );
        assert_eq!(
            packet(&ping_event(), Some(0)),
            [0x00, 0xC0, 0x84, 0x0E, 0x04, 0x00, 0x00]
        );
    }

    #[test]
    fn reassembles_receiver_and_mouse_info() {
        let mut link = Link::default();
        let rx = event(
            CMD_QUERY_RESULT,
            &text(
                "{\"r\":\"2.0.2\",\"rc\":4,\"dn\":\"RAWM HS Receiver\",\"pi\":9030,\"vi\":6421,\"msg\":\"\",\"cn\":0,\
                 \"esb_addr\":\"00000000000000000000000000000000\",\"esb_ch\":255,\"light\":1,\"slow\":0}",
            ),
        );
        for r in reports(&rx, 63, None, 0x5A) {
            link.feed(&r);
        }
        assert!(link.direct.status.have_info && link.direct.status.percent.is_none());
        assert_eq!(
            json_string(&link.direct.status.json, "dn"),
            "RAWM HS Receiver"
        );
        assert!(!link.channel0.status.have_info);

        // Flow-control packet seen before the mouse data (ignored).
        let mut flow = vec![0xC0, 0x52, 0xFB, 0x0B];
        flow.resize(18, 0);
        flow.push(0x0C);
        link.feed(&flow);

        let mut mouse = String::from(
            "{\"r\":\"1.2.0\",\"rc\":6,\"dn\":\"LEVIATHAN V4\",\"pi\":9034,\"vi\":6421,\"battery\":100,\"chr\":0,\"cpi\":1600,\
             \"polling\":1000,\"light\":48,\"cpi_l\":[400,800,1600,3200,0,0,0,0],\"esb_addr\":\"00000000000000000000000000000000\",\
             \"msg\":\"\",\"cn\":8,\"pm\":2,\"esb_ch\":0,\"crc\":1,\"sst\":\"PAW3950\",\"rr\":\"1.0.0\",\"rrc\":1}",
        );
        while mouse.len() < 547 {
            mouse.insert(mouse.len() - 1, ' '); // keep the captured 2-byte length form
        }
        let info = event(CMD_QUERY_RESULT, &text(&mouse));
        assert_eq!(info[4..6], [0x22, 0x26]);
        let mut stream = info;
        stream.extend(event(CMD_PING, &[0x8E, 0, 0])); // captured ping reply
        stream.extend(event(
            CMD_NOTIFY,
            &[0x14, 0x03, 0x08, 0x16, 0x01, 0x0A, 0x00, 0x01, 0x01],
        ));
        let rs = reports(&stream, 30, Some(0), 0x5A);
        for (i, r) in rs.iter().enumerate().take(rs.len() - 1) {
            link.feed(r);
            if i < 10 {
                assert!(!link.channel0.status.have_info, "not complete yet");
            }
        }
        link.feed(rs.last().unwrap());
        let st = &link.channel0.status;
        assert!(st.have_info && st.percent == Some(100) && !st.charging);
        assert_eq!(json_int(&st.json, "pi"), Some(0x234A));
        assert_eq!(json_string(&st.json, "dn"), "LEVIATHAN V4");
        assert!(
            link.direct.status.percent.is_none(),
            "channel data never leaks"
        );

        // NOTIFY_TYPE_MOUSE_BATTERY: [0x17, percent, charging].
        for r in reports(&event(CMD_NOTIFY, &[0x17, 63, 1]), 30, Some(0), 0x5A) {
            link.feed(&r);
        }
        assert!(link.channel0.status.percent == Some(63) && link.channel0.status.charging);
        for r in reports(&event(CMD_NOTIFY, &[0x17, 101, 0]), 30, Some(0), 0x5A) {
            link.feed(&r);
        }
        assert_eq!(link.channel0.status.percent, Some(63));

        // Other channels are not the mouse.
        link.feed(&[0xC1, 0x86, 0xFF, 0xFF, 0xFF, 0xFF, 0x0B, 0x05]);
        assert_eq!(link.channel0.status.percent, Some(63));
    }

    // ---- rawm_driver_test.cpp ----

    #[derive(Clone)]
    struct Unit {
        receiver: bool,
        answers: bool,
        mouse_awake: bool,
        own_info: String,
        mouse_info: String,
    }

    fn receiver_info(pid: u16) -> String {
        format!(
            "{{\"r\":\"2.0.2\",\"rc\":4,\"dn\":\"RAWM HS Receiver\",\"pi\":{pid},\"vi\":6421,\"cn\":0}}"
        )
    }

    fn mouse_info(name: &str, pid: u16, battery: u8, chr: u8) -> String {
        format!(
            "{{\"r\":\"1.2.0\",\"rc\":6,\"dn\":\"{name}\",\"pi\":{pid},\"vi\":6421,\"battery\":{battery},\"chr\":{chr},\
             \"cpi\":1600,\"polling\":1000,\"crc\":1,\"sst\":\"PAW3950\"}}"
        )
    }

    /// Devices that only send while the host writes: a few reports are released per packet.
    fn devices(units: Vec<(u16, Unit)>) -> Fake {
        let units: HashMap<u16, Unit> = units.into_iter().collect();
        let mut queued: HashMap<String, VecDeque<Vec<u8>>> = HashMap::new();
        Fake::with(move |s, path, kind, data| {
            assert_eq!(kind, Sent::Write);
            let pid = u16::from_str_radix(&path[5..9], 16).unwrap();
            let u = &units[&pid];
            assert!(data.len() >= 3 && data[0] == 0);
            let mut i = 1;
            let mut channel = None;
            if data[i] & 0xC0 == 0xC0 {
                channel = Some(data[i] & 0x3F);
                i += 1;
            }
            assert!(channel.is_none() || channel == Some(0));
            assert_eq!(data[i] & 0xC0, 0x80);
            let e = &data[i + 1..];
            assert_eq!(e.len(), usize::from(data[i] & 0x3F));
            let is_query = e.len() == 13 && e[..3] == [0x01, 13, 0x03];
            assert!(is_query || e == [0x0E, 0x04, 0x00, 0x00]);
            let q = queued.entry(path.to_string()).or_default();
            if is_query && channel.is_none() && u.answers {
                q.extend(reports(&event(2, &text(&u.own_info)), 63, None, 0));
            }
            if is_query && channel == Some(0) && u.receiver && u.mouse_awake {
                q.extend(reports(&event(2, &text(&u.mouse_info)), 30, Some(0), 0));
            }
            for _ in 0..4 {
                if let Some(r) = q.pop_front() {
                    s.push_input(path, r);
                }
            }
        })
    }

    fn rawm(pid: u16) -> Coll {
        coll(VID, pid, 2, 0xFF00, 1, 65, 65, 0)
    }

    #[test]
    fn leviathan_behind_the_receiver() {
        let mut f = devices(vec![(
            0x2346,
            Unit {
                receiver: true,
                answers: true,
                mouse_awake: true,
                own_info: receiver_info(0x2346),
                mouse_info: mouse_info("LEVIATHAN V4", 0x234A, 82, 0),
            },
        )]);
        let out = poll_with(
            &[coll(VID, 0x2346, 2, 0x0001, 2, 8, 8, 0), rawm(0x2346)],
            &mut f,
        );
        assert_eq!(out.len(), 1);
        let s = &out[0];
        assert_eq!(
            (s.key.as_str(), s.name.as_str()),
            ("rawm:234A", "RAWM LEVIATHAN V4")
        );
        assert_eq!((s.via.as_str(), s.kind), ("2.4 GHz receiver", Kind::Mouse));
        assert_eq!((s.percent, s.charging), (Some(82), false));
        assert_eq!(s.grace, Duration::from_secs(1800));
        assert_eq!(f.s().opens, 1);
    }

    #[test]
    fn sleeping_mouse_and_silent_receiver_are_bounded() {
        for (pid, answers, awake) in [(0x2346, true, false), (0x232A, false, true)] {
            let mut f = devices(vec![(
                pid,
                Unit {
                    receiver: true,
                    answers,
                    mouse_awake: awake,
                    own_info: receiver_info(pid),
                    mouse_info: mouse_info("ES21", 0x2328, 50, 0),
                },
            )]);
            let start = Instant::now();
            assert!(poll_with(&[rawm(pid)], &mut f).is_empty());
            assert!(start.elapsed() < Duration::from_millis(1500));
        }
    }

    #[test]
    fn cable_and_receiver_are_one_entry() {
        let cable = Unit {
            receiver: false,
            answers: true,
            mouse_awake: false,
            own_info: mouse_info("LEVIATHAN V4", 0x234A, 47, 1),
            mouse_info: String::new(),
        };
        let out = poll_with(&[rawm(0x234A)], &mut devices(vec![(0x234A, cable.clone())]));
        assert_eq!(
            (out[0].key.as_str(), out[0].via.as_str()),
            ("rawm:234A", "USB cable")
        );
        assert_eq!((out[0].percent, out[0].charging), (Some(47), true));

        let rx = Unit {
            receiver: true,
            answers: true,
            mouse_awake: true,
            own_info: receiver_info(0x2346),
            mouse_info: mouse_info("LEVIATHAN V4", 0x234A, 47, 0),
        };
        let out = poll_with(
            &[rawm(0x234A), rawm(0x2346)],
            &mut devices(vec![(0x2346, rx), (0x234A, cable)]),
        );
        assert_eq!(out.len(), 1);
        assert_eq!(
            (out[0].via.as_str(), out[0].charging, out[0].percent),
            ("USB cable", true, Some(47))
        );
    }

    #[test]
    fn names_and_missing_battery() {
        let unit = |info: String| Unit {
            receiver: true,
            answers: true,
            mouse_awake: true,
            own_info: receiver_info(0x232A),
            mouse_info: info,
        };
        let out = poll_with(
            &[rawm(0x232A)],
            &mut devices(vec![(
                0x232A,
                unit(mouse_info("SADES Dagger", 0x2329, 100, 0)),
            )]),
        );
        assert_eq!(
            (out[0].name.as_str(), out[0].key.as_str()),
            ("SADES Dagger", "rawm:2329")
        );
        let out = poll_with(
            &[rawm(0x232A)],
            &mut devices(vec![(
                0x232A,
                unit("{\"dn\":\"LEVIATHAN V4\",\"pi\":9034}".into()),
            )]),
        );
        assert!(out.is_empty());
    }

    #[test]
    fn strict_matching() {
        let mut f = devices(vec![]);
        let out = poll_with(
            &[
                rawm(0xAE1C),                                  // Ninjutso Sora V2 receiver
                rawm(0xAC1C),                                  // Sprime PM1 receiver
                rawm(0x0723),                                  // Darmoshark on a Nordic 1K receiver
                coll(0x373E, 0x2346, 2, 0xFF00, 1, 65, 65, 0), // right PID, wrong vendor
                coll(VID, 0x2346, 2, 0xFF01, 1, 65, 65, 0),    // wrong usage page
                coll(VID, 0x2346, 2, 0xFF00, 1, 33, 33, 0),    // wrong report size
                coll(VID, 0x2346, 2, 0x0001, 2, 8, 8, 0),      // the mouse collection
            ],
            &mut f,
        );
        let st = f.s();
        assert!(out.is_empty() && st.opens == 0 && st.sent.is_empty());
    }
}

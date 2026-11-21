//! Corsair wireless headsets, mice and keyboards (VID 1B1C).
//!  * Void / Void Pro / Void Elite / HS70 / HS80 family: output C9 64 -> [2] % (bit 7 = mic up),
//!    [4] status (0 offline, 1-2 ok, 4-5 charging). From HeadsetControl.
//!  * "Bragi" devices (Virtuoso, Void V2, HS80 MAX, Slipstream mice and keyboards): 02 <target> 02
//!    <property> is a GET; reply [source][02][status][value LE x4]. Property 0F = battery (0..1000),
//!    10 = charge state (1 charging, 3 full), 12 = product id. Target 08 is the device or receiver
//!    itself, 09+ the devices paired to a receiver. Only GETs: never switches to software mode.
//!  * Compx mice (Sabre V2 Pro): 17-byte report 08, command 03 = link, 04 = battery
//!    ([6] %, [7] charging, [8..9] mV), byte 16 = checksum; collection FF02:0002.
//!
//! Ported from BatteryHub (src/drivers/corsair.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0, which credits
//! HeadsetControl, OpenRGB and HaloBattery.

use std::time::Duration;

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind, guess_kind};
use crate::drivers::hid::{self, Coll, Dev, Hid, MAX_WAIT, find, products};

pub const VID: u16 = 0x1B1C;
const RECEIVER_GRACE: Duration = Duration::from_secs(30 * 60);

const VOID_PIDS: &[u16] = &[
    0x1B1C, 0x1B27, 0x0A14, 0x0A16, 0x0A17, 0x0A1D, 0x0A1A, 0x1B2A, 0x1B23, 0x1B29, 0x0A55, 0x0A51,
    0x0A52, 0x0A38, 0x0A4F, 0x0A2B, 0x0A75, 0x0A56,
];
const BRAGI_PIDS: &[u16] = &[
    0x2A08, 0x2A02, 0x0A97, 0x0A64, 0x0A62, 0x0A3E, 0x0A3D, 0x2B00,
];

/// Mice on the Compx battery protocol.
fn compx_name(pid: u16) -> Option<&'static str> {
    match pid {
        0x2B2A => Some("Corsair Sabre V2 Pro Ultralight"),
        0x2B34 => Some("Corsair Sabre v2 PRO WIRELESS MG"), // device request #2563, untested
        _ => None,
    }
}

/// Devices seen behind a receiver, by the product id they report (property 12).
fn paired_model(pid: u32) -> Option<(&'static str, Kind)> {
    match pid {
        0x1B7E => Some(("Corsair Dark Core RGB Pro SE", Kind::Mouse)), // on Slipstream receiver 1BDC
        _ => None,
    }
}

// ---- Void ---------------------------------------------------------------------------------------

/// Void reply: (percent, charging, connected).
pub fn parse_void(r: &[u8]) -> Option<(Option<u8>, bool, bool)> {
    if r.len() < 5 {
        return None;
    }
    let st = r[4];
    let charging = st == 4 || st == 5;
    let pct = (st == 1 || st == 2 || charging).then(|| (r[2] & 0x7F).min(100));
    Some((pct, charging, st != 0))
}

fn poll_void(hid: &mut dyn Hid, c: &Coll) -> Option<Option<Device>> {
    let mut d = hid.open(c)?;
    d.flush();
    if !d.write(&[0xC9, 0x64]) {
        return None;
    }
    let r = hid::read_until(d.as_mut(), MAX_WAIT, |_| true)?;
    let (pct, charging, connected) = parse_void(&r)?;
    if !connected {
        return Some(None); // receiver answered, headset off: the tracker keeps the last level
    }
    let name = if c.product.is_empty() {
        "Corsair wireless headset"
    } else {
        &c.product
    };
    let mut s = Device::new(
        format!("corsair:{:04X}", c.pid),
        name,
        Kind::Headset,
        "2.4 GHz receiver",
    );
    s.charging = charging;
    s.percent = pct;
    s.grace = RECEIVER_GRACE;
    Some(Some(s))
}

// ---- Compx --------------------------------------------------------------------------------------

pub fn compx_checksum(b: &[u8]) -> u8 {
    0x55u8.wrapping_sub(b[..16].iter().fold(0u8, |a, x| a.wrapping_add(*x)))
}

pub fn compx_request(cmd: u8) -> Vec<u8> {
    let mut req = vec![0u8; 17];
    req[0] = 0x08;
    req[1] = cmd;
    req[16] = compx_checksum(&req);
    req
}

pub fn is_compx_reply(r: &[u8], cmd: u8) -> bool {
    r.len() >= 17 && r[0] == 0x08 && r[1] == cmd && r[16] == compx_checksum(r)
}

fn compx_query(hid: &dyn Hid, d: &mut dyn Dev, cmd: u8) -> Option<Vec<u8>> {
    d.flush();
    if !d.write(&compx_request(cmd)) {
        return None;
    }
    let end = hid.now() + MAX_WAIT;
    loop {
        let left = end.saturating_sub(hid.now());
        if left.is_zero() {
            return None;
        }
        let r = d.read(left).filter(|r| r.len() >= 17)?;
        if is_compx_reply(&r, cmd) {
            return Some(r);
        }
    }
}

/// Some(None) = the receiver answered but the mouse is off; None = no answer.
fn poll_compx(hid: &mut dyn Hid, c: &Coll, name: &str) -> Option<Option<Device>> {
    let mut d = hid.open(c)?;
    let mut answered = false;
    let mut connected = false;
    // Is the mouse linked to the receiver? If command 03 is unsupported, try the battery directly.
    if let Some(r) = compx_query(hid, d.as_mut(), 0x03) {
        answered = true;
        connected = r[6] != 0;
    }
    let mut s = Device::new(
        format!("corsair:{:04X}", c.pid),
        name,
        Kind::Mouse,
        "2.4 GHz receiver",
    );
    s.grace = RECEIVER_GRACE;
    if !answered || connected {
        if let Some(r) = compx_query(hid, d.as_mut(), 0x04) {
            answered = true;
            connected = true;
            if r[6] <= 100 {
                s.percent = Some(r[6]);
            }
            s.charging = r[7] != 0;
            let mv = u16::from_be_bytes([r[8], r[9]]);
            s.millivolts = (mv != 0).then_some(mv);
        } else if answered {
            s.asleep = true; // linked, but the battery query failed
        }
    }
    if !answered {
        return None;
    }
    Some(connected.then_some(s))
}

// ---- Bragi --------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Unknown,
    Prefixed,
    Plain,
}

/// Value of a Bragi GET reply. `plain` framing has no leading 02.
pub fn parse_bragi(r: &[u8], plain: bool, target: u8) -> Option<Result<u32, u8>> {
    let o = if plain { 0 } else { 1 };
    let source = if target == 0x08 { 0x00 } else { 0x01 };
    if r.len() < o + 7 || r[o + 1] != 0x02 || r[o] != source {
        return None; // notification or another app's reply
    }
    if r[o + 2] != 0x00 {
        return Some(Err(r[o + 2]));
    }
    Some(Ok(u32::from_le_bytes([
        r[o + 3],
        r[o + 4],
        r[o + 5],
        r[o + 6],
    ])))
}

struct Bragi {
    d: Box<dyn Dev>,
    numbered: bool,
    mode: Mode,
}

impl Bragi {
    /// Two framings exist: "prefixed" (02 <target> 02 <prop>) and "plain" on newer Slipstream
    /// receivers (00 <target> 02 <prop>). The first that answers is kept for this handle.
    fn get(&mut self, hid: &dyn Hid, target: u8, prop: u8) -> Option<u32> {
        if self.mode != Mode::Plain
            && let Some(v) = self.get_framed(hid, false, target, prop)
        {
            self.mode = Mode::Prefixed;
            return Some(v);
        }
        if self.mode == Mode::Prefixed {
            return None;
        }
        let v = self.get_framed(hid, true, target, prop)?;
        self.mode = Mode::Plain;
        Some(v)
    }

    fn get_framed(&mut self, hid: &dyn Hid, plain: bool, target: u8, prop: u8) -> Option<u32> {
        let req: Vec<u8> = if plain {
            vec![0x00, target, 0x02, prop]
        } else if self.numbered {
            vec![0x02, target, 0x02, prop]
        } else {
            vec![0x00, 0x02, target, 0x02, prop]
        };
        self.d.flush();
        if !self.d.write(&req) {
            return None;
        }
        let end = hid.now() + Duration::from_millis(300);
        loop {
            let left = end.saturating_sub(hid.now());
            if left.is_zero() {
                return None;
            }
            let r = self.d.read(left)?;
            match parse_bragi(&r, plain, target) {
                None => continue,
                Some(v) => return v.ok(),
            }
        }
    }
}

fn poll_bragi(hid: &mut dyn Hid, colls: &[Coll], vid: u16, pid: u16) -> Vec<Device> {
    let known = BRAGI_PIDS.contains(&pid);
    let candidates = find(colls, vid, pid, |c| {
        let sizes = matches!(c.out_len, 64 | 65) && matches!(c.in_len, 64 | 65);
        sizes && (c.page == 0xFF42 || (known && c.page >= 0xFF00) || (known && c.iface == 4))
    });
    for c in candidates {
        let Some(d) = hid.open(c) else { continue };
        let mut b = Bragi {
            d,
            numbered: c.out_len == 64,
            mode: Mode::Unknown,
        };
        let mut out = Vec::new();
        // Paired devices behind a receiver first, then the device itself (wired / Bluetooth).
        for target in [0x09u8, 0x0A, 0x0B, 0x08] {
            if target == 0x08 && !out.is_empty() {
                break;
            }
            let mut level = b.get(hid, target, 0x0F);
            for _ in 0..2 {
                if !matches!(level, Some(l) if l == 0 || l > 1000) {
                    break;
                }
                level = b.get(hid, target, 0x0F);
            }
            let Some(level) = level.filter(|l| *l <= 1000) else {
                continue;
            };
            let name = if c.product.is_empty() {
                "Corsair wireless device"
            } else {
                &c.product
            };
            let mut s = Device::new(
                format!("corsair:{pid:04X}:{target}"),
                name,
                guess_kind(name),
                if target == 0x08 {
                    "USB / Bluetooth"
                } else {
                    "Corsair receiver"
                },
            );
            // A receiver names itself, not the device behind it. Property 12 is the paired
            // device's product id; Corsair headsets use 0Axx.
            if target != 0x08
                && let Some(dev_pid) = b.get(hid, target, 0x12)
            {
                if let Some((n, k)) = paired_model(dev_pid) {
                    s.name = n.into();
                    s.kind = k;
                } else if dev_pid >> 8 == 0x0A {
                    s.kind = Kind::Headset;
                    if c.product.contains("Receiver") {
                        s.name = "Corsair wireless headset".into();
                    }
                }
            }
            // Corsair headsets and their receivers use product ids 0Axx.
            if s.kind == Kind::Other && pid >> 8 == 0x0A {
                s.kind = Kind::Headset;
            }
            if s.kind == Kind::Other {
                s.kind = hid::kind_from_colls(colls, vid, pid);
            }
            s.percent = Some(bragi_percent(level));
            if let Some(charge) = b.get(hid, target, 0x10) {
                s.charging = charge == 1 || charge == 3;
            }
            if target != 0x08 {
                s.grace = RECEIVER_GRACE;
            }
            out.push(s);
        }
        if !out.is_empty() {
            return out;
        }
    }
    Vec::new()
}

/// Bragi battery 0..1000 to percent, rounded.
pub fn bragi_percent(level: u32) -> u8 {
    ((level + 5) / 10).min(100) as u8
}

fn poll_with(hid: &mut dyn Hid, colls: &[Coll]) -> Vec<Device> {
    let mut out = Vec::new();
    for (vid, pid) in products(colls) {
        if VOID_PIDS.contains(&pid) {
            let void = find(colls, vid, pid, |c| {
                (c.page == 0xFFC5 || c.page == 0xFF00) && c.out_len > 0
            });
            for c in void {
                if let Some(s) = poll_void(hid, c) {
                    out.extend(s);
                    break;
                }
            }
            continue;
        }
        if let Some(name) = compx_name(pid) {
            let compx = find(colls, vid, pid, |c| {
                c.page == 0xFF02 && c.usage == 0x0002 && c.out_len >= 17 && c.in_len >= 17
            });
            for c in compx {
                if let Some(s) = poll_compx(hid, c, name) {
                    out.extend(s);
                    break;
                }
            }
            continue;
        }
        out.extend(poll_bragi(hid, colls, vid, pid));
    }
    out
}

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    hid::run(api, infos, poll_with)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::hid::fake::{Fake, Op};
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    fn queue(replies: Vec<Vec<u8>>) -> (Fake, Arc<Mutex<Vec<Vec<u8>>>>) {
        let writes = Arc::new(Mutex::new(Vec::new()));
        let w = writes.clone();
        let mut q: VecDeque<Vec<u8>> = replies.into();
        let f = Fake::with(move |_, _, op| match op.clone() {
            Op::Write(data) => {
                w.lock().unwrap().push(data);
                Some(vec![])
            }
            Op::Read(_) => q.pop_front(),
            Op::Flush => Some(vec![]),
            _ => None,
        });
        (f, writes)
    }

    #[test]
    fn void_status() {
        assert_eq!(
            parse_void(&[0, 0, 0x80 | 64, 0, 1]),
            Some((Some(64), false, true))
        );
        assert_eq!(parse_void(&[0, 0, 70, 0, 5]), Some((Some(70), true, true)));
        assert_eq!(parse_void(&[0, 0, 70, 0, 0]), Some((None, false, false)));
        assert_eq!(parse_void(&[0, 0, 70, 0]), None);
        let c = Coll {
            vid: VID,
            pid: 0x0A14,
            page: 0xFFC5,
            out_len: 65,
            product: "CORSAIR VOID ELITE Wireless".into(),
            ..Default::default()
        };
        let (mut f, writes) = queue(vec![vec![0x64, 0, 0x80 | 55, 0, 2]]);
        let out = poll_with(&mut f, &[c]);
        assert_eq!(writes.lock().unwrap()[0], [0xC9, 0x64]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].percent, Some(55));
        assert_eq!(out[0].kind, Kind::Headset);
        assert_eq!(out[0].key, "corsair:0A14");
    }

    fn compx_reply(cmd: u8, b6: u8, b7: u8, mv: u16) -> Vec<u8> {
        let mut r = vec![0u8; 17];
        r[0] = 0x08;
        r[1] = cmd;
        r[6] = b6;
        r[7] = b7;
        r[8..10].copy_from_slice(&mv.to_be_bytes());
        r[16] = compx_checksum(&r);
        r
    }

    fn sabre() -> Coll {
        Coll {
            vid: VID,
            pid: 0x2B2A,
            page: 0xFF02,
            usage: 0x0002,
            in_len: 17,
            out_len: 17,
            ..Default::default()
        }
    }

    #[test]
    fn compx_battery() {
        assert_eq!(compx_request(0x04)[16], 0x55 - 0x0C);
        let (mut f, writes) = queue(vec![
            compx_reply(0x03, 1, 0, 0),
            compx_reply(0x04, 81, 1, 4012),
        ]);
        let out = poll_with(&mut f, &[sabre()]);
        assert_eq!(writes.lock().unwrap().len(), 2);
        assert_eq!(writes.lock().unwrap()[1], compx_request(0x04));
        let s = &out[0];
        assert_eq!(
            (s.percent, s.charging, s.millivolts),
            (Some(81), true, Some(4012))
        );
        assert_eq!(s.name, "Corsair Sabre V2 Pro Ultralight");
        // Mouse off: listed nowhere, battery not asked.
        let (mut f, writes) = queue(vec![compx_reply(0x03, 0, 0, 0)]);
        assert!(poll_with(&mut f, &[sabre()]).is_empty());
        assert_eq!(writes.lock().unwrap().len(), 1);
        // A bad checksum is not a reply.
        let mut bad = compx_reply(0x04, 50, 0, 0);
        bad[16] ^= 1;
        let (mut f, _) = queue(vec![bad]);
        assert!(poll_with(&mut f, &[sabre()]).is_empty());
    }

    fn bragi_reply(source: u8, value: u32) -> Vec<u8> {
        let mut r = vec![0x02, source, 0x02, 0x00];
        r.extend(value.to_le_bytes());
        r.resize(64, 0);
        r
    }

    #[test]
    fn bragi_receiver_with_paired_mouse() {
        let c = Coll {
            vid: VID,
            pid: 0x1BDC,
            page: 0xFF42,
            in_len: 65,
            out_len: 65,
            product: "CORSAIR SLIPSTREAM WIRELESS USB Receiver".into(),
            ..Default::default()
        };
        // Target 09: battery 735, product id 1B7E, charge state 1; 0A / 0B / plain framing silent.
        let mut last: Option<Vec<u8>> = None;
        let mut fake = Fake::with(move |_, _, op| match op.clone() {
            Op::Write(w) => {
                assert_eq!(w[0], 0x00, "unnumbered: report id 0 first");
                last = Some(w);
                Some(vec![])
            }
            Op::Flush => Some(vec![]),
            Op::Read(_) => match last.take()?.as_slice() {
                [0x00, 0x02, 0x09, 0x02, 0x0F] => Some(bragi_reply(0x01, 735)),
                [0x00, 0x02, 0x09, 0x02, 0x12] => Some(bragi_reply(0x01, 0x1B7E)),
                [0x00, 0x02, 0x09, 0x02, 0x10] => Some(bragi_reply(0x01, 1)),
                _ => None,
            },
            _ => None,
        });
        let out = poll_with(&mut fake, &[c]);
        assert_eq!(out.len(), 1);
        let s = &out[0];
        assert_eq!(s.name, "Corsair Dark Core RGB Pro SE");
        assert_eq!(s.kind, Kind::Mouse);
        assert_eq!((s.percent, s.charging), (Some(74), true));
        assert_eq!(
            (s.key.as_str(), s.via.as_str()),
            ("corsair:1BDC:9", "Corsair receiver")
        );
    }

    #[test]
    fn bragi_reply_framings() {
        let r = bragi_reply(0x00, 1000);
        assert_eq!(parse_bragi(&r, false, 0x08), Some(Ok(1000)));
        assert_eq!(parse_bragi(&r[1..], true, 0x08), Some(Ok(1000)));
        assert_eq!(parse_bragi(&r, false, 0x09), None, "wrong source");
        let mut e = bragi_reply(0x01, 0);
        e[3] = 0x05;
        assert_eq!(parse_bragi(&e, false, 0x09), Some(Err(5)));
        assert_eq!(bragi_percent(735), 74);
        assert_eq!(bragi_percent(1000), 100);
    }
}

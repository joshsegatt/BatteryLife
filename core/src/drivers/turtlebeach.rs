//! Turtle Beach Stealth 700 Gen 3 headsets (VID 10F5) on the USB transmitter (2251 / 2262) or the
//! headset's own USB cable (2253). Vendor collection FF13:0001 on interface 3: output report 06
//! `06 <len16 LE> <RACE bytes>`, input report 07 `07 <len> 00 <payload>` fetched with GetInputReport.
//!
//! 1. RACE 0x9942 enables the chip's CoAP client (what Swarm II sends on every poll).
//! 2. RACE 0x9902 carrying a CoAP GET /GSI; the answer is a JSON document
//!    `{"OR":"GSI","KVP":{..., "240":"<percent>", ...}}`, split across reports.
//!
//! Nothing else is sent. No charging flag is reported.
//!
//! Ported from BatteryHub (src/drivers/turtlebeach.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0, which credits HaloBattery
//! (providers/turtlebeach.py) and PaulRichez/halobattery-stealth700.

use std::time::Duration;

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::hid::{self, Coll, Dev, Hid, find};

pub const VID: u16 = 0x10F5;
/// (pid, cable); the transmitters first: the same headset is listed once.
const MODELS: &[(u16, bool)] = &[(0x2251, false), (0x2262, false), (0x2253, true)];
pub const PIDS: &[u16] = &[0x2251, 0x2262, 0x2253];

fn send(io: &mut dyn Dev, race: &[u8]) -> bool {
    let mut buf = [0u8; 62];
    buf[0] = 0x06;
    buf[1] = race.len() as u8;
    buf[3..3 + race.len()].copy_from_slice(race);
    io.write(&buf)
}

fn drain(io: &mut dyn Dev, n: usize) {
    for _ in 0..n {
        io.get_input(0x07);
    }
}

/// The newest "240" (battery percent) of a GSI document in the payload stream.
pub fn find_level(s: &[u8]) -> Option<u8> {
    let has = |pat: &[u8]| s.windows(pat.len()).any(|w| w == pat);
    if !has(b"\"GSI\"") {
        return None;
    }
    const KEY: &[u8] = b"\"240\":\"";
    let mut level = None;
    for p in (0..s.len()).filter(|&p| s[p..].starts_with(KEY)) {
        let digits: Vec<u8> = s[p + KEY.len()..]
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .take(4)
            .copied()
            .collect();
        let q = p + KEY.len() + digits.len();
        let v = digits
            .iter()
            .fold(0u32, |v, d| v * 10 + u32::from(d - b'0'));
        if !digits.is_empty() && s.get(q) == Some(&b'"') && v <= 100 {
            level = Some(v as u8);
        }
    }
    level
}

pub fn read_level(io: &mut dyn Dev) -> Option<u8> {
    drain(io, 5);
    if !send(io, &[0x05, 0x5A, 0x02, 0x00, 0x42, 0x99]) {
        return None; // enable the CoAP client
    }
    io.sleep(Duration::from_millis(300)); // the chip needs a moment after the client is enabled
    drain(io, 5);
    let gsi = [
        0x05, 0x5A, 0x0A, 0x00, 0x02, 0x99, 0x40, 0x01, 0x00, 0x01, 0xB3, b'G', b'S', b'I',
    ];
    if !send(io, &gsi) {
        return None;
    }
    let mut payload = Vec::new();
    for _ in 0..30 {
        io.sleep(Duration::from_millis(30));
        let Some(r) = io.get_input(0x07).filter(|r| r.len() >= 3 && r[0] == 0x07) else {
            continue;
        };
        let n = usize::from(r[1]).min(r.len() - 3);
        payload.extend_from_slice(&r[3..3 + n]);
        if let Some(l) = find_level(&payload) {
            return Some(l);
        }
    }
    None
}

/// "Turtle Beach " + the product string (e.g. "Stealth 700X Gen 3").
pub fn model_name(product: &str) -> String {
    if product.is_empty() {
        "Turtle Beach Stealth 700 Gen 3".into()
    } else if product.to_lowercase().starts_with("turtle beach") {
        product.into()
    } else {
        format!("Turtle Beach {product}")
    }
}

pub fn poll_colls(colls: &[Coll], hid: &mut dyn Hid) -> Vec<Device> {
    let mut found: Option<(&Coll, bool)> = None;
    let mut level = None;
    for &(pid, cable) in MODELS {
        let cs = find(colls, VID, pid, |c| {
            c.page == 0xFF13 && c.usage == 0x0001 && c.in_len > 0 && c.out_len > 0
        });
        let Some(&c) = cs.first() else { continue };
        found.get_or_insert((c, cable));
        level = hid.open(c).and_then(|mut io| read_level(io.as_mut()));
        if level.is_some() {
            found = Some((c, cable));
            break;
        }
    }
    let Some((c, cable)) = found else {
        return Vec::new();
    };
    let via = if cable {
        "USB cable"
    } else {
        "2.4 GHz transmitter"
    };
    // Transmitter and cable are one headset.
    let mut d = Device::new(
        "turtlebeach:stealth700",
        model_name(&c.product),
        Kind::Headset,
        via,
    );
    d.grace = Duration::from_secs(300);
    match level {
        Some(l) => d.percent = Some(l),
        // No record (headset off, or Swarm II holding the channel): keep the last value.
        None => d.asleep = true,
    }
    vec![d]
}

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    hid::run(api, infos, |hid, colls| poll_colls(colls, hid))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::hid::fake::{Fake, Op, queued};

    #[test]
    fn gsi_level_parsing() {
        assert_eq!(
            find_level(br#"{"OR":"GSI","KVP":{"240":"85","250":"0"}}"#),
            Some(85)
        );
        assert_eq!(find_level(br#"{"OR":"GSI","KVP":{"240":"101"}}"#), None);
        assert_eq!(find_level(br#"{"OR":"XYZ","KVP":{"240":"85"}}"#), None);
        assert_eq!(find_level(br#""GSI" "240":"7"#), None); // value not closed yet
        assert_eq!(
            find_level(br#""GSI" "240":"60" "240":"59""#),
            Some(59),
            "the newest value wins"
        );
    }

    #[test]
    fn document_split_across_reports() {
        let doc = br#"{"OR":"GSI","KVP":{"240":"72"}}"#;
        let report = |part: &[u8]| {
            let mut r = vec![0x07, part.len() as u8, 0x00];
            r.extend_from_slice(part);
            r.resize(62, 0xEE); // leftovers past the valid length are ignored
            r
        };
        let parts = [report(&doc[..15]), report(&doc[15..])];
        let calls = std::cell::Cell::new(0usize);
        let f = Fake::with(move |s, c, op| {
            let Op::GetInput(_) = op else {
                return queued(s, c, op);
            };
            calls.set(calls.get() + 1);
            // Ten drains first, then the two parts.
            parts.get(calls.get().wrapping_sub(11)).cloned()
        });
        let mut c = Coll::new(VID, 0x2251, 0xFF13, 1, 62, 62);
        c.product = "Stealth 700X Gen 3".into();
        let out = poll_colls(&[c], &mut f.clone());
        assert_eq!(out[0].percent, Some(72));
        assert_eq!(out[0].name, "Turtle Beach Stealth 700X Gen 3");
        assert_eq!(out[0].key, "turtlebeach:stealth700");
        assert_eq!(f.s().writes().len(), 2);
        assert_eq!(
            f.s().writes()[0][..9],
            [0x06, 6, 0, 0x05, 0x5A, 0x02, 0x00, 0x42, 0x99]
        );
    }

    #[test]
    fn no_record_keeps_the_last_value() {
        let f = Fake::new();
        let c = Coll::new(VID, 0x2253, 0xFF13, 1, 62, 62);
        let out = poll_colls(&[c], &mut f.clone());
        assert_eq!((out[0].percent, out[0].asleep), (None, true));
        assert_eq!(out[0].via, "USB cable");
        assert_eq!(model_name(""), "Turtle Beach Stealth 700 Gen 3");
    }
}

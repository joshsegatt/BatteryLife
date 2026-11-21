//! Sony INZONE H9 II (054C:0FA8) and H5 (054C:0EBF) headsets and the INZONE Buds dongle (054C:0EC2):
//! a Sony command layer tunnelled through HID (report 02, 64 bytes) on the vendor collection FF04.
//!   command: [02][len][01][00 FC][8+n][96 C3][addr][event][type][tid lo][tid hi][payload][sum]
//!   event:   [02][len][04][FF][..][00][96 C3][addr][event][type][tid lo][tid hi][payload][sum]
//! Battery = event 04, type 01 (GET) to the headset (addr 0x41).
//!   Headsets: reply payload [charging][percent], percent FF = headset off.
//!   Buds: [status][percent] for left, right, then case; FF = bud in the case. With both buds in the
//!   case the dongle does not respond. No charging flag.
//!
//! Ported from BatteryHub (src/drivers/sony_inzone.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0, which credits
//! Sapd/HeadsetControl, kinako19/inzone-buds-battery-tray and takamachi66/inzone-buds-battery-tray.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::hid::{self, Coll, Dev, Hid, find};

pub const VID: u16 = 0x054C;
pub const PIDS: &[u16] = &[0x0FA8, 0x0EBF, 0x0EC2];

/// Transaction id of the last request; 1 is used by the dongle's own notifications.
static TID: Mutex<u16> = Mutex::new(1);

#[derive(Debug, PartialEq)]
pub struct Event {
    pub id: u8,
    pub kind: u8,
    pub tid: u16,
    pub payload: Vec<u8>,
}

pub fn parse_event(b: &[u8]) -> Option<Event> {
    if b.len() < 15 || b[0] != 0x02 {
        return None;
    }
    let len = usize::from(b[1]);
    if !(12..=62).contains(&len) || b.len() < len + 2 {
        return None;
    }
    if b[2] != 0x04 || b[3] != 0xFF || b[5] != 0x00 || b[6] != 0x96 || b[7] != 0xC3 {
        return None;
    }
    if b[8] >> 4 != 0x1 {
        return None; // must be addressed to the PC
    }
    let sum = b[5..=len].iter().fold(0u8, |s, v| s.wrapping_add(*v));
    if sum != b[len + 1] {
        return None;
    }
    Some(Event {
        id: b[9],
        kind: b[10],
        tid: u16::from_le_bytes([b[11], b[12]]),
        payload: b[13..=len].to_vec(),
    })
}

pub fn request(tid: u16) -> [u8; 64] {
    let mut req = [0u8; 64];
    req[..13].copy_from_slice(&[
        0x02,
        12,
        0x01,
        0x00,
        0xFC,
        8,
        0x96,
        0xC3,
        0x41,
        0x04,
        0x01,
        tid as u8,
        (tid >> 8) as u8,
    ]);
    req[13] = req[6..=12].iter().fold(0u8, |s, v| s.wrapping_add(*v));
    req
}

fn next_tid() -> u16 {
    let mut t = TID.lock().unwrap_or_else(|e| e.into_inner());
    *t = t.wrapping_add(1);
    if *t <= 1 {
        *t = 2;
    }
    *t
}

/// Battery reply payload. None when the write failed or nothing answered (which the buds dongle
/// does with both buds in the case).
fn get_battery(io: &mut dyn Dev, min_payload: usize) -> Option<Vec<u8>> {
    let tid = next_tid();
    io.flush();
    if !io.write(&request(tid)) {
        return None;
    }
    let end = Instant::now() + hid::MAX_WAIT;
    loop {
        let left = end.saturating_duration_since(Instant::now()).as_millis() as u64;
        if left == 0 {
            return None;
        }
        let r = io
            .read(Duration::from_millis(left))
            .filter(|r| r.len() >= 15)?;
        if let Some(e) = parse_event(&r)
            && e.id == 0x04
            && e.tid == tid
            && e.payload.len() >= min_payload
            && (e.kind == 0x10 || e.kind == 0xA0)
        {
            return Some(e.payload); // RET or active notification
        }
    }
}

fn level(v: u8) -> Option<u8> {
    (v <= 100).then_some(v)
}

/// Device from a battery reply payload; None when the headset is off (or both buds are out).
pub fn device(pid: u16, name: &str, buds: bool, p: &[u8]) -> Option<Device> {
    let mut d = Device::new(
        format!("inzone:{pid:04X}"),
        name,
        Kind::Headset,
        "2.4 GHz receiver",
    );
    if buds {
        let (left, right) = (level(p[1]), level(p[3]));
        d.percent = match (left, right) {
            (Some(l), Some(r)) => Some(l.min(r)),
            (l, r) => l.or(r),
        };
        d.percent?;
    } else {
        if p[1] == 0xFF {
            return None;
        }
        d.percent = level(p[1]);
        d.charging = p[0] != 0;
    }
    Some(d)
}

pub fn poll_colls(colls: &[Coll], hid: &mut dyn Hid) -> Vec<Device> {
    // min payload: headsets [charging][percent]; buds 3x [status][percent].
    const MODELS: &[(u16, &str, bool, usize)] = &[
        (0x0FA8, "Sony INZONE H9 II", false, 2),
        (0x0EBF, "Sony INZONE H5", false, 2),
        (0x0EC2, "Sony INZONE Buds", true, 6),
    ];
    let mut out = Vec::new();
    for &(pid, name, buds, min_payload) in MODELS {
        for c in find(colls, VID, pid, |c| c.page == 0xFF04 && c.out_len >= 64) {
            let Some(mut io) = hid.open(c) else { continue };
            // No reply from the buds dongle means both buds are in the case: not listed.
            if let Some(p) = get_battery(io.as_mut(), min_payload) {
                out.extend(device(pid, name, buds, &p));
                break;
            }
        }
    }
    out
}

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    hid::run(api, infos, |hid, colls| poll_colls(colls, hid))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::hid::fake::Fake;

    /// Battery event for `tid` with `payload`, type RET.
    fn event(tid: u16, payload: &[u8]) -> Vec<u8> {
        let len = 12 + payload.len();
        let mut b = vec![0u8; 64];
        b[..13].copy_from_slice(&[
            0x02,
            len as u8,
            0x04,
            0xFF,
            0,
            0x00,
            0x96,
            0xC3,
            0x14,
            0x04,
            0x10,
            tid as u8,
            (tid >> 8) as u8,
        ]);
        b[13..13 + payload.len()].copy_from_slice(payload);
        b[len + 1] = b[5..=len].iter().fold(0u8, |s, v| s.wrapping_add(*v));
        b
    }

    #[test]
    fn request_checksum_and_event_parsing() {
        let r = request(0x0203);
        assert_eq!(
            r[..13],
            [2, 12, 1, 0, 0xFC, 8, 0x96, 0xC3, 0x41, 4, 1, 3, 2]
        );
        let sum = [0x96u8, 0xC3, 0x41, 4, 1, 3, 2]
            .iter()
            .fold(0u8, |s, v| s.wrapping_add(*v));
        assert_eq!(r[13], sum);
        let e = parse_event(&event(7, &[1, 80])).unwrap();
        assert_eq!((e.id, e.kind, e.tid, e.payload), (4, 0x10, 7, vec![1, 80]));
        let mut bad = event(7, &[1, 80]);
        bad[15] ^= 1;
        assert!(parse_event(&bad).is_none());
    }

    #[test]
    fn headset_and_buds_devices() {
        let d = device(0x0FA8, "Sony INZONE H9 II", false, &[1, 80]).unwrap();
        assert_eq!(
            (d.percent, d.charging, d.key.as_str()),
            (Some(80), true, "inzone:0FA8")
        );
        assert!(device(0x0FA8, "H9", false, &[0, 0xFF]).is_none());
        let buds = |l, r| device(0x0EC2, "Buds", true, &[0, l, 0, r, 0, 50]);
        assert_eq!(buds(70, 60).unwrap().percent, Some(60));
        assert_eq!(buds(0xFF, 60).unwrap().percent, Some(60));
        assert!(buds(0xFF, 0xFF).is_none());
    }

    #[test]
    fn poll_answers_the_current_transaction() {
        let f = Fake::replies(|w| {
            let tid = u16::from_le_bytes([w[11], w[12]]);
            vec![event(tid.wrapping_sub(1), &[0, 10]), event(tid, &[0, 42])]
        });
        let c = Coll::new(VID, 0x0EBF, 0xFF04, 1, 64, 64);
        let out = poll_colls(&[c], &mut f.clone());
        assert_eq!(out[0].name, "Sony INZONE H5");
        assert_eq!(out[0].percent, Some(42));
    }
}

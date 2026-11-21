//! ENDORFY Viro Wireless headset on its 2.4 GHz dongle (3738:0072). The dongle has the stock Airoha
//! layout (vendor collection FF13:0001, 62-byte report 06 out / 07 in), so it is read with Airoha's
//! generic RACE "get battery" query: output report 06 `06 07 80 05 5A 03 00 D6 0C 00`; the reply,
//! read with GetInputReport 07, carries RACE frames 05 <type> <u16le len> <u16le opcode> <payload>,
//! and the battery is the 0x5D indication of opcode 0x0CD6 with payload [status 0][role][percent].
//! No charging flag. The query only reads.
//!
//! Ported from BatteryHub (src/drivers/endorfy.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0, which credits HaloBattery
//! (providers/skullcandy.py, providers/audeze.py) and the Headroom project.

use std::time::Duration;

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::hid::{self, Coll, Dev, Hid, find};

pub const VID: u16 = 0x3738;
pub const PID: u16 = 0x0072;
const REPORT_LEN: u16 = 62;
const READS: usize = 12;
const READ_DELAY_MS: u64 = 60; // the answer arrives within ~40 ms on other Airoha dongles

/// The percent carried by an input report 07.
pub fn battery_percent(r: &[u8]) -> Option<u8> {
    if r.len() < 9 || r[0] != 0x07 {
        return None;
    }
    let mut i = 3;
    while i + 6 <= r.len() {
        if r[i] != 0x05 {
            i += 1;
            continue;
        }
        let len = usize::from(u16::from_le_bytes([r[i + 2], r[i + 3]]));
        if len < 2 || i + 4 + len > r.len() {
            break;
        }
        let op = u16::from_le_bytes([r[i + 4], r[i + 5]]);
        if r[i + 1] == 0x5D && op == 0x0CD6 && len >= 5 && r[i + 6] == 0 && r[i + 8] <= 100 {
            return Some(r[i + 8]);
        }
        i += 4 + len;
    }
    None
}

pub fn read_level(io: &mut dyn Dev) -> Option<u8> {
    // Report 07 still holds the previous answer, so a read identical to it is not taken as the new
    // one (except the last read, so a steady level is still shown).
    let before = io.get_input(0x07).unwrap_or_default();
    let mut req = [0u8; REPORT_LEN as usize];
    req[..10].copy_from_slice(&[0x06, 0x07, 0x80, 0x05, 0x5A, 0x03, 0x00, 0xD6, 0x0C, 0x00]);
    if !io.write(&req) {
        return None;
    }
    for i in 0..READS {
        io.sleep(Duration::from_millis(READ_DELAY_MS));
        let Some(r) = io.get_input(0x07) else {
            continue;
        };
        if !before.is_empty() && r == before && i + 1 < READS {
            continue;
        }
        if let Some(l) = battery_percent(&r) {
            return Some(l);
        }
    }
    None
}

pub fn poll_colls(colls: &[Coll], hid: &mut dyn Hid) -> Vec<Device> {
    let found = find(colls, VID, PID, |c| {
        c.page == 0xFF13 && c.usage == 0x0001 && c.in_len == REPORT_LEN && c.out_len == REPORT_LEN
    });
    // Headset off or no answer: left out, the last level stays for the grace period.
    let Some(level) = found
        .first()
        .and_then(|c| hid.open(c))
        .and_then(|mut io| read_level(io.as_mut()))
    else {
        return Vec::new();
    };
    let mut d = Device::new(
        "endorfy:viro",
        "ENDORFY Viro Wireless",
        Kind::Headset,
        "2.4 GHz dongle",
    );
    d.percent = Some(level);
    d.grace = Duration::from_secs(180);
    vec![d]
}

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    hid::run(api, infos, |hid, colls| poll_colls(colls, hid))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::hid::fake::Fake;

    fn report(frames: &[u8]) -> Vec<u8> {
        let mut r = vec![0x07, frames.len() as u8, 0x80];
        r.extend_from_slice(frames);
        r.resize(62, 0);
        r
    }
    const ACK: [u8; 7] = [0x05, 0x5B, 0x03, 0x00, 0xD6, 0x0C, 0x00];
    const BATTERY: [u8; 9] = [0x05, 0x5D, 0x05, 0x00, 0xD6, 0x0C, 0x00, 0x00, 0x46];

    #[test]
    fn indication_after_the_acknowledgement() {
        let mut both = ACK.to_vec();
        both.extend_from_slice(&BATTERY);
        assert_eq!(battery_percent(&report(&both)), Some(70));
        assert_eq!(battery_percent(&report(&ACK)), None);
        let mut bad = BATTERY;
        bad[6] = 1; // status not ok
        assert_eq!(battery_percent(&report(&bad)), None);
    }

    #[test]
    fn stale_report_is_skipped_until_the_new_answer() {
        let old = report(&BATTERY);
        let mut new = BATTERY;
        new[8] = 65;
        let f = Fake::new();
        f.s()
            .input_reports
            .insert(String::new(), vec![old.clone(), old, report(&new)].into());
        let c = Coll::new(VID, PID, 0xFF13, 1, 62, 62);
        let out = poll_colls(&[c], &mut f.clone());
        assert_eq!(out[0].key, "endorfy:viro");
        assert_eq!(out[0].percent, Some(65));
        assert_eq!(out[0].grace, Duration::from_secs(180));
    }
}

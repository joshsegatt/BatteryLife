//! Logitech G435 LIGHTSPEED headset on the newer 046D:0ADF dongle (exact product id, so it beats the
//! whole-vendor HID++ driver). Not HID++: the vendor collection FF00:0001 (unnumbered 64-byte reports)
//! carries CRC-16 framed messages <cmd> <body length> <crc lo> <crc hi> <body>, and "PA" commands
//! inside them reach the headset's radio SoC. Session: open (84 01), 85 01, session init, remote
//! get / set mode 01 / get mode (routes the query to the headset; not a user setting and not stored),
//! OTA GET_BATTERY (millivolts), close (85 02).
//!
//! Ported from BatteryHub (src/drivers/logitech_g435_0adf.cpp and .h, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0, which credits
//! portyporty/g435-battery-percentage and deddroid/logitech-g435-battery.

use std::time::{Duration, Instant};

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::hid::{self, Coll, Dev, Hid, crc16_xmodem, find};

pub const VID: u16 = 0x046D;
pub const PID: u16 = 0x0ADF;
const CAT_OTA: u8 = 0x06;
const CAT_WIRELESS: u8 = 0x0E;
const GET_MODE: u8 = 0xE0;
const SET_MODE: u8 = 0xE1;

/// Session init from the capture (OTA_CMD_IOCTL_READ_MAX_LEN, sequence 1 embedded).
const SESSION_INIT: [u8; 19] = [
    0x50, 0x41, 0x06, 0x01, 0x0D, 0x00, 0x25, 0x34, 0x12, 0x5A, 0x5A, 0x01, 0x00, 0x00, 0x00, 0xF0,
    0x00, 0x00, 0x00,
];

/// 64-byte frame: <cmd> <body length> <crc lo> <crc hi> <body>, zero-padded; the CRC covers the
/// whole frame with the CRC field zeroed.
pub fn frame(cmd: u8, body: &[u8]) -> [u8; 64] {
    let mut f = [0u8; 64];
    f[0] = cmd;
    f[1] = body.len() as u8;
    let n = body.len().min(60);
    f[4..4 + n].copy_from_slice(&body[..n]);
    let crc = crc16_xmodem(&f);
    f[2..4].copy_from_slice(&crc.to_le_bytes());
    f
}

/// "PA" command: 50 41 <category> <seq> <length field> <payload>.
pub fn pa(category: u8, seq: u8, len: u8, payload: &[u8]) -> Vec<u8> {
    let mut cmd = vec![0x50, 0x41, category, seq, len];
    cmd.extend_from_slice(payload);
    cmd
}

/// Battery voltage from the GET_BATTERY reply (00 07 00 06 <seq> 00 <mV lo> <mV hi>); only a
/// plausible Li-Po voltage counts.
pub fn parse_millivolts(data: &[u8]) -> Option<u16> {
    data.windows(8)
        .filter(|w| w[0] == 0x00 && w[1] == 0x07 && w[2] == 0x00 && w[3] == 0x06 && w[5] == 0x00)
        .map(|w| u16::from_le_bytes([w[6], w[7]]))
        .find(|mv| (2500..=4500).contains(mv))
}

/// Voltage -> % for the G435 Li-Po cell (portyporty's curve), piecewise-linear with rounding.
pub fn percent(mv: i32) -> u8 {
    const V: [i32; 16] = [
        4200, 4150, 4100, 4050, 4000, 3950, 3900, 3850, 3800, 3750, 3700, 3650, 3600, 3550, 3500,
        3400,
    ];
    const P: [i32; 16] = [
        100, 95, 90, 85, 80, 75, 70, 62, 55, 45, 35, 25, 15, 10, 5, 0,
    ];
    if mv >= V[0] {
        return 100;
    }
    for i in 1..16 {
        if mv >= V[i] {
            let (dv, dp) = (V[i - 1] - V[i], P[i - 1] - P[i]);
            return (P[i] + (dp * (mv - V[i]) + dv / 2) / dv) as u8;
        }
    }
    0
}

/// Sends one frame (report id 0) and takes the input reports arriving within `wait_ms`; with
/// `want_mv`, returns as soon as a battery reply is seen. Err when the frame was not sent.
fn transact(
    io: &mut dyn Dev,
    cmd: u8,
    body: &[u8],
    wait_ms: u64,
    want_mv: bool,
) -> Result<Option<u16>, ()> {
    let mut out = vec![0x00];
    out.extend_from_slice(&frame(cmd, body));
    io.flush();
    if !io.write(&out) {
        return Err(());
    }
    let end = Instant::now() + Duration::from_millis(wait_ms);
    loop {
        let left = end.saturating_duration_since(Instant::now()).as_millis() as u64;
        if left == 0 {
            return Ok(None);
        }
        if let Some(r) = io.read(Duration::from_millis(left))
            && want_mv
            && let Some(mv) = parse_millivolts(&r)
        {
            return Ok(Some(mv));
        }
    }
}

fn read_millivolts(io: &mut dyn Dev) -> Option<u16> {
    transact(io, 0x84, &[0x01], 50, false).ok()?;
    let _ = transact(io, 0x85, &[0x01], 50, false);
    let _ = transact(io, 0x80, &SESSION_INIT, 450, false);
    let mut seq = 2u8;
    let mut next = |cat: u8, len: u8, payload: &[u8]| {
        let cmd = pa(cat, seq, len, payload);
        seq = seq % 127 + 1;
        cmd
    };
    let steps = [
        next(CAT_WIRELESS, 1, &[GET_MODE]),
        next(CAT_WIRELESS, 2, &[SET_MODE, 0x01]),
        next(CAT_WIRELESS, 1, &[GET_MODE]),
    ];
    let battery = next(CAT_OTA, 1, &[0x00, 0x31]);
    for s in &steps {
        let _ = transact(io, 0x80, s, 120, false);
    }
    let mv = transact(io, 0x80, &battery, 250, true).ok().flatten();
    // The session is always closed once it was opened.
    let _ = transact(io, 0x85, &[0x02], 50, false);
    mv
}

pub fn poll_colls(colls: &[Coll], hid: &mut dyn Hid) -> Vec<Device> {
    let found = find(colls, VID, PID, |c| {
        c.page == 0xFF00 && c.usage == 0x0001 && c.in_len == 65 && c.out_len == 65
    });
    let Some(mut io) = found.first().and_then(|c| hid.open(c)) else {
        return Vec::new();
    };
    // Dongle present but no reply: the headset is off or out of range.
    let Some(mv) = read_millivolts(io.as_mut()) else {
        return Vec::new();
    };
    let mut d = Device::new(
        "g435:0ADF",
        "Logitech G435",
        Kind::Headset,
        "2.4 GHz receiver",
    );
    d.millivolts = Some(mv);
    d.percent = Some(percent(i32::from(mv)));
    vec![d]
}

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    hid::run(api, infos, |hid, colls| poll_colls(colls, hid))
}

#[cfg(test)]
mod tests {
    // tests/logitech_g435_0adf_test.cpp
    use super::*;

    #[test]
    fn frame_layout_and_crc() {
        let f = frame(0x84, &[0x01]);
        assert_eq!((f.len(), f[0], f[1], f[4], f[5]), (64, 0x84, 1, 0x01, 0));
        let mut zeroed = f;
        zeroed[2] = 0;
        zeroed[3] = 0;
        assert_eq!(crc16_xmodem(&zeroed).to_le_bytes(), [f[2], f[3]]);
    }

    #[test]
    fn get_battery_command() {
        assert_eq!(
            pa(0x06, 5, 1, &[0x00, 0x31]),
            vec![0x50, 0x41, 0x06, 0x05, 0x01, 0x00, 0x31]
        );
    }

    #[test]
    fn battery_reply() {
        let mut reply = vec![0u8; 64];
        let body = [0x50, 0x49, 0x00, 0x07, 0x00, 0x06, 0x85, 0x00, 0xC4, 0x0E];
        reply[10..20].copy_from_slice(&body);
        assert_eq!(parse_millivolts(&reply), Some(3780));
        // Implausible voltage (dongle answering itself) and missing reply are rejected.
        reply[18] = 0xD2;
        reply[19] = 0x00;
        assert_eq!(parse_millivolts(&reply), None);
        assert_eq!(parse_millivolts(&[0u8; 64]), None);
    }

    #[test]
    fn voltage_curve() {
        assert_eq!((percent(4250), percent(4200)), (100, 100));
        assert_eq!((percent(3800), percent(3775)), (55, 50));
        assert_eq!((percent(3400), percent(3000)), (0, 0));
    }
}

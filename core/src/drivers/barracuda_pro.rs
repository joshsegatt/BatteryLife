//! Razer Barracuda Pro on its 2.4 GHz receiver (1532:053A): the Razer "PA" protocol, 64-byte
//! reports with report id 01.
//!   query:  [01][80][08]['P']['A'][08][08][03][cmd]       cmd 21 = battery %, 2A = charging
//!   remote: [01][80][07]['P']['A'][0E][08][02][E1][1/0]   on for one query and off again, as Synapse
//!           does; it is not stored on the headset
//!   reply:  [13] = command echo, [14] = 01/02, [15] = length, [16..] = data
//!
//! Ported from BatteryHub (src/drivers/barracuda_pro.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0, which credits HaloBattery
//! (providers/barracuda.py).

use std::time::Duration;

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::hid::{self, Coll, Dev, Hid, find};

pub const VID: u16 = 0x1532;
pub const PID: u16 = 0x053A;

pub fn query_frame(cmd: u8) -> [u8; 64] {
    let mut b = [0u8; 64];
    b[..9].copy_from_slice(&[0x01, 0x80, 0x08, 0x50, 0x41, 0x08, 0x08, 0x03, cmd]);
    b
}

pub fn remote_frame(on: bool) -> [u8; 64] {
    let mut b = [0u8; 64];
    b[..10].copy_from_slice(&[
        0x01,
        0x80,
        0x07,
        0x50,
        0x41,
        0x0E,
        0x08,
        0x02,
        0xE1,
        u8::from(on),
    ]);
    b
}

/// First data byte of a reply to `cmd`.
pub fn reply_value(r: &[u8], cmd: u8) -> Option<u8> {
    (r.len() >= 17 && r[13] == cmd && (r[14] == 0x01 || r[14] == 0x02) && (1..=32).contains(&r[15]))
        .then(|| r[16])
}

fn drain(io: &mut dyn Dev) {
    for _ in 0..8 {
        if io
            .read(Duration::from_millis(20))
            .is_none_or(|r| r.is_empty())
        {
            break;
        }
    }
}

fn ask(io: &mut dyn Dev, cmd: u8) -> Option<u8> {
    for _ in 0..4 {
        drain(io);
        if !io.write(&query_frame(cmd)) {
            return None;
        }
        for _ in 0..6 {
            if let Some(v) = io
                .read(Duration::from_millis(150))
                .and_then(|r| reply_value(&r, cmd))
            {
                return Some(v);
            }
        }
    }
    None
}

pub fn poll_colls(colls: &[Coll], hid: &mut dyn Hid) -> Vec<Device> {
    let colls = find(colls, VID, PID, |c| c.page == 0xFF00 && c.out_len >= 64);
    let Some(c) = colls.first() else {
        return Vec::new();
    };
    let Some(mut io) = hid.open(c) else {
        return Vec::new();
    };
    let io = io.as_mut();
    let mut remote = false;
    for attempt in 0..3 {
        drain(io);
        remote = io.write(&remote_frame(true));
        if remote {
            break;
        }
        io.sleep(Duration::from_millis(150 * (attempt + 1)));
    }
    let (mut level, mut charging) = (None, None);
    if remote {
        io.sleep(Duration::from_millis(50));
        level = ask(io, 0x21);
        if level.is_some() {
            charging = ask(io, 0x2A);
        }
        drain(io);
        io.write(&remote_frame(false));
    }
    let mut d = Device::new(
        "barracuda:053A",
        "Razer Barracuda Pro",
        Kind::Headset,
        "2.4 GHz receiver",
    );
    // No level: receiver present, headset off or out of range.
    let Some(level) = level.filter(|l| *l <= 100) else {
        d.off = true;
        return vec![d];
    };
    d.percent = Some(level);
    d.charging = charging.is_some_and(|c| c > 0);
    vec![d]
}

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    hid::run(api, infos, |hid, colls| poll_colls(colls, hid))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::hid::fake::Fake;

    fn reply(cmd: u8, value: u8) -> Vec<u8> {
        let mut r = vec![0u8; 64];
        r[..5].copy_from_slice(&[0x01, 0x80, 0x0A, 0x50, 0x49]);
        r[13] = cmd;
        r[14] = 0x02;
        r[15] = 1;
        r[16] = value;
        r
    }

    #[test]
    fn frames_and_reply() {
        assert_eq!(
            query_frame(0x21)[..9],
            [1, 0x80, 8, b'P', b'A', 8, 8, 3, 0x21]
        );
        assert_eq!(remote_frame(false)[9], 0);
        assert_eq!(reply_value(&reply(0x21, 77), 0x21), Some(77));
        assert_eq!(reply_value(&reply(0x21, 77), 0x2A), None);
        let mut bad = reply(0x21, 77);
        bad[15] = 0;
        assert_eq!(reply_value(&bad, 0x21), None);
    }

    #[test]
    fn reads_level_and_charging_inside_remote_mode() {
        let f = Fake::replies(|w| match (w[7], w[8]) {
            (0x03, cmd) => vec![reply(cmd, if cmd == 0x21 { 62 } else { 1 })],
            _ => vec![],
        });
        let c = Coll::new(VID, PID, 0xFF00, 1, 64, 64);
        let out = poll_colls(&[c], &mut f.clone());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].key, "barracuda:053A");
        assert_eq!((out[0].percent, out[0].charging), (Some(62), true));
        let writes = f.s().writes().clone();
        assert_eq!(writes.first().unwrap()[..10], remote_frame(true)[..10]);
        assert_eq!(writes.last().unwrap()[..10], remote_frame(false)[..10]);
    }

    #[test]
    fn headset_off_is_listed_switched_off() {
        let f = Fake::new();
        let c = Coll::new(VID, PID, 0xFF00, 1, 64, 64);
        let out = poll_colls(&[c], &mut f.clone());
        assert_eq!((out.len(), out[0].off, out[0].percent), (1, true, None));
        assert_eq!(out[0].key, "barracuda:053A");
    }
}

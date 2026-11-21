//! AULA X9 Ultra (and V9 mice) on the "AULA-8K Dongle" receiver 320F:22A0, FF1C:0092 on interface 1,
//! 64-byte reports with report id 04. One read-state transaction: START (01), battery (1A), END
//! (02); the battery reply carries [8] percent and [9] 0 battery / 1 charging / 2 full. The
//! receiver caches the level; a pushed AA link-down frame ([8] = 0) marks the mouse asleep, and a
//! zero level is an uninitialised cache. No settings, pairing or firmware commands are sent.
//! 320F is shared with other brands, so only 22A0 is declared, and its product string is checked.
//!
//! Ported from BatteryHub (src/drivers/aula_mouse.cpp and aula_mouse_protocol.h, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0, which cites
//! github.com/HeyOkay/HaloBattery issue #325 and github.com/ahmedkhursheed23/HaloBattery
//! (providers/lofree.py).

use std::time::{Duration, Instant};

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::hid_dev::{Coll, Dev, HidIo, Io, colls, find};
use crate::registry::Match;

const VID: u16 = 0x320F;
const PID: u16 = 0x22A0;

pub static MATCHES: &[Match] = &[Match {
    vid: VID,
    pids: &[PID],
}];

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    poll_with(&colls(infos), &mut HidIo(api))
}

pub fn request(cmd: u8) -> [u8; 64] {
    let mut r = [0u8; 64];
    r[0] = 4;
    r[3] = cmd;
    r
}

pub fn is_heartbeat(r: &[u8]) -> bool {
    r.len() >= 9
        && r[0] == 4
        && r[1] == 0xAA
        && r[2] == 0
        && r[3] == 0xAA
        && (r[8] == 0 || r[8] == 0xFF)
}

pub fn is_reply(r: &[u8], cmd: u8) -> bool {
    r.len() >= 8 && r[0] == 4 && r[1..3] == [0, 0] && r[3] == cmd && r[4..8] == [0, 0, 0, 0]
}

/// (percent, charging) from a battery (1A) reply.
pub fn parse_battery(r: &[u8]) -> Option<(u8, bool)> {
    (r.len() >= 10 && is_reply(r, 0x1A) && (1..=100).contains(&r[8]) && r[9] <= 2)
        .then(|| (r[8], r[9] != 0))
}

/// Waits for the reply to `cmd` until `end`, noting link-down heartbeats on the way.
fn wait_reply(d: &mut dyn Dev, cmd: u8, end: Instant, link_down: &mut bool) -> Option<Vec<u8>> {
    while Instant::now() < end {
        let left = end.saturating_duration_since(Instant::now()).as_millis() as u64;
        if left == 0 {
            break;
        }
        let Some(r) = d.read(left.min(100)) else {
            continue;
        };
        if is_heartbeat(&r) {
            *link_down = r[8] == 0;
            continue;
        }
        if is_reply(&r, cmd) && (cmd != 0x1A || r.len() >= 10) {
            return Some(r);
        }
    }
    None
}

/// One bounded transaction (1 s including cleanup). Some((percent, charging)) on a valid reading.
fn read_battery(d: &mut dyn Dev, link_down: &mut bool) -> Option<(u8, bool)> {
    d.flush();
    let end = Instant::now() + Duration::from_millis(1000);
    if !d.write(&request(1)) {
        return None;
    }
    let started = wait_reply(d, 1, end - Duration::from_millis(550), link_down).is_some();
    let mut valid = None;
    if started
        && d.write(&request(0x1A))
        && let Some(r) = wait_reply(d, 0x1A, end - Duration::from_millis(200), link_down)
    {
        valid = parse_battery(&r);
    }
    // End even after a missing acknowledgement, to leave no transaction open.
    if d.write(&request(2)) {
        wait_reply(d, 2, end, link_down);
    }
    valid
}

fn poll_with(colls: &[Coll], io: &mut dyn Io) -> Vec<Device> {
    for c in find(colls, VID, PID, |c| {
        c.iface == 1 && c.page == 0xFF1C && c.usage == 0x0092 && c.in_len == 64 && c.out_len == 64
    }) {
        if c.product != "AULA-8K Dongle" {
            continue;
        }
        let mut s = Device::new(
            "aula_mouse:320F:22A0",
            "AULA wireless mouse (X9 / V9)",
            Kind::Mouse,
            "2.4 GHz receiver",
        );
        s.grace = Duration::from_secs(1800);
        let mut link_down = false;
        match io
            .open(c)
            .and_then(|mut d| read_battery(d.as_mut(), &mut link_down))
        {
            Some((pct, charging)) if !link_down => {
                s.percent = Some(pct);
                s.charging = charging;
            }
            _ => s.asleep = true,
        }
        return vec![s];
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::hid_dev::fake::{Fake, Sent, coll};
    use std::sync::{Arc, Mutex};

    fn dongle() -> Coll {
        let mut c = coll(VID, PID, 1, 0xFF1C, 0x92, 64, 64, 0);
        c.product = "AULA-8K Dongle".into();
        c
    }

    /// The receiver of aula_mouse_driver_test: echoes START / END, answers 1A after short frames.
    fn receiver(battery: Arc<Mutex<Vec<u8>>>, down: bool, silent: bool) -> Fake {
        Fake::with(move |s, _, kind, q| {
            assert_eq!(kind, Sent::Write);
            assert!(q.len() == 64 && q[0] == 4 && [1, 2, 0x1A].contains(&q[3]));
            if silent {
                return;
            }
            if q[3] == 0x1A {
                // Short battery frames must not hide a complete reply queued after them.
                s.push_input("", vec![4, 0, 0, 0x1A]);
                s.push_input("", vec![4, 0, 0, 0x1A, 0, 0, 0, 0]);
                s.push_input("", vec![4, 0, 0, 0x1A, 0, 0, 0, 0, 86]);
                if down {
                    s.push_input("", vec![4, 0xAA, 0, 0xAA, 0, 0, 0, 0, 0]);
                }
                s.push_input("", battery.lock().unwrap().clone());
            } else {
                s.push_input("", q.to_vec());
            }
        })
    }

    fn battery(level: u8, charge: u8) -> Arc<Mutex<Vec<u8>>> {
        let mut b = request(0x1A).to_vec();
        b[8] = level;
        b[9] = charge;
        Arc::new(Mutex::new(b))
    }

    #[test]
    fn reads_battery_in_one_transaction() {
        let mut f = receiver(battery(86, 0), false, false);
        let out = poll_with(&[dongle()], &mut f);
        assert_eq!(out.len(), 1);
        assert_eq!((out[0].percent, out[0].charging), (Some(86), false));
        assert_eq!(out[0].kind, Kind::Mouse);
        let w = f.s().sent_of(Sent::Write);
        assert_eq!(w.iter().map(|q| q[3]).collect::<Vec<_>>(), [1, 0x1A, 2]);
        for charge in [1, 2] {
            let out = poll_with(
                &[dongle()],
                &mut receiver(battery(86, charge), false, false),
            );
            assert!(out[0].charging);
        }
    }

    #[test]
    fn invalid_levels_and_link_down_are_asleep() {
        for (level, charge) in [(0, 0), (101, 0), (255, 0), (86, 3)] {
            let out = poll_with(
                &[dongle()],
                &mut receiver(battery(level, charge), false, false),
            );
            assert!(out[0].asleep && out[0].percent.is_none());
        }
        let out = poll_with(&[dongle()], &mut receiver(battery(86, 0), true, false));
        assert!(out[0].asleep && out[0].percent.is_none());
    }

    #[test]
    fn matching_open_failure_and_timeout() {
        for i in 0..6 {
            let mut c = dongle();
            match i {
                0 => c.vid += 1,
                1 => c.pid += 1,
                2 => c.iface += 1,
                3 => c.page += 1,
                4 => c.usage += 1,
                _ => c.out_len = 32,
            }
            let mut f = receiver(battery(86, 0), false, false);
            assert!(poll_with(&[c], &mut f).is_empty());
            assert_eq!(f.s().opens, 0);
        }
        let mut f = receiver(battery(86, 0), false, false);
        f.s().refuse_open = true;
        let out = poll_with(&[dongle()], &mut f);
        assert!(f.s().sent.is_empty() && out[0].asleep);

        let mut f = receiver(battery(86, 0), false, true);
        let start = Instant::now();
        let out = poll_with(&[dongle()], &mut f);
        assert!(start.elapsed() < Duration::from_millis(1200) && out[0].asleep);
        let w = f.s().sent_of(Sent::Write);
        assert_eq!(w.len(), 2);
        assert_eq!(w[1][3], 2, "END is sent even without a START reply");
    }
}

//! Mice on the shared Kysona / CompX protocol: Redragon (M916 Pro 1K, M913 tri-mode, G49; Taidu
//! TSG808) and VXE Dragonfly R1 on 3554, ATK on 373B, the original Hitscan Hyperlight (3770),
//! G-Wolves Fenrir Asym (33E4), Pulsar's XS-1 8K dongle (3710:5406) and HECATE mice (35BB).
//! 17-byte output / input report 08: [08][cmd][0 x14][checksum = 0x55 - sum(bytes 0..15)]
//!   cmd 03 = online status -> [6] = 1 when the mouse is linked to the dongle
//!   cmd 04 = battery       -> [6] percent, [7] charging, [8..9] millivolts (big-endian)
//!
//! FF02:0002 is tried first where it exists. These vendor ids are shared with Chimera, AULA, ATK,
//! Pulsar, Glorious and Hitscan drivers, so only the product ids BatteryHub names for this protocol
//! are declared (BatteryHub itself scans every unclaimed product of these vendors).
//!
//! Ported from BatteryHub (src/drivers/kysona.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0, which cites Linux
//! drivers/hid/hid-kysona.c (Lode Willems), github.com/S1berla/vxe-r1-pro-max-battery,
//! github.com/orcunturkol/BatteryTray, github.com/EliottV17/redragon-battery-omarchy and
//! github.com/pingpingjinping/g49-web-control.

use std::time::{Duration, Instant};

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::hid::{self, BUDGET, Coll, Dev, Hid, find, hex4, kind_from_colls, left};
use crate::registry::Match;

const REDRAGON: [u16; 3] = [0xF55D, 0xF5D5, 0xF55E];
const VXE_R1: [u16; 6] = [0xF58A, 0xF58C, 0xF58E, 0xF58F, 0xF505, 0xF506];

pub static MATCHES: &[Match] = &[
    Match {
        vid: 0x3554,
        pids: &[
            0xF55D, 0xF5D5, 0xF55E, 0xF58A, 0xF58C, 0xF58E, 0xF58F, 0xF505, 0xF506,
        ],
    },
    // "CX Wireless mouse", the example output in BatteryHub's README.
    Match {
        vid: 0x373B,
        pids: &[0x1085],
    },
    // Hitscan Hyperlight, verified on the 1K receiver.
    Match {
        vid: 0x3770,
        pids: &[0x0200],
    },
    // G-Wolves Fenrir Asym, the model kysona.cpp names; other G-Wolves ids use Glorious' protocol.
    Match {
        vid: 0x33E4,
        pids: &[0x3519],
    },
    // Pulsar XS-1 generation 8K dongle, verified (the X2 V3 ids belong to pulsar.cpp).
    Match {
        vid: 0x3710,
        pids: &[0x5406],
    },
    // HECATE Connect's mouse ids only; its headsets on 35BB speak something else.
    Match {
        vid: 0x35BB,
        pids: &[0xD3FA, 0xD3FB, 0xD3FC, 0xD3FD],
    },
];

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    hid::run(api, infos, |hid, colls| poll_with(colls, hid))
}

pub fn checksum(b: &[u8]) -> u8 {
    0x55u8.wrapping_sub(b[..16].iter().fold(0u8, |a, x| a.wrapping_add(*x)))
}

pub fn request(cmd: u8) -> [u8; 17] {
    let mut q = [0u8; 17];
    q[0] = 0x08;
    q[1] = cmd;
    q[16] = checksum(&q);
    q
}

pub fn is_reply(r: &[u8], cmd: u8) -> bool {
    r.len() >= 17 && r[0] == 0x08 && r[1] == cmd && r[16] == checksum(r)
}

/// A VXE R1 cable reports under its receiver's id, so the mouse is one entry on either link.
fn key_pid(vid: u16, pid: u16) -> u16 {
    match (vid, pid) {
        (0x3554, 0xF58C) => 0xF58A,
        (0x3554, 0xF58F) => 0xF58E,
        (0x3554, 0xF506) => 0xF505,
        _ => pid,
    }
}

fn is_cable(vid: u16, pid: u16) -> bool {
    // G-Wolves' wired ids (glorious.cpp's GWolvesIsCable) include 3519.
    (vid == 0x33E4 && pid == 0x3519)
        || (vid == 0x35BB && [0xD3FA, 0xD3FC, 0xD3FD].contains(&pid))
        || (vid == 0x3554 && [0xF55E, 0xF58C, 0xF58F, 0xF506].contains(&pid))
}

pub fn clean_name(product: &str, vid: u16, pid: u16) -> String {
    if vid == 0x33E4 {
        return if pid == 0x3519 {
            "G-Wolves Fenrir Asym"
        } else {
            "G-Wolves wireless mouse"
        }
        .into();
    }
    if vid == 0x3554 && REDRAGON.contains(&pid) {
        return "Redragon wireless mouse".into(); // shared by several models
    }
    if vid == 0x3554 && VXE_R1.contains(&pid) {
        return "VXE Dragonfly R1".into(); // the receivers call themselves "VXE NordicMouse 1K Dongle"
    }
    let mut n = product.to_string();
    for suffix in [" Dongle", " Receiver", " 2.4G", " Wireless Receiver"] {
        if n.len() > suffix.len() && n.ends_with(suffix) {
            n.truncate(n.len() - suffix.len());
        }
    }
    if vid == 0x3770 {
        return "Hitscan Hyperlight".into(); // the receiver only calls itself "Hitscan USB Receiver"
    }
    if vid == 0x35BB {
        return "HECATE mouse".into();
    }
    if n.is_empty() {
        n = if vid == 0x373B {
            "ATK wireless mouse"
        } else {
            "Wireless mouse"
        }
        .into();
    }
    n
}

fn query(d: &mut dyn Dev, cmd: u8, end: Instant) -> Option<Vec<u8>> {
    d.flush();
    if !d.write(&request(cmd)) {
        return None;
    }
    let end = end.min(Instant::now() + Duration::from_millis(500));
    loop {
        let left = left(end);
        if left.is_zero() {
            return None;
        }
        let r = d.read(left).filter(|r| r.len() >= 17)?;
        if is_reply(&r, cmd) {
            return Some(r);
        }
    }
}

enum Outcome {
    /// The dongle answered: the device, or None when the mouse is off (nothing shown).
    Answered(Option<Device>),
    Silent,
}

fn read_one(d: &mut dyn Dev, c: &Coll, colls: &[Coll], end: Instant) -> Outcome {
    let (vid, pid) = (c.vid, c.pid);
    let mut answered = false;
    let mut connected = true;
    if let Some(r) = query(d, 0x03, end) {
        answered = true;
        connected = r[6] != 0;
    }
    let mut s = Device::new(
        format!("kysona:{}:{}", hex4(vid), hex4(key_pid(vid, pid))),
        clean_name(&c.product, vid, pid),
        match kind_from_colls(colls, vid, pid) {
            Kind::Other => Kind::Mouse,
            k => k,
        },
        if is_cable(vid, pid) {
            "USB cable"
        } else {
            "2.4 GHz receiver"
        },
    );
    s.grace = Duration::from_secs(1800);
    let battery = if !answered || connected {
        query(d, 0x04, end)
    } else {
        None
    };
    if let Some(r) = battery {
        answered = true;
        connected = true;
        s.percent = Some(r[6]).filter(|p| *p <= 100);
        s.charging = r[7] != 0;
        s.millivolts = Some(u16::from_be_bytes([r[8], r[9]])).filter(|v| *v != 0);
    } else if answered && connected {
        // The receiver says online but the mouse did not answer: keep the last value.
        s.asleep = true;
    }
    match (answered, connected) {
        (false, _) => Outcome::Silent,
        (true, false) => Outcome::Answered(None),
        (true, true) => Outcome::Answered(Some(s)),
    }
}

fn poll_with(colls: &[Coll], io: &mut dyn Hid) -> Vec<Device> {
    let end = Instant::now() + BUDGET;
    let mut out: Vec<Device> = Vec::new();
    for m in MATCHES {
        for &pid in m.pids {
            let mut cs = find(colls, m.vid, pid, |c| {
                (c.out_len == 17 && c.in_len == 17)
                    || (c.page >= 0xFF00 && c.out_len >= 17 && c.in_len >= 17)
            });
            // The battery channel is FF02:0002 where it exists.
            cs.sort_by_key(|c| !(c.page == 0xFF02 && c.usage == 2));
            for c in cs {
                if left(end).is_zero() {
                    return out;
                }
                let Some(mut d) = io.open(c) else { continue };
                match read_one(d.as_mut(), c, colls, end) {
                    Outcome::Silent => continue,
                    Outcome::Answered(None) => {}
                    Outcome::Answered(Some(s)) => {
                        // A mouse plugged in for charging can show up twice (cable + dongle).
                        let mut dup = false;
                        for o in out.iter_mut().filter(|o| o.name == s.name) {
                            dup = true;
                            o.charging |= s.charging;
                        }
                        if !dup {
                            out.push(s);
                        }
                    }
                }
                break;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::hid::fake::{self, Fake, iface_coll as coll};

    fn reply(cmd: u8, b6: u8, b7: u8) -> Vec<u8> {
        let mut r = vec![0u8; 17];
        r[0] = 8;
        r[1] = cmd;
        r[6] = b6;
        r[7] = b7;
        if cmd == 4 {
            r[8] = 0x0F;
            r[9] = 0x6E;
        }
        r[16] = checksum(&r);
        r
    }

    fn device(online: u8, battery: Option<(u8, u8)>) -> Fake {
        Fake::with(move |s, fc, op| {
            if let Some(q) = op.sent() {
                assert!(q == request(3) || q == request(4));
                match q[1] {
                    3 => s.push_input("", reply(3, online, 0)),
                    _ => {
                        if let Some((p, c)) = battery {
                            s.push_input("", reply(4, p, c));
                        }
                    }
                }
            }
            fake::queued(s, fc, op)
        })
    }

    fn vxe(pid: u16) -> Coll {
        let mut c = coll(0x3554, pid, 1, 0xFF02, 2, 17, 17, 0);
        c.product = "VXE NordicMouse 1K Dongle".into();
        c
    }

    #[test]
    fn request_checksum() {
        assert_eq!(request(4)[16], 0x55 - 8 - 4);
        assert!(is_reply(&reply(4, 50, 0), 4));
        let mut bad = reply(4, 50, 0);
        bad[16] ^= 1;
        assert!(!is_reply(&bad, 4));
    }

    #[test]
    fn reads_battery_and_millivolts() {
        let out = poll_with(&[vxe(0xF58A)], &mut device(1, Some((76, 1))));
        assert_eq!(out.len(), 1);
        let s = &out[0];
        assert_eq!(
            (s.percent, s.charging, s.millivolts),
            (Some(76), true, Some(3950))
        );
        assert_eq!(
            (s.name.as_str(), s.key.as_str()),
            ("VXE Dragonfly R1", "kysona:3554:F58A")
        );
        assert_eq!(s.via, "2.4 GHz receiver");
    }

    #[test]
    fn cable_shares_the_receiver_key() {
        let out = poll_with(&[vxe(0xF58C)], &mut device(1, Some((76, 1))));
        assert_eq!(
            (out[0].key.as_str(), out[0].via.as_str()),
            ("kysona:3554:F58A", "USB cable")
        );
    }

    #[test]
    fn offline_and_asleep() {
        let mut f = device(0, Some((76, 0)));
        assert!(poll_with(&[vxe(0xF58A)], &mut f).is_empty());
        assert_eq!(
            f.s().sends(),
            1,
            "no battery query when the dongle says offline"
        );
        let out = poll_with(&[vxe(0xF58A)], &mut device(1, None));
        assert!(out[0].asleep && out[0].percent.is_none());
    }

    #[test]
    fn names() {
        assert_eq!(
            clean_name("ATK X1 Wireless Receiver", 0x373B, 0x1085),
            "ATK X1 Wireless"
        );
        assert_eq!(clean_name("", 0x373B, 0x1085), "ATK wireless mouse");
        assert_eq!(
            clean_name("Hitscan USB Receiver", 0x3770, 0x0200),
            "Hitscan Hyperlight"
        );
        assert_eq!(
            clean_name("2.4G Wireless Receiver", 0x3554, 0xF55D),
            "Redragon wireless mouse"
        );
        assert_eq!(clean_name("Pulsar 8K Dongle", 0x3710, 0x5406), "Pulsar 8K");
    }
}

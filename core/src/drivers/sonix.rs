//! SONiX keyboards on their 2.4 GHz dongles: AJAZZ AK820 MAX and AULA F108 Pro (05AC:024F, Apple's
//! vendor id; probed from `aula.rs`, which claims that id), AJAZZ AK980 V2 (0C45:FEFE), AJAZZ AK820 Pro
//! (0C45:FDFD), and the 342D / 388D / 0DB0 / 36B0 / 19F5 / 362D boards listed below.
//!
//! Vendor collection FF60:0061 with 33-byte reports (report id 0 + 32 bytes); interface 3 on the
//! 05AC / 0C45 dongles, any interface on the others.
//!   request [20 01 00 ... sum]   sum = byte sum of the 31 bytes before it
//!   reply   [20 01 00 level ...] level in percent; 0 = keyboard off, out of range or in another mode
//! The reply is recognised by its echoed first three bytes. Read-only.
//!
//! Ported from BatteryHub (src/drivers/sonix.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0, which credits
//! donglover69/sonix-keyboard-studio, deddroid/ak820-rs and usharik/ak820-studio.

use std::time::Duration;

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::hid::{self, Coll, Dev, Hid, RECEIVER, RECEIVER_GRACE, read_until, sum};
use crate::registry::Match;

/// (vid, pid, interface; -1 = any).
const DONGLES: &[(u16, u16, i32)] = &[
    (0x05AC, 0x024F, 3),
    (0x0C45, 0xFEFE, 3),
    (0x0C45, 0xFDFD, 3),
    (0x342D, 0xE479, -1), // EPOMAKER P75
    (0x342D, 0xE485, -1), // RK Royal Kludge R75
    (0x342D, 0xE4BC, -1), // Redragon Arlokks K742B
    (0x342D, 0xE4C2, -1), // M1 V5 VIA
    (0x342D, 0xE407, -1), // Lucky65V2
    (0x388D, 0x0003, -1), // Lofree Flow2
    (0x0DB0, 0x0CA0, -1), // MSI FORGE TKL W FRIEREN EDITION
    (0x36B0, 0x30BD, -1), // Essential75
    (0x342D, 0xE51A, -1), // NUT65
    (0x342D, 0xE487, -1), // RK 2.4G Dongle
    (0x342D, 0xE537, -1), // WEIKAV MAX102
    (0x388D, 0x0002, -1), // Lofree Flow2
    (0x19F5, 0x3245, -1), // NuPhy Air75V2
    (0x362D, 0xD028, -1), // SKN Qinglong
    (0x362D, 0xD030, -1), // Lemokey Link
    (0x362D, 0xD101, -1), // Wireless Link
    (0x342D, 0xE49F, -1), // Spade65-QMK-2.4GHz
];

/// Registry claims; 05AC:024F is claimed by the AULA driver, which hands non-MCHOSE dongles back here.
pub const MATCHES: &[Match] = &[
    Match {
        vid: 0x0C45,
        pids: &[0xFEFE, 0xFDFD],
    },
    Match {
        vid: 0x342D,
        pids: &[
            0xE479, 0xE485, 0xE4BC, 0xE4C2, 0xE407, 0xE51A, 0xE487, 0xE537, 0xE49F,
        ],
    },
    Match {
        vid: 0x388D,
        pids: &[0x0003, 0x0002],
    },
    Match {
        vid: 0x0DB0,
        pids: &[0x0CA0],
    },
    Match {
        vid: 0x36B0,
        pids: &[0x30BD],
    },
    Match {
        vid: 0x19F5,
        pids: &[0x3245],
    },
    Match {
        vid: 0x362D,
        pids: &[0xD028, 0xD030, 0xD101],
    },
];

pub fn request() -> Vec<u8> {
    let mut req = vec![0u8; 33];
    req[1] = 0x20;
    req[2] = 0x01;
    req[32] = sum(&req[1..32]);
    req
}

/// The level byte, or None when the dongle did not answer.
fn query_level(port: &mut dyn Dev) -> Option<u8> {
    port.flush();
    if !port.write(&request()) {
        return None;
    }
    read_until(port, Duration::from_millis(500), |r| {
        r.len() >= 4 && r[..3] == [0x20, 0x01, 0x00]
    })
    .map(|r| r[3])
}

fn keyboard_name(product: &str) -> String {
    let name = match product.strip_suffix(" 2.4G") {
        Some(n) if !n.is_empty() => n,
        _ => product,
    };
    if name.is_empty() {
        "Wireless keyboard".into()
    } else {
        name.into()
    }
}

/// Polls one dongle product; None when it is absent, silent, or reports no keyboard connected.
pub fn poll_product(colls: &[Coll], hid: &mut dyn Hid, vid: u16, pid: u16) -> Option<Device> {
    let &(_, _, iface) = DONGLES.iter().find(|d| d.0 == vid && d.1 == pid)?;
    for c in colls.iter().filter(|c| {
        c.is(vid, pid)
            && (iface < 0 || c.iface == iface)
            && c.page == 0xFF60
            && c.usage == 0x0061
            && c.out_len == 33
            && c.in_len == 33
    }) {
        let Some(mut port) = hid.open(c) else {
            continue;
        };
        // The first request can go unanswered while the 2.4 GHz link wakes up; retry once.
        let Some(level) = query_level(port.as_mut()).or_else(|| query_level(port.as_mut())) else {
            continue;
        };
        // 0 (or out of range): keyboard off, out of range or in another mode. BatteryHub hides the
        // entry; here the tracker keeps the last level for the grace period.
        if !(1..=100).contains(&level) {
            return None;
        }
        let mut d = Device::new(
            format!("sonix:{vid:04X}:{pid:04X}"),
            keyboard_name(&c.product),
            Kind::Keyboard,
            RECEIVER,
        );
        d.percent = Some(level);
        d.grace = RECEIVER_GRACE; // a sleeping keyboard stops answering
        return Some(d);
    }
    None
}

pub fn poll_with(colls: &[Coll], hid: &mut dyn Hid) -> Vec<Device> {
    DONGLES
        .iter()
        .filter_map(|&(vid, pid, _)| poll_product(colls, hid, vid, pid))
        .collect()
}

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    hid::run(api, infos, |hid, colls| poll_with(colls, hid))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::hid::fake::{Fake, coll, per_write};

    fn collection(vid: u16, pid: u16, iface: i32) -> Coll {
        let mut c = coll(vid, pid, 0xFF60, 0x61, 33);
        c.iface = iface;
        c.product = "Keyboard 2.4G".into();
        c
    }

    fn run(c: Coll, replies: Vec<Vec<Vec<u8>>>) -> (Vec<Device>, Fake) {
        let s = per_write(replies);
        (poll_with(&[c], &mut s.clone()), s)
    }

    #[test]
    fn request_bytes() {
        let mut expected = vec![0u8; 33];
        expected[1] = 0x20;
        expected[2] = 1;
        expected[32] = 0x21;
        assert_eq!(request(), expected);
    }

    #[test]
    fn every_dongle_id() {
        for &(vid, pid, _) in DONGLES {
            let iface = if vid == 0x05AC || vid == 0x0C45 { 3 } else { 1 };
            let (out, s) = run(
                collection(vid, pid, iface),
                vec![vec![vec![0x20, 1, 0, 73]]],
            );
            assert_eq!(out.len(), 1, "{vid:04X}:{pid:04X}");
            assert_eq!(out[0].percent, Some(73));
            assert_eq!(out[0].key, format!("sonix:{vid:04X}:{pid:04X}"));
            assert_eq!(out[0].name, "Keyboard");
            assert_eq!(out[0].grace, RECEIVER_GRACE);
            assert_eq!(s.s().writes(), vec![request()]);
        }
    }

    #[test]
    fn levels_outside_1_to_100_mean_no_keyboard() {
        for level in [0u8, 1, 73, 100, 101, 255] {
            let (out, _) = run(
                collection(0x342D, 0xE479, 1),
                vec![vec![vec![0x20, 1, 0, level]]],
            );
            let connected = (1..=100).contains(&level);
            assert_eq!(out.len(), usize::from(connected));
            if connected {
                assert_eq!(out[0].percent, Some(level));
            }
        }
    }

    #[test]
    fn strict_matching() {
        for fault in 0..8 {
            let mut c = collection(0x342D, 0xE479, 1);
            match fault {
                0 => c.vid = 0xFFFF,
                1 => c.pid = 0xFFFF,
                2 => c.page += 1,
                3 => c.usage += 1,
                4 => c.in_len -= 1,
                5 => c.out_len -= 1,
                6 => c = collection(0x05AC, 0x024F, 1),
                _ => c = collection(0x36B0, 0x3002, 1), // the PMO / VIA shared id stays excluded
            }
            let (out, s) = run(c, vec![vec![vec![0x20, 1, 0, 73]]]);
            assert!(out.is_empty() && s.s().opened.is_empty());
        }
    }

    #[test]
    fn noise_and_retry() {
        let (out, s) = run(
            collection(0x342D, 0xE479, 1),
            vec![vec![
                vec![0x20],
                vec![0, 1, 0, 73],
                vec![0x20, 2, 0, 73],
                vec![0x20, 1, 1, 73],
                vec![0x20, 1, 0, 73],
            ]],
        );
        assert_eq!((out.len(), s.s().writes().len()), (1, 1));
        let (out, s) = run(
            collection(0x342D, 0xE479, 1),
            vec![vec![], vec![vec![0x20, 1, 0, 73]]],
        );
        assert_eq!((out.len(), s.s().writes().len()), (1, 2));
        let (out, s) = run(collection(0x342D, 0xE479, 1), vec![]);
        assert!(out.is_empty());
        assert_eq!(s.s().writes().len(), 2);
    }
}

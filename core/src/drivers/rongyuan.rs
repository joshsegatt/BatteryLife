//! Rongyuan 2.4 GHz receivers, used by Attack Shark, Ajazz, MonsGeek, Akko and many other mice and
//! keyboards. One feature report returns both the keyboard and the mouse slot:
//!   request [00 F7 00 ...] on interface 2, vendor collection FFFF:0002
//!   reply   [00][ok][keyboard %][mouse %][keyboard offline][mouse offline]...
//! The status carries no model name, so names come from the receiver's product string with the link
//! words stripped ("AJAZZ 2.4G 8K" -> "AJAZZ wireless mouse").
//!
//! Ported from BatteryHub (src/drivers/rongyuan.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0, which credits
//! qu1ncky/BatteryTray (RongyuanDongleProvider, MIT) and the ATTACK SHARK web driver.

use std::time::{Duration, Instant};

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::hidport::{
    BUDGET, Coll, Opener, Port, RECEIVER, RECEIVER_GRACE, colls, hid_opener,
};
use crate::registry::Match;

/// Receiver ids from BatteryHub's list.
const RECEIVERS: &[(u16, u16)] = &[
    (0x0DB0, 0x197E),
    (0x0DB0, 0x8E9A),
    (0x145F, 0x0376),
    (0x145F, 0x038D),
    (0x14A5, 0xC045),
    (0x25AA, 0x2008),
    (0x25AA, 0x4008),
    (0x25AA, 0x4018),
    (0x3151, 0x4011),
    (0x3151, 0x4014),
    (0x3151, 0x4017),
    (0x3151, 0x4021),
    (0x3151, 0x4027),
    (0x3151, 0x402D),
    (0x3151, 0x4038),
    (0x3151, 0x5006),
    (0x3151, 0x5007),
    (0x3151, 0x5008),
    (0x3151, 0x5020),
    (0x3151, 0x5026),
    (0x3151, 0x502B),
    (0x3151, 0x5031),
    (0x3151, 0x5037),
    (0x3151, 0x5038),
    (0x3151, 0x503D),
    (0x3151, 0x503F),
    (0x3151, 0x5040),
    (0x3151, 0x5042),
    (0x3151, 0x5045),
    (0x3151, 0x504B),
    (0x3151, 0x504E),
    (0x3151, 0x5050),
    (0x3151, 0x5052),
    (0x3151, 0x5055),
    (0x3151, 0x505A),
    (0x342D, 0xE3E0),
    (0x342D, 0xE3E1),
    (0x342D, 0xE3E2),
    (0x342D, 0xE3E6),
    (0x342D, 0xE3E7),
    (0x342D, 0xE3E8),
    (0x342D, 0xE3F5),
    (0x342D, 0xE3F6),
    (0x342D, 0xE3F7),
    (0x342D, 0xE3FB),
    (0x342D, 0xE3FC),
    (0x342D, 0xE3FD),
    (0x347A, 0xA302),
    (0x347A, 0xA305),
    (0x374A, 0xA205),
    (0x374A, 0xA217),
    (0x374A, 0xA220),
    (0x374A, 0xA223),
    (0x374A, 0xA226),
    (0x374A, 0xA229),
    (0x374A, 0xA234),
    (0x374A, 0xA237),
    (0x374A, 0xA251),
    (0x374A, 0xA263),
    (0x374A, 0xA268),
    (0x374A, 0xA311),
    (0x374A, 0xA314),
    (0x374A, 0xA319),
    (0x374A, 0xA322),
    (0x374A, 0xA329),
    (0x374A, 0xA355),
    (0x379A, 0x1804),
    (0x379A, 0x1807),
    (0x38A9, 0x0002),
    (0x38EE, 0x0000),
    (0x38EE, 0x0002),
    (0x38EE, 0x0039),
    (0x391D, 0x1A04),
    (0x391D, 0x1A08),
    (0x391D, 0x1A0E),
    (0x391D, 0x1A13),
    (0x391D, 0x1A25),
    (0x391D, 0x1A2B),
    (0x391D, 0x1A34),
    (0x391D, 0x2017),
    (0x391D, 0x2A05),
];

/// The same receivers, grouped by vendor for the registry.
pub const MATCHES: &[Match] = &[
    Match {
        vid: 0x0DB0,
        pids: &[0x197E, 0x8E9A],
    },
    Match {
        vid: 0x145F,
        pids: &[0x0376, 0x038D],
    },
    Match {
        vid: 0x14A5,
        pids: &[0xC045],
    },
    Match {
        vid: 0x25AA,
        pids: &[0x2008, 0x4008, 0x4018],
    },
    Match {
        vid: 0x3151,
        pids: &[
            0x4011, 0x4014, 0x4017, 0x4021, 0x4027, 0x402D, 0x4038, 0x5006, 0x5007, 0x5008, 0x5020,
            0x5026, 0x502B, 0x5031, 0x5037, 0x5038, 0x503D, 0x503F, 0x5040, 0x5042, 0x5045, 0x504B,
            0x504E, 0x5050, 0x5052, 0x5055, 0x505A,
        ],
    },
    Match {
        vid: 0x342D,
        pids: &[
            0xE3E0, 0xE3E1, 0xE3E2, 0xE3E6, 0xE3E7, 0xE3E8, 0xE3F5, 0xE3F6, 0xE3F7, 0xE3FB, 0xE3FC,
            0xE3FD,
        ],
    },
    Match {
        vid: 0x347A,
        pids: &[0xA302, 0xA305],
    },
    Match {
        vid: 0x374A,
        pids: &[
            0xA205, 0xA217, 0xA220, 0xA223, 0xA226, 0xA229, 0xA234, 0xA237, 0xA251, 0xA263, 0xA268,
            0xA311, 0xA314, 0xA319, 0xA322, 0xA329, 0xA355,
        ],
    },
    Match {
        vid: 0x379A,
        pids: &[0x1804, 0x1807],
    },
    Match {
        vid: 0x38A9,
        pids: &[0x0002],
    },
    Match {
        vid: 0x38EE,
        pids: &[0x0000, 0x0002, 0x0039],
    },
    Match {
        vid: 0x391D,
        pids: &[
            0x1A04, 0x1A08, 0x1A0E, 0x1A13, 0x1A25, 0x1A2B, 0x1A34, 0x2017, 0x2A05,
        ],
    },
];

/// Product string without link words: "AJAZZ 2.4G 8K" -> "AJAZZ", "2.4G Wireless Receiver" -> "".
fn brand(product: &str) -> String {
    const LINK_WORDS: [&str; 10] = [
        "2.4G", "2.4GHZ", "8K", "4K", "2K", "1K", "RECEIVER", "WIRELESS", "DONGLE", "USB",
    ];
    product
        .split_whitespace()
        .filter(|w| !LINK_WORDS.contains(&w.to_uppercase().as_str()))
        .collect::<Vec<_>>()
        .join(" ")
}

fn query_status(port: &mut dyn Port, feat_len: u16) -> Option<Vec<u8>> {
    for attempt in 0..3 {
        if attempt > 0 {
            port.pause(Duration::from_millis(100));
        }
        let mut cmd = vec![0u8; usize::from(feat_len)];
        cmd[1] = 0xF7;
        if !port.set_feature(&cmd) {
            return None;
        }
        port.pause(Duration::from_millis(20));
        if let Some(r) = port
            .get_feature(0, usize::from(feat_len))
            .filter(|r| r.len() >= 7 && r[2..7].iter().any(|b| *b != 0))
        {
            return Some(r);
        }
    }
    None
}

fn slot_name(vid: u16, pid: u16, product: &str, keyboard: bool) -> String {
    let generic = product.is_empty() || product.contains("Receiver") || product.contains("2.4G");
    let brand = brand(product);
    let other_brand = !brand.is_empty() && !brand.to_uppercase().contains("ATTACK SHARK");
    let kind = if keyboard { "keyboard" } else { "mouse" };
    if (vid, pid) == (0x3151, 0x402D) && !keyboard && !other_brand {
        "Attack Shark R3".into()
    } else if (vid, pid) == (0x3151, 0x5038) && keyboard {
        "MonsGeek keyboard".into() // the receiver says "MonsGeek 2.4G Wireless Keyboard"
    } else if generic && !brand.is_empty() {
        format!("{brand} wireless {kind}")
    } else if (vid, pid) == (0x3151, 0x5006) && keyboard && product.contains("Akko") {
        "Akko Air 01".into()
    } else if generic {
        format!("Wireless {kind}")
    } else {
        product.into()
    }
}

/// Devices in the two slots of a status reply: [2] keyboard %, [3] mouse %, [4] / [5] offline flags.
pub fn slots(r: &[u8], vid: u16, pid: u16, product: &str) -> Vec<Device> {
    let mut out = Vec::new();
    for keyboard in [true, false] {
        let (level, offline) = if keyboard { (r[2], r[4]) } else { (r[3], r[5]) };
        if offline != 0 {
            continue; // nothing connected in this slot
        }
        let mut d = Device::new(
            format!(
                "rongyuan:{vid:04X}:{pid:04X}:{}",
                if keyboard { "kb" } else { "ms" }
            ),
            slot_name(vid, pid, product, keyboard),
            if keyboard {
                Kind::Keyboard
            } else {
                Kind::Mouse
            },
            RECEIVER,
        );
        d.grace = RECEIVER_GRACE; // keep the last reading while the device sleeps
        d.percent = Some(level).filter(|l| (1..=100).contains(l));
        out.push(d);
    }
    out
}

pub fn poll_with(colls: &[Coll], open: &mut Opener) -> Vec<Device> {
    let mut out = Vec::new();
    let mut done: Vec<(u16, u16)> = Vec::new();
    for c in colls {
        let id = (c.vid, c.pid);
        if done.contains(&id)
            || !RECEIVERS.contains(&id)
            || c.iface != 2
            || c.page != 0xFFFF
            || c.usage != 0x0002
            || c.feat_len < 7
        {
            continue;
        }
        let Some(mut port) = open(c) else { continue };
        let Some(r) = query_status(port.as_mut(), c.feat_len) else {
            continue;
        };
        out.extend(slots(&r, c.vid, c.pid, &c.product));
        done.push(id);
    }
    out
}

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    poll_with(&colls(infos), &mut hid_opener(api, Instant::now() + BUDGET))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::hidport::fake::{self, coll};

    #[test]
    fn matches_list_every_receiver_once() {
        let mut from_matches: Vec<(u16, u16)> = MATCHES
            .iter()
            .flat_map(|m| m.pids.iter().map(move |p| (m.vid, *p)))
            .collect();
        let mut list = RECEIVERS.to_vec();
        from_matches.sort_unstable();
        list.sort_unstable();
        assert_eq!(from_matches, list);
        assert_eq!(list.len(), 81);
    }

    #[test]
    fn brands_and_names() {
        assert_eq!(brand("AJAZZ 2.4G 8K"), "AJAZZ");
        assert_eq!(brand("2.4G Wireless Receiver"), "");
        assert_eq!(
            slot_name(0x3151, 0x402D, "2.4G Wireless Receiver", false),
            "Attack Shark R3"
        );
        assert_eq!(
            slot_name(0x3151, 0x402D, "AJAZZ 2.4G 8K", false),
            "AJAZZ wireless mouse"
        );
        assert_eq!(
            slot_name(0x3151, 0x5038, "MonsGeek 2.4G Wireless Keyboard", true),
            "MonsGeek keyboard"
        );
        assert_eq!(
            slot_name(0x3151, 0x5006, "Akko Keyboard", true),
            "Akko Air 01"
        );
        assert_eq!(slot_name(0x3151, 0x4011, "", false), "Wireless mouse");
        assert_eq!(slot_name(0x3151, 0x4011, "X68 Pro", true), "X68 Pro");
    }

    fn receiver(vid: u16, pid: u16, product: &str) -> Coll {
        let mut c = coll(vid, pid, 0xFFFF, 0x0002, 0);
        c.iface = 2;
        c.feat_len = 65;
        c.product = product.into();
        c
    }

    fn device(status: [u8; 7]) -> fake::Shared {
        let s = fake::script();
        let log = s.clone();
        s.borrow_mut().feature = Some(Box::new(move |_, id, len| {
            assert_eq!((id, len), (0, 65));
            let asked = log
                .borrow()
                .features
                .last()
                .is_some_and(|f| f.len() == 65 && f[1] == 0xF7);
            let mut r = vec![0u8; len];
            if asked {
                r[..7].copy_from_slice(&status);
            }
            Some(r)
        }));
        s
    }

    #[test]
    fn both_slots_from_one_report() {
        let s = device([0, 1, 80, 45, 0, 0, 0]);
        let out = poll_with(
            &[receiver(0x3151, 0x402D, "2.4G Wireless Receiver")],
            &mut fake::opener(&s),
        );
        assert_eq!(s.borrow().features.len(), 1);
        assert_eq!(out.len(), 2);
        assert_eq!(
            (out[0].key.as_str(), out[0].kind, out[0].percent),
            ("rongyuan:3151:402D:kb", Kind::Keyboard, Some(80))
        );
        assert_eq!(
            (out[1].name.as_str(), out[1].percent),
            ("Attack Shark R3", Some(45))
        );
    }

    #[test]
    fn offline_slot_and_unknown_level() {
        // Keyboard offline; mouse connected but reporting 0 (unknown level).
        let s = device([0, 1, 0, 0, 1, 0, 0]);
        let out = poll_with(
            &[receiver(0x342D, 0xE3E0, "AJAZZ 2.4G 8K")],
            &mut fake::opener(&s),
        );
        assert_eq!(out.len(), 1);
        assert_eq!(
            (out[0].name.as_str(), out[0].percent),
            ("AJAZZ wireless mouse", None)
        );
    }

    #[test]
    fn silent_receiver_and_strict_matching() {
        let s = device([0; 7]);
        assert!(poll_with(&[receiver(0x3151, 0x402D, "")], &mut fake::opener(&s)).is_empty());
        assert_eq!(s.borrow().features.len(), 3);
        let mut other_iface = receiver(0x3151, 0x402D, "");
        other_iface.iface = 1;
        let s = device([0, 1, 80, 45, 0, 0, 0]);
        let colls = [other_iface, receiver(0x3151, 0x1234, "")];
        assert!(poll_with(&colls, &mut fake::opener(&s)).is_empty());
        assert!(s.borrow().opened.is_empty());
    }
}

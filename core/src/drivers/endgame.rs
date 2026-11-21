//! Endgame Gear 4K wireless mice (OP1w 4k v1 / v2, XM2w 4k v1 / v2) on the shared 4K dongle 3367:1970.
//! Config channel FF01:0002, feature report A1 (64 bytes): [A1][cmd][target][payload len][00 ...],
//! payload from byte 16. SET_FEATURE, wait ~150 ms, GET_FEATURE A1; reply byte 1 is the status:
//! 01 ok, 03 busy (read again), 08 / 11 mouse asleep or out of range.
//!   B4 battery: reply[16] = percent (no charging flag is decoded).
//!   0E mouse info: reply[16..17] = 67 33 (VID), [18..19] = mouse PID (LE).
//!   0F probe: after a failed command the dongle stays silent until a 0F gets through, so it is sent
//!   only on failure. Only these reads are used; never the write / reset / re-pair commands.
//!
//! Ported from BatteryHub (src/drivers/endgame.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0, which credits vole and
//! Endgame_Gear_Web_Configurator.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::report_io::{self, Coll, Dev, Hid, device, find};

pub const VID: u16 = 0x3367;
pub const PID_DONGLE_4K: u16 = 0x1970;

fn mouse_name(pid: u16) -> Option<&'static str> {
    match pid {
        0x1972 => Some("Endgame Gear OP1w 4k"),
        0x1984 => Some("Endgame Gear OP1w 4k v2"),
        0x1968 => Some("Endgame Gear XM2w 4k"),
        0x1982 => Some("Endgame Gear XM2w 4k v2"),
        _ => None,
    }
}

pub fn request(cmd: u8, target: u8) -> Vec<u8> {
    let mut req = vec![0u8; 64];
    req[..3].copy_from_slice(&[0xA1, cmd, target]);
    req
}

/// Sends one command; returns (status, reply), or None on I/O error.
fn command(hid: &mut dyn Hid, d: &mut dyn Dev, cmd: u8, target: u8) -> Option<(u8, Vec<u8>)> {
    if !d.set_feature(&request(cmd, target)) {
        return None;
    }
    let mut last = Vec::new();
    for attempt in 0..3 {
        hid.sleep(Duration::from_millis(150 + attempt * 100));
        let r = d
            .get_feature(0xA1)
            .filter(|r| r.len() >= 20 && r[0] == 0xA1)?;
        if r[1] != 0x03 {
            return Some((r[1], r));
        }
        last = r; // 03 = busy, read again
    }
    Some((0x03, last))
}

/// The mouse model from a 0E reply.
pub fn parse_info(r: &[u8]) -> Option<&'static str> {
    (r[16] == 0x67 && r[17] == 0x33)
        .then(|| mouse_name(u16::from_le_bytes([r[18], r[19]])))
        .flatten()
}

/// The mouse only says what it is while awake: the last name seen, by dongle path.
static LAST_NAME: Mutex<Option<HashMap<String, &'static str>>> = Mutex::new(None);

fn poll_with(hid: &mut dyn Hid, colls: &[Coll]) -> Vec<Device> {
    let cs = find(colls, VID, PID_DONGLE_4K, |c| {
        c.page == 0xFF01 && c.usage == 0x0002 && c.feature >= 64
    });
    for c in cs {
        let Some(mut d) = hid.open(c) else { continue };
        let mut res = command(hid, d.as_mut(), 0xB4, 0x00);
        if !matches!(res, Some((0x01 | 0x08 | 0x11, _))) {
            // Unexpected status or no answer: probe the dongle and the mouse once, then retry.
            let _ = command(hid, d.as_mut(), 0x0F, 0x01);
            let _ = command(hid, d.as_mut(), 0x0F, 0x0F);
            res = command(hid, d.as_mut(), 0xB4, 0x00);
        }
        let Some((st, r)) = res else { continue };
        let awake = st == 0x01 && r[16] <= 100;
        let asleep = st == 0x08 || st == 0x11;
        if !awake && !asleep {
            continue;
        }
        let mut s = device(
            format!("endgame:{PID_DONGLE_4K:04X}"),
            "",
            Kind::Mouse,
            "2.4 GHz receiver",
        );
        s.grace = Duration::from_secs(30 * 60);
        s.asleep = asleep; // shown with the last known value
        let mut model = None;
        if awake {
            s.percent = Some(r[16]);
            if let Some((0x01, info)) = command(hid, d.as_mut(), 0x0E, 0x00) {
                model = parse_info(&info);
            }
        }
        let mut names = LAST_NAME.lock().unwrap_or_else(|e| e.into_inner());
        let names = names.get_or_insert_with(HashMap::new);
        if let Some(n) = model {
            names.insert(c.path.clone(), n);
        }
        s.name = names
            .get(&c.path)
            .copied()
            .unwrap_or("Endgame Gear 4k wireless mouse")
            .into();
        return vec![s];
    }
    Vec::new()
}

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    report_io::run(api, infos, poll_with)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::report_io::fake::{Fake, Op};

    fn coll(path: &str) -> Coll {
        Coll {
            vid: VID,
            pid: PID_DONGLE_4K,
            page: 0xFF01,
            usage: 0x0002,
            feature: 64,
            path: path.into(),
            ..Default::default()
        }
    }

    fn reply(cmd: u8, status: u8, payload: &[u8]) -> Vec<u8> {
        let mut r = vec![0u8; 64];
        r[0] = 0xA1;
        r[1] = status;
        r[2] = cmd;
        r[16..16 + payload.len()].copy_from_slice(payload);
        r
    }

    /// Answers each command with `answer(cmd)`; records the commands sent.
    fn dongle(answer: impl Fn(u8) -> Vec<u8> + 'static) -> Fake {
        let mut last = 0u8;
        Fake::new(move |_, op| match op {
            Op::SetFeature(w) => {
                assert!(matches!(w[1], 0xB4 | 0x0E | 0x0F), "read commands only");
                last = w[1];
                Some(vec![])
            }
            Op::GetFeature(0xA1) => Some(answer(last)),
            _ => None,
        })
    }

    fn sent(f: &Fake) -> Vec<u8> {
        f.log
            .borrow()
            .iter()
            .filter_map(|(_, o)| match o {
                Op::SetFeature(w) => Some(w[1]),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn awake_mouse_names_itself() {
        let mut f = dongle(|cmd| match cmd {
            0xB4 => reply(cmd, 0x01, &[83]),
            _ => reply(cmd, 0x01, &[0x67, 0x33, 0x84, 0x19]),
        });
        let out = poll_with(&mut f, &[coll("eg-awake")]);
        assert_eq!(sent(&f), [0xB4, 0x0E]);
        let s = &out[0];
        assert_eq!((s.percent, s.asleep), (Some(83), false));
        assert_eq!(
            (s.name.as_str(), s.key.as_str()),
            ("Endgame Gear OP1w 4k v2", "endgame:1970")
        );
        // Asleep next time: the remembered name stays, without a level.
        let mut f = dongle(|cmd| reply(cmd, 0x08, &[]));
        let out = poll_with(&mut f, &[coll("eg-awake")]);
        assert!(out[0].asleep && out[0].percent.is_none());
        assert_eq!(out[0].name, "Endgame Gear OP1w 4k v2");
    }

    #[test]
    fn unexpected_status_probes_once_then_retries() {
        let mut f = dongle(|cmd| reply(cmd, 0x02, &[]));
        assert!(poll_with(&mut f, &[coll("eg-silent")]).is_empty());
        assert_eq!(sent(&f), [0xB4, 0x0F, 0x0F, 0xB4]);
        // Busy three times counts as no answer.
        let mut f = dongle(|cmd| reply(cmd, 0x03, &[]));
        assert!(poll_with(&mut f, &[coll("eg-busy")]).is_empty());
        let mut f = dongle(|cmd| reply(cmd, 0x11, &[]));
        let out = poll_with(&mut f, &[coll("eg-off")]);
        assert_eq!(out[0].name, "Endgame Gear 4k wireless mouse");
    }
}

//! DAREU (VID 260D): EM901X mouse on its receiver (1074) or cable (1090), Sharkoon Skiller SGM50W
//! (1125), the A84 keyboard (0022), and the Togran receiver (1131) / A980 Pro cable (1148).
//!
//! - EM901X / SGM50W: feature report 08 [08][04][0 ...][49] on FF02:0002 (checksum 0x55 - sum), the
//!   reply arrives as input report 09 on the sibling FF01:0000 collection:
//!   [09][04][status 0]...[6 percent][7 charging].
//! - A84: SetFeature [00][04 F6 00 ...] (65 bytes) on the interface 0 keyboard collection, GetFeature
//!   report 0; three reads discarded, then up to three until byte 15 is non-zero; byte 5 = percent.
//! - Togran: feature report 0A (65 bytes) on the consumer-control collection,
//!   [0A][slot << 4][len][class][cmd]; slot 0 class 00 / A0 lists paired mice, slot n class 07 / 80
//!   gives [7] status (1 = charging) [8] level [9] max level.
//!
//! Only these read-only queries are sent. From the official EM901X AIM-WL, A84 and Togran drivers
//! (protocol notes in wushuai0802/myfinder-device-battery).
//!
//! Ported from BatteryHub (src/drivers/dareu.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0.

use std::time::{Duration, Instant};

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::hid::{self, CABLE, Coll, Dev, Hid, RECEIVER, RECEIVER_GRACE};

pub const VID: u16 = 0x260D;
pub const PIDS: &[u16] = &[0x1131, 0x1148, 0x0022, 0x1090, 0x1074, 0x1125];

fn device(key: String, name: String, kind: Kind, via: &'static str) -> Device {
    let mut d = Device::new(key, name, kind, via);
    if via == RECEIVER {
        d.grace = RECEIVER_GRACE;
    }
    d
}

/// The only collection of a product that matches `pred`; None when there is none or several (do not
/// pair arbitrary collections when several devices share a product id).
fn only(colls: &[Coll], pid: u16, pred: impl Fn(&Coll) -> bool) -> Option<&Coll> {
    let mut it = colls.iter().filter(|c| c.is(VID, pid) && pred(c));
    let first = it.next()?;
    it.next().is_none().then_some(first)
}

// --- Togran ----------------------------------------------------------------------------------

fn togran_query(port: &mut dyn Dev, slot: u8, len: u8, cls: u8, cmd: u8) -> Option<Vec<u8>> {
    let mut req = vec![0u8; 65];
    req[..5].copy_from_slice(&[0x0A, slot << 4, len, cls, cmd]);
    if !port.set_feature(&req) {
        return None;
    }
    for _ in 0..15 {
        port.sleep(Duration::from_millis(30));
        let r = port.get_feature_len(0x0A, 65).filter(|r| r.len() >= 11)?;
        if r[1] & 0x0F == 2 && r[3] == cls && r[4] == cmd {
            return Some(r);
        }
    }
    None
}

fn togran_battery(port: &mut dyn Dev, slot: u8, pid: u16, mouse_pid: u16) -> Option<Device> {
    let b = togran_query(port, slot, 4, 0x07, 0x80)?;
    let max = if b[9] != 0 { b[9] } else { 100 };
    if b[8] > max {
        return None;
    }
    let key = if pid == mouse_pid {
        format!("dareu:260D:{pid:04X}")
    } else {
        format!("dareu:260D:{pid:04X}:{mouse_pid:04X}")
    };
    let name = if mouse_pid == 0x1148 {
        "DAREU A980 Pro".into()
    } else {
        format!("DAREU {mouse_pid:04X}")
    };
    let via = if slot != 0 { RECEIVER } else { CABLE };
    let mut d = device(key, name, Kind::Mouse, via);
    d.percent = Some((u16::from(b[8]) * 100 / u16::from(max)) as u8);
    d.coarse = max < 100;
    d.charging = b[7] == 1;
    Some(d)
}

/// The receiver lists mice by slot; the A980 Pro on its cable answers on slot 0. While wired, the
/// receiver keeps the mouse listed with status 0, so it is not shown twice.
fn poll_togran(colls: &[Coll], hid: &mut dyn Hid, out: &mut Vec<Device>) {
    for pid in [0x1131u16, 0x1148] {
        let Some(c) = only(colls, pid, |c| {
            c.page == 0x000C && c.usage == 1 && c.feat_len == 65
        }) else {
            continue;
        };
        let Some(mut port) = hid.open(c) else {
            continue;
        };
        if pid != 0x1131 {
            out.extend(togran_battery(port.as_mut(), 0, pid, pid));
            continue;
        }
        let Some(r) = togran_query(port.as_mut(), 0, 6, 0x00, 0xA0) else {
            continue;
        };
        let count = usize::from(r[7]).min(5);
        for slot in 1..=count {
            let e = &r[8 + (slot - 1) * 3..8 + slot * 3];
            if e[0] != 0 {
                let mouse = u16::from_be_bytes([e[1], e[2]]);
                out.extend(togran_battery(port.as_mut(), slot as u8, pid, mouse));
            }
        }
    }
}

// --- A84 -------------------------------------------------------------------------------------

fn poll_a84(colls: &[Coll], hid: &mut dyn Hid) -> Option<Device> {
    let c = only(colls, 0x0022, |c| {
        c.iface == 0 && c.page == 0x01 && c.usage == 0x06 && c.feat_len == 65
    })?;
    let mut port = hid.open(c)?;
    let mut req = vec![0u8; 65];
    req[1] = 0x04;
    req[2] = 0xF6;
    if !port.set_feature(&req) {
        return None;
    }
    port.sleep(Duration::from_millis(10));
    for _ in 0..3 {
        port.get_feature_len(0, 65); // the official driver discards these reads
        port.sleep(Duration::from_millis(10));
    }
    for i in 0..3 {
        if i > 0 {
            port.sleep(Duration::from_millis(10));
        }
        let Some(r) = port
            .get_feature_len(0, 65)
            .filter(|r| r.len() >= 16 && r[15] != 0)
        else {
            continue;
        };
        if r[5] > 100 {
            return None;
        }
        let mut d = device(
            "dareu:260D:0022".into(),
            "DAREU A84".into(),
            Kind::Keyboard,
            "HID",
        );
        d.percent = Some(r[5]);
        return Some(d);
    }
    None
}

// --- EM901X / SGM50W ---------------------------------------------------------------------------

pub const EM901X_REQUEST: [u8; 17] = [0x08, 0x04, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x49];

/// (percent, charging) from an input report 09 reply.
pub fn em901x_reply(r: &[u8]) -> Option<(u8, bool)> {
    (r.len() == 17 && r[0] == 0x09 && r[1] == 0x04 && r[2] == 0 && r[6] <= 100)
        .then(|| (r[6], r[7] != 0))
}

fn poll_em901x(colls: &[Coll], hid: &mut dyn Hid, out: &mut Vec<Device>) {
    // Prefer a cable reading over the receiver's possibly cached telemetry.
    let mut em901x_read = false;
    for pid in [0x1090u16, 0x1074, 0x1125] {
        if em901x_read && pid == 0x1074 {
            continue;
        }
        let input = only(colls, pid, |c| {
            c.iface == 1 && c.page == 0xFF01 && c.usage == 0 && c.in_len == 17
        });
        let feature = only(colls, pid, |c| {
            c.iface == 1 && c.page == 0xFF02 && c.usage == 2 && c.feat_len == 17
        });
        let (Some(input), Some(feature)) = (input, feature) else {
            continue;
        };
        let Some(mut input) = hid.open(input) else {
            continue;
        };
        let Some(mut feature) = hid.open(feature) else {
            continue;
        };
        input.flush();
        if !feature.set_feature(&EM901X_REQUEST) {
            continue;
        }
        let end = Instant::now() + Duration::from_secs(1);
        for _ in 0..32 {
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            let Some(r) = input.read(left) else { break };
            let Some((percent, charging)) = em901x_reply(&r) else {
                continue;
            };
            let name = if pid == 0x1125 {
                "Sharkoon Skiller SGM50W"
            } else {
                "DAREU EM901X"
            };
            let via = if pid == 0x1090 { CABLE } else { RECEIVER };
            let mut d = device(
                format!("dareu:260D:{pid:04X}"),
                name.into(),
                Kind::Mouse,
                via,
            );
            d.percent = Some(percent);
            d.charging = charging;
            out.push(d);
            em901x_read = pid != 0x1125;
            break;
        }
    }
}

pub fn poll_with(colls: &[Coll], hid: &mut dyn Hid) -> Vec<Device> {
    let mut out = Vec::new();
    poll_togran(colls, hid, &mut out);
    out.extend(poll_a84(colls, hid));
    poll_em901x(colls, hid, &mut out);
    out
}

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    hid::run(api, infos, |hid, colls| poll_with(colls, hid))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::hid::fake::{Fake, at, coll, feature_script, path_responder, reads};

    /// After each accepted send, queues `respond(path, data)`; a flush drops queued replies.
    /// Feature reads go to `feature(state, path, id, len)`; a flush drops queued replies.
    /// Input and feature reports read.
    fn pair(pid: u16) -> [Coll; 2] {
        let mut input = at(coll(VID, pid, 0xFF01, 0, 17), &format!("input-{pid:04X}"));
        input.iface = 1;
        input.out_len = 0;
        let mut feature = at(coll(VID, pid, 0xFF02, 2, 0), &format!("feature-{pid:04X}"));
        feature.iface = 1;
        feature.feat_len = 17;
        [input, feature]
    }

    fn reply(percent: u8, charging: u8) -> Vec<u8> {
        vec![
            0x09, 0x04, 0, 0, 0, 2, percent, charging, 0, 0, 0, 0, 0, 0, 0, 0, 0xF1,
        ]
    }

    /// Replies per product id, queued when the feature request reaches that product.
    fn by_pid(table: Vec<(u16, Vec<Vec<u8>>)>) -> Fake {
        let mut table = table;
        path_responder(move |path, data| {
            assert_eq!(data, EM901X_REQUEST);
            table
                .iter_mut()
                .find(|(pid, _)| path.ends_with(&format!("{pid:04X}")))
                .map(|(_, r)| std::mem::take(r))
                .unwrap_or_default()
        })
    }

    #[test]
    fn em901x_receiver() {
        let s = by_pid(vec![(0x1074, vec![reply(85, 0)])]);
        let out = poll_with(&pair(0x1074), &mut s.clone());
        let d = &out[0];
        assert_eq!(
            (d.name.as_str(), d.key.as_str()),
            ("DAREU EM901X", "dareu:260D:1074")
        );
        assert_eq!(
            (d.kind, d.via.as_str(), d.percent, d.charging),
            (Kind::Mouse, RECEIVER, Some(85), false)
        );
        let n = reads(&s);
        let st = s.s();
        assert_eq!((st.opened.len(), st.features().len(), n), (2, 1, 1));
        for percent in [0u8, 100] {
            let s = by_pid(vec![(0x1074, vec![reply(percent, 2)])]);
            let d = &poll_with(&pair(0x1074), &mut s.clone())[0];
            assert_eq!((d.percent, d.charging), (Some(percent), true));
        }
    }

    #[test]
    fn unrelated_and_bad_reports_are_skipped() {
        let mut wrong_id = reply(85, 0);
        wrong_id[0] = 8;
        let mut wrong_command = reply(85, 0);
        wrong_command[1] = 3;
        let mut failed = reply(85, 0);
        failed[2] = 1;
        let mut short = reply(85, 0);
        short.truncate(7);
        let mut long = reply(85, 0);
        long.push(0);
        let s = by_pid(vec![(
            0x1074,
            vec![
                wrong_id.clone(),
                wrong_command.clone(),
                failed.clone(),
                short,
                long,
                reply(101, 0),
                reply(42, 1),
            ],
        )]);
        let out = poll_with(&pair(0x1074), &mut s.clone());
        assert_eq!(
            (out[0].percent, out[0].charging, reads(&s)),
            (Some(42), true, 7)
        );

        let s = by_pid(vec![(0x1074, vec![wrong_id, failed, reply(255, 0)])]);
        assert!(poll_with(&pair(0x1074), &mut s.clone()).is_empty());
        // Bounded under constant unrelated traffic.
        let s = by_pid(vec![(0x1074, vec![wrong_command; 33])]);
        assert!(poll_with(&pair(0x1074), &mut s.clone()).is_empty());
        assert_eq!(reads(&s), 32);
        // Silence is not 0 %.
        let s = by_pid(vec![]);
        assert!(poll_with(&pair(0x1074), &mut s.clone()).is_empty());
    }

    #[test]
    fn cable_wins_over_the_receiver_but_not_over_the_sgm50w() {
        let mut colls = pair(0x1074).to_vec();
        colls.extend(pair(0x1090));
        let s = by_pid(vec![
            (0x1090, vec![reply(85, 1)]),
            (0x1074, vec![reply(80, 0)]),
        ]);
        let out = poll_with(&colls, &mut s.clone());
        assert_eq!(out.len(), 1);
        assert_eq!(
            (out[0].via.as_str(), out[0].percent, out[0].key.as_str()),
            (CABLE, Some(85), "dareu:260D:1090")
        );
        assert_eq!(s.s().features().len(), 1);

        let s = by_pid(vec![(0x1074, vec![reply(80, 0)])]);
        let out = poll_with(&colls, &mut s.clone());
        assert_eq!(
            (out.len(), out[0].via.as_str(), out[0].percent),
            (1, RECEIVER, Some(80))
        );
        assert_eq!(s.s().features().len(), 2);

        let mut colls = pair(0x1125).to_vec();
        colls.extend(pair(0x1074));
        let s = by_pid(vec![
            (0x1074, vec![reply(80, 0)]),
            (0x1125, vec![reply(64, 0)]),
        ]);
        let out = poll_with(&colls, &mut s.clone());
        assert_eq!(out.len(), 2);
        assert_eq!((out[0].percent, out[1].percent), (Some(80), Some(64)));
        assert_eq!(out[1].name, "Sharkoon Skiller SGM50W");
    }

    #[test]
    fn em901x_strict_matching() {
        for invalid in 0..10 {
            let mut colls = pair(0x1074).to_vec();
            match invalid {
                0 => colls[0].pid = 0x1090,
                1 => colls[1].vid = 0x3554,
                2 => colls[0].page = 0xFF03,
                3 => colls[1].usage = 0,
                4 => colls[0].in_len = 8,
                5 => colls[1].feat_len = 65,
                6 => colls[0].iface = 2,
                7 => colls.push(colls[0].clone()),
                8 => colls.push(colls[1].clone()),
                _ => {
                    colls.pop();
                }
            }
            let s = by_pid(vec![(0x1074, vec![reply(85, 0)])]);
            assert!(
                poll_with(&colls, &mut s.clone()).is_empty(),
                "case {invalid}"
            );
            assert!(s.s().opened.is_empty());
        }
    }

    #[test]
    fn togran_receiver_lists_slots() {
        let mut c = coll(VID, 0x1131, 0x000C, 1, 0);
        c.feat_len = 65;
        let s = feature_script(move |log, _, id, len| {
            assert_eq!((id, len), (0x0A, 65));
            let last = log.features().last().cloned().unwrap();
            let mut r = vec![0u8; 65];
            r[0] = 0x0A;
            r[1] = (last[1] & 0xF0) | 2;
            r[3] = last[3];
            r[4] = last[4];
            if last[3] == 0x00 {
                // Two slots: slot 1 holds an A980 Pro (1148), slot 2 is empty.
                r[7] = 2;
                r[8..11].copy_from_slice(&[1, 0x11, 0x48]);
            } else {
                r[7] = 1; // charging
                r[8] = 7; // level 7 of 10
                r[9] = 10;
            }
            Some(r)
        });
        let out = poll_with(&[c], &mut s.clone());
        assert_eq!(out.len(), 1);
        let d = &out[0];
        assert_eq!(
            (d.key.as_str(), d.name.as_str()),
            ("dareu:260D:1131:1148", "DAREU A980 Pro")
        );
        assert_eq!(
            (d.percent, d.charging, d.coarse, d.via.as_str()),
            (Some(70), true, true, RECEIVER)
        );
        assert_eq!(s.s().features()[0][..5], [0x0A, 0x00, 6, 0x00, 0xA0]);
        assert_eq!(s.s().features()[1][..5], [0x0A, 0x10, 4, 0x07, 0x80]);
    }

    #[test]
    fn a84_reads_level_after_discarded_reads() {
        let mut c = coll(VID, 0x0022, 0x01, 0x06, 0);
        c.iface = 0;
        c.feat_len = 65;
        let mut n = 0;
        let s = feature_script(move |_, _, id, len| {
            assert_eq!((id, len), (0, 65));
            n += 1;
            let mut r = vec![0u8; 65];
            if n >= 5 {
                r[5] = 66;
                r[15] = 1;
            }
            Some(r)
        });
        let out = poll_with(&[c], &mut s.clone());
        assert_eq!(s.s().features()[0][..3], [0x00, 0x04, 0xF6]);
        assert_eq!(
            (out[0].name.as_str(), out[0].kind, out[0].percent),
            ("DAREU A84", Kind::Keyboard, Some(66))
        );
    }
}

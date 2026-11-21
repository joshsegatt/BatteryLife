//! Keychron (VID 3434): wireless mice on their receivers and cables, the Nape Pro trackball, and
//! Keychron keyboards on the Keychron Link receiver. Every Keychron product is scanned, as BatteryHub does.
//!
//! - Mice, Launcher protocol (vendor page, 65-byte reports, no report id): 64-byte request
//!   [cmd][00][81][sub][0..][checksum], checksum (byte 63) = 0xA1 - sum(bytes 0..62).
//!   01 = the mouse's own status ([6..7] vendor id, [10] charging, [11] percent); 41 = the same asked
//!   over a receiver (8K Nordic: [10] state, [11] percent); on the 4K Link (FF0A:0001, a 01 reply
//!   without 34 34 / 2D 36 at bytes 6/7) the 41 reply has [5] state, [6] percent, and 40 00 81 00 gives
//!   the paired mouse's product id at bytes 4/5.
//! - 1K receiver (D030 / D031): SetFeature [51][06] on the usage page 8C collection, then GetFeature 51
//!   returns [51][06][01 = ok][00][vid LE][pid LE][..][..][..][percent][charging].
//! - Ultra-Link 8K receiver (D028, FFC1:0001, numbered reports): B5 [03] lists the receiver's slots,
//!   B3 [06] returns the mouse's base info whose payload byte 19 is the power byte (bits 0-6 percent,
//!   bit 7 charging).
//! - Nape Pro (Link-KM receiver D026 or its cable 0440, FF60:0061, 32-byte raw HID): B2 lists the
//!   receiver's slots, A7 31 returns [A7][31][percent][charging].
//! - Keyboards on the Keychron Link (FF60:0061): B2 lists the slots, A4 (KC_GET_BATTERY firmware patch)
//!   returns [A4][percent][mV LE][0 battery / 1 charging / 2 full]. Stock firmware never answers.
//!
//! Only these read-only queries are sent. Sources (via BatteryHub): Keychron's web Launcher,
//! ldkv/qmk_firmware#1, nvalis/nape_pro, gtun/KeychronNapeBatteryWidget, ky0209/NapeBar.
//!
//! Ported from BatteryHub (src/drivers/keychron.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0.

use std::time::{Duration, Instant};

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::hid::{self, CABLE, Coll, Dev, Hid, RECEIVER, RECEIVER_GRACE, sum};

pub const VID: u16 = 0x3434;

const LINK_KM: u16 = 0xD026;
const NAPE_USB: u16 = 0x0440;
const NAPE_IN_SLOT: u16 = 0x4004;
const LEGACY_A: u16 = 0xD030;
const LEGACY_C: u16 = 0xD031;
const ULTRA_LINK_8K: u16 = 0xD028;
const REPLY: Duration = Duration::from_millis(600);

fn device(key: String, name: &str, kind: Kind, via: &'static str) -> Device {
    let mut d = Device::new(key, name, kind, via);
    if via == RECEIVER {
        d.grace = RECEIVER_GRACE;
    }
    d
}

/// Reads until `take` accepts a report; `stop` ends the wait early (a timeout always does).
fn wait(
    port: &mut dyn Dev,
    timeout: Duration,
    stop: impl Fn(&[u8]) -> bool,
    take: impl Fn(&[u8]) -> bool,
) -> Option<Vec<u8>> {
    let end = Instant::now() + timeout;
    loop {
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return None;
        }
        let r = port.read(left)?;
        if stop(&r) {
            return None;
        }
        if take(&r) {
            return Some(r);
        }
    }
}

// --- Launcher mouse protocol -----------------------------------------------------------------

/// [report id 0][cmd][00][81][sub][0 ...][checksum]
pub fn mouse_request(cmd: u8, sub: u8) -> Vec<u8> {
    let mut req = vec![0u8; 65];
    req[1] = cmd;
    req[3] = 0x81;
    req[4] = sub;
    req[64] = 0xA1u8.wrapping_sub(sum(&req[1..64]));
    req
}

fn mouse_query(port: &mut dyn Dev, cmd: u8, sub: u8) -> Option<Vec<u8>> {
    port.flush();
    if !port.write(&mouse_request(cmd, sub)) {
        return None;
    }
    wait(
        port,
        REPLY,
        |r| r.len() < 13,
        |r| r[0] == cmd && r[3] == sub,
    )
}

fn clean_name(product: &str) -> String {
    if product.is_empty() || product.contains("Link") || product.contains("Receiver") {
        "Keychron mouse".into()
    } else {
        product.into()
    }
}

/// Product ids the 4K Link reports for its paired mouse (Launcher's productFirmware table).
fn name_4k(pid: u16) -> Option<&'static str> {
    Some(match pid {
        0x0620 | 0x0622 => "Keychron M3 mini 4K",
        0x0621 => "Keychron M4 4K",
        0x0624 => "Keychron M6 4K",
        0x07A0 => "Keychron M3 4K",
        _ => return None,
    })
}

/// A mouse on the Launcher protocol behind one product, or None.
fn poll_mouse(c: &Coll, port: &mut dyn Dev) -> Option<Device> {
    let r1 = mouse_query(port, 0x01, 0x01);
    let tag = r1.as_ref().map_or(0, |r| u16::from_le_bytes([r[6], r[7]]));
    let key = format!("keychron:{:04X}", c.pid);
    if r1.is_some() && c.page == 0xFF0A && c.usage == 0x01 && tag != 0x3434 && tag != 0x362D {
        // 4K mouse on the 4K Link receiver (Launcher protocol "4k"); the 8K layout does not apply.
        let r = mouse_query(port, 0x41, 0x01).filter(|r| (1..=100).contains(&r[6]))?;
        let mut d = device(key, &clean_name(&c.product), Kind::Mouse, RECEIVER);
        if let Some(n) =
            mouse_query(port, 0x40, 0x00).and_then(|st| name_4k(u16::from_le_bytes([st[4], st[5]])))
        {
            d.name = n.into();
        }
        d.percent = Some(r[6]);
        d.charging = r[5] != 0;
        return Some(d);
    }
    let (percent, charging, wireless) = match &r1 {
        // M4 and other mice on the classic protocol: the reply carries the vendor id.
        Some(r1) if tag == 0x3434 && r1[11] <= 100 => (r1[11], r1[10] != 0, true),
        _ => match mouse_query(port, 0x41, 0x01).filter(|r| r[10] <= 3 && r[11] <= 100) {
            // 8K mouse on its receiver.
            Some(r) => (r[11], r[10] == 1, true),
            // 8K mouse on the USB cable (no bit 6); byte 5 = highest polling rate level.
            None => {
                let r1 = r1.filter(|r| {
                    r[10] <= 3 && (1..=100).contains(&r[11]) && (1..=8).contains(&r[5])
                })?;
                (r1[11], r1[10] == 1, false)
            }
        },
    };
    let via = if wireless { RECEIVER } else { CABLE };
    let mut d = device(key, &clean_name(&c.product), Kind::Mouse, via);
    d.percent = Some(percent);
    d.charging = charging;
    Some(d)
}

// --- Raw HID (FF60:0061, report id 0 + 32 bytes) -----------------------------------------------

fn raw_request(cmd: &[u8]) -> Vec<u8> {
    let mut req = vec![0u8; 33];
    req[1..=cmd.len()].copy_from_slice(cmd);
    req
}

/// Nape Pro channel: skips short reports and notifications until a reply echoing `cmd`.
fn raw_query(port: &mut dyn Dev, cmd: &[u8]) -> Option<Vec<u8>> {
    port.flush();
    if !port.write(&raw_request(cmd)) {
        return None;
    }
    wait(
        port,
        REPLY,
        |_| false,
        |r| r.len() >= cmd.len() + 2 && r.starts_with(cmd),
    )
}

/// Keychron Link channel: a short report ends the wait, a reply starts with `cmd`.
fn link_query(port: &mut dyn Dev, cmd: u8, ms: u64) -> Option<Vec<u8>> {
    port.flush();
    if !port.write(&raw_request(&[cmd])) {
        return None;
    }
    wait(
        port,
        Duration::from_millis(ms),
        |r| r.len() < 8,
        |r| r[0] == cmd,
    )
}

fn is_raw_hid(c: &Coll) -> bool {
    c.page == 0xFF60 && c.usage == 0x61 && c.out_len == 33
}

/// Nape Pro trackball on the Link-KM receiver or its USB cable.
fn poll_nape(colls: &[Coll], hid: &mut dyn Hid) -> Option<(u16, Device)> {
    for pid in [LINK_KM, NAPE_USB] {
        for c in colls.iter().filter(|c| c.is(VID, pid) && is_raw_hid(c)) {
            let Some(mut port) = hid.open(c) else {
                continue;
            };
            if pid == LINK_KM {
                // Only ask when a Nape Pro sits in a receiver slot and is connected.
                let Some(r) = raw_query(port.as_mut(), &[0xB2]).filter(|r| r.len() >= 17) else {
                    continue;
                };
                let mut connected = false;
                for off in [2, 7, 12] {
                    if u16::from_be_bytes([r[off + 2], r[off + 3]]) == NAPE_IN_SLOT {
                        connected = r[off + 4] == 1;
                    }
                }
                if !connected {
                    continue;
                }
            }
            let Some(r) = raw_query(port.as_mut(), &[0xA7, 0x31]).filter(|r| r[2] <= 100) else {
                continue;
            };
            let via = if pid == LINK_KM { RECEIVER } else { CABLE };
            let mut d = device(
                "keychron:nape".into(),
                "Keychron Nape Pro",
                Kind::Mouse,
                via,
            );
            d.percent = Some(r[2]);
            d.charging = r[3] != 0;
            return Some((pid, d));
        }
    }
    None
}

fn legacy_name(pid: u16) -> &'static str {
    match pid {
        0xD032 | 0xD033 => "Keychron M3",
        0xD035 => "Keychron M1",
        0xD036 | 0xD03E => "Keychron M3 mini",
        0xD03B => "Keychron M2",
        0xD03D => "Keychron M2 mini",
        0xD03F => "Keychron M6",
        0xD044 => "Keychron M7",
        _ => "Keychron mouse",
    }
}

/// Status reply of the 1K receiver: (mouse pid, percent, charging).
pub fn parse_legacy(r: &[u8]) -> Option<(u16, u8, bool)> {
    if r.len() < 13 || r[0] != 0x51 || r[1] != 0x06 || r[2] != 0x01 {
        return None;
    }
    if u16::from_le_bytes([r[4], r[5]]) != VID || r[11] > 100 {
        return None;
    }
    Some((u16::from_le_bytes([r[6], r[7]]), r[11], r[12] == 0x01))
}

/// Older mice on the 1K receiver (D030 Type-A, D031 Type-C).
fn poll_legacy(colls: &[Coll], hid: &mut dyn Hid, out: &mut Vec<(u16, Device)>) {
    for rpid in [LEGACY_A, LEGACY_C] {
        for c in colls
            .iter()
            .filter(|c| c.is(VID, rpid) && c.page == 0x8C && c.feat_len >= 16)
        {
            let Some(mut port) = hid.open(c) else {
                continue;
            };
            if !port.set_feature(&[0x51, 0x06]) {
                continue;
            }
            // The reply is usually there at once; give the mouse up to ~200 ms.
            let mut reply = None;
            for _ in 0..5 {
                port.sleep(Duration::from_millis(40));
                reply = port
                    .get_feature_len(0x51, c.feat_len as usize)
                    .filter(|r| r.len() >= 13 && r[0] == 0x51 && r[1] == 0x06);
                if reply.is_some() {
                    break;
                }
            }
            let Some((mpid, percent, charging)) = reply.as_deref().and_then(parse_legacy) else {
                continue;
            };
            let mut d = device(
                format!("keychron:{mpid:04X}"),
                legacy_name(mpid),
                Kind::Mouse,
                RECEIVER,
            );
            d.percent = Some(percent);
            d.charging = charging;
            out.push((rpid, d));
            break;
        }
    }
}

/// Numbered report `id` [cmd] on FFC1; waits for an input report whose payload starts with cmd.
fn ultra_query(port: &mut dyn Dev, id: u8, cmd: u8) -> Option<Vec<u8>> {
    port.flush();
    if !port.write(&[id, cmd]) {
        return None;
    }
    wait(port, REPLY, |_| false, |r| r.len() >= 21 && r[1] == cmd)
}

/// 8K mouse on the Ultra-Link 8K receiver (Launcher protocol "8k" on FFC1:0001).
fn poll_ultra(colls: &[Coll], hid: &mut dyn Hid) -> Option<Device> {
    for c in colls.iter().filter(|c| {
        c.is(VID, ULTRA_LINK_8K)
            && c.page == 0xFFC1
            && c.usage == 0x01
            && c.out_len >= 21
            && c.in_len >= 21
    }) {
        let Some(mut port) = hid.open(c) else {
            continue;
        };
        // Only ask when a mouse sits in a receiver slot and is connected.
        let Some(r) = ultra_query(port.as_mut(), 0xB5, 0x03) else {
            continue;
        };
        // Payload offsets 2/7/12 (+1 for the report id): vid LE16, pid LE16, status.
        let connected = [3, 8, 13]
            .iter()
            .any(|&off| (r[off] | r[off + 1]) != 0 && r[off + 4] != 0);
        if !connected {
            continue;
        }
        let Some(r) = ultra_query(port.as_mut(), 0xB3, 0x06) else {
            continue;
        };
        let power = r[20]; // payload byte 19
        let percent = power & 0x7F;
        if !(1..=100).contains(&percent) {
            continue;
        }
        let mut d = device(
            format!("keychron:{ULTRA_LINK_8K:04X}"),
            &clean_name(&c.product),
            Kind::Mouse,
            RECEIVER,
        );
        d.percent = Some(percent);
        d.charging = power & 0x80 != 0;
        return Some(d);
    }
    None
}

/// Keychron keyboards with the KC_GET_BATTERY patch, behind a Keychron Link receiver or on the cable.
fn poll_link_keyboard(colls: &[Coll], hid: &mut dyn Hid, pid: u16) -> Option<Device> {
    if pid == LINK_KM || pid == NAPE_USB {
        return None; // Nape Pro receiver / cable
    }
    for c in colls.iter().filter(|c| c.is(VID, pid) && is_raw_hid(c)) {
        let Some(mut port) = hid.open(c) else {
            continue;
        };
        let port = port.as_mut();
        let via_link = link_query(port, 0xB2, 300);
        let via = if via_link.is_some() { RECEIVER } else { CABLE };
        let (kb_pid, r) = match via_link {
            Some(r) => {
                let slots = r[1].min(5) as usize;
                let kb_pid = (0..slots)
                    .take_while(|i| 2 + 5 * i + 4 < r.len())
                    .map(|i| &r[2 + 5 * i..2 + 5 * i + 5])
                    .find(|s| u16::from_le_bytes([s[0], s[1]]) == VID && s[4] != 0)
                    .map(|s| u16::from_le_bytes([s[2], s[3]]))?; // keyboard off, asleep or on its cable
                // After an idle spell the first request is lost while the link wakes up.
                let r = link_query(port, 0xA4, 400).or_else(|| link_query(port, 0xA4, 400))?;
                (kb_pid, r)
            }
            // Not a receiver (a keyboard answers B2 with VIA's "unhandled" FF): ask it directly.
            None => match link_query(port, 0xA4, 400) {
                Some(r) => (pid, r),
                None => continue, // stock firmware answers FF, never A4
            },
        };
        if r[1] > 100 {
            return None;
        }
        let name = if (0x0A50..=0x0A58).contains(&kb_pid) {
            "Keychron K5 Max"
        } else {
            "Keychron keyboard"
        };
        let mut d = device(
            format!("keychron-kb:{kb_pid:04X}"),
            name,
            Kind::Keyboard,
            via,
        );
        d.percent = Some(r[1]);
        let mv = u16::from_le_bytes([r[2], r[3]]);
        d.millivolts = (mv != 0).then_some(mv);
        d.charging = r[4] == 1; // 0 on battery, 1 charging, 2 full
        return Some(d);
    }
    None
}

pub fn poll_with(colls: &[Coll], hid: &mut dyn Hid) -> Vec<Device> {
    let mut found: Vec<(u16, Device)> = Vec::new();
    found.extend(poll_nape(colls, hid));
    poll_legacy(colls, hid, &mut found);
    found.extend(poll_ultra(colls, hid).map(|d| (ULTRA_LINK_8K, d)));

    let mut pids: Vec<u16> = colls
        .iter()
        .filter(|c| c.vid == VID)
        .map(|c| c.pid)
        .collect();
    pids.sort_unstable();
    pids.dedup();
    for pid in pids {
        if found.iter().any(|(p, _)| *p == pid) {
            continue; // already read above
        }
        for c in colls
            .iter()
            .filter(|c| c.is(VID, pid) && c.page >= 0xFF00 && c.out_len == 65 && c.in_len == 65)
        {
            let Some(mut port) = hid.open(c) else {
                continue;
            };
            if let Some(d) = poll_mouse(c, port.as_mut()) {
                found.push((pid, d));
                break;
            }
        }
        found.extend(poll_link_keyboard(colls, hid, pid).map(|d| (pid, d)));
    }
    found.into_iter().map(|(_, d)| d).collect()
}

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    hid::run(api, infos, |hid, colls| poll_with(colls, hid))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::hid::fake::{Fake, coll, feature_script, per_write, responder};

    /// Feature reads go to `feature(state, path, id, len)`; a flush drops queued replies.
    use std::collections::HashMap;

    /// Replies per (cmd, sub) of the 65-byte requests, like BatteryHub's scripted device.
    fn mouse_script(replies: Vec<((u8, u8), Vec<u8>)>) -> Fake {
        let table: HashMap<(u8, u8), Vec<u8>> = replies.into_iter().collect();
        responder(move |w| {
            assert_eq!(w.len(), 65);
            assert_eq!(w[0], 0);
            table.get(&(w[1], w[4])).cloned().into_iter().collect()
        })
    }

    /// 64-byte input report (report id 0 already dropped).
    fn reply(cmd: u8, sub: u8, bytes: &[(usize, u8)]) -> Vec<u8> {
        let mut r = vec![0u8; 64];
        r[0] = cmd;
        r[2] = 0x81;
        r[3] = sub;
        for (i, v) in bytes {
            r[*i] = *v;
        }
        r
    }

    fn mouse_coll(pid: u16) -> Coll {
        let mut c = coll(VID, pid, 0xFF0A, 0x01, 65);
        c.product = "Keychron 4K Link".into();
        c
    }

    fn ask(cmd: u8, sub: u8, checksum: u8) -> Vec<u8> {
        let mut w = vec![0u8; 65];
        w[1] = cmd;
        w[3] = 0x81;
        w[4] = sub;
        w[64] = checksum;
        w
    }

    fn run(s: &Fake, c: Coll) -> Vec<Device> {
        poll_with(&[c], &mut s.clone())
    }

    #[test]
    fn request_checksums() {
        assert_eq!(mouse_request(0x01, 0x01), ask(0x01, 0x01, 0x1E));
        assert_eq!(mouse_request(0x41, 0x01), ask(0x41, 0x01, 0xDE));
        assert_eq!(mouse_request(0x40, 0x00), ask(0x40, 0x00, 0xE0));
    }

    #[test]
    fn link_4k_layout() {
        let s = mouse_script(vec![
            ((0x01, 0x01), reply(0x01, 0x01, &[(6, 0x12), (7, 0x05)])),
            (
                (0x41, 0x01),
                reply(0x41, 0x01, &[(5, 1), (6, 73), (7, 2), (10, 0), (11, 0)]),
            ),
            ((0x40, 0x00), reply(0x40, 0x00, &[(4, 0x21), (5, 0x06)])),
        ]);
        let out = run(&s, mouse_coll(0xD038));
        assert_eq!(
            s.s().writes(),
            vec![
                ask(0x01, 0x01, 0x1E),
                ask(0x41, 0x01, 0xDE),
                ask(0x40, 0x00, 0xE0)
            ]
        );
        assert_eq!(out.len(), 1);
        let d = &out[0];
        assert_eq!(
            (d.key.as_str(), d.name.as_str()),
            ("keychron:D038", "Keychron M4 4K")
        );
        assert_eq!((d.percent, d.charging), (Some(73), true));
        assert_eq!((d.kind, d.via.as_str()), (Kind::Mouse, RECEIVER));

        let s = mouse_script(vec![
            ((0x01, 0x01), reply(0x01, 0x01, &[])),
            ((0x41, 0x01), reply(0x41, 0x01, &[(5, 0), (6, 100)])),
            ((0x40, 0x00), reply(0x40, 0x00, &[(4, 0xA0), (5, 0x07)])),
        ]);
        let d = &run(&s, mouse_coll(0xD038))[0];
        assert_eq!(
            (d.percent, d.charging, d.name.as_str()),
            (Some(100), false, "Keychron M3 4K")
        );

        // No 40 reply, or an unknown product id: the generic name, no invented model.
        let s = mouse_script(vec![
            ((0x01, 0x01), reply(0x01, 0x01, &[])),
            ((0x41, 0x01), reply(0x41, 0x01, &[(5, 2), (6, 40)])),
        ]);
        let d = &run(&s, mouse_coll(0xD038))[0];
        assert_eq!(
            (d.percent, d.charging, d.name.as_str()),
            (Some(40), true, "Keychron mouse")
        );
        let s = mouse_script(vec![
            ((0x01, 0x01), reply(0x01, 0x01, &[])),
            ((0x41, 0x01), reply(0x41, 0x01, &[(6, 55)])),
            ((0x40, 0x00), reply(0x40, 0x00, &[(4, 0x99), (5, 0x09)])),
        ]);
        assert_eq!(run(&s, mouse_coll(0xD038))[0].name, "Keychron mouse");
    }

    #[test]
    fn link_4k_without_battery_reports_nothing() {
        for bad in [0u8, 101, 0xFF] {
            let s = mouse_script(vec![
                ((0x01, 0x01), reply(0x01, 0x01, &[])),
                (
                    (0x41, 0x01),
                    reply(0x41, 0x01, &[(5, 1), (6, bad), (10, 1), (11, 50)]),
                ),
            ]);
            assert!(run(&s, mouse_coll(0xD038)).is_empty());
            assert_eq!(
                s.s().writes(),
                vec![ask(0x01, 0x01, 0x1E), ask(0x41, 0x01, 0xDE)]
            );
        }
        let s = mouse_script(vec![]);
        assert!(run(&s, mouse_coll(0xD038)).is_empty());
        assert_eq!(
            s.s().writes(),
            vec![ask(0x01, 0x01, 0x1E), ask(0x41, 0x01, 0xDE)]
        );
        let s = mouse_script(vec![((0x01, 0x01), reply(0x01, 0x01, &[]))]);
        assert!(run(&s, mouse_coll(0xD038)).is_empty());
    }

    #[test]
    fn nordic_8k_layouts() {
        // Tag 34 34: classic path, battery at byte 11, charging at byte 10, only 01 is sent.
        let s = mouse_script(vec![
            (
                (0x01, 0x01),
                reply(
                    0x01,
                    0x01,
                    &[(5, 0), (6, 0x34), (7, 0x34), (10, 1), (11, 64)],
                ),
            ),
            ((0x41, 0x01), reply(0x41, 0x01, &[(6, 99)])),
        ]);
        let out = run(&s, mouse_coll(0xD028));
        assert_eq!(s.s().writes(), vec![ask(0x01, 0x01, 0x1E)]);
        assert_eq!(
            (out[0].key.as_str(), out[0].percent, out[0].charging),
            ("keychron:D028", Some(64), true)
        );
        assert_eq!(out[0].name, "Keychron mouse");

        // Tag 2D 36: the 8K receiver layout of the 41 reply, not the 4K one.
        let s = mouse_script(vec![
            ((0x01, 0x01), reply(0x01, 0x01, &[(6, 0x2D), (7, 0x36)])),
            (
                (0x41, 0x01),
                reply(0x41, 0x01, &[(5, 1), (6, 99), (10, 3), (11, 81)]),
            ),
            ((0x40, 0x00), reply(0x40, 0x00, &[(4, 0x21), (5, 0x06)])),
        ]);
        let out = run(&s, mouse_coll(0xD028));
        assert_eq!(
            s.s().writes(),
            vec![ask(0x01, 0x01, 0x1E), ask(0x41, 0x01, 0xDE)]
        );
        assert_eq!((out[0].percent, out[0].charging), (Some(81), false));
    }

    #[test]
    fn other_collections_are_never_opened() {
        let mut short = mouse_coll(0xD038);
        short.in_len = 33;
        let mut mouse_page = mouse_coll(0xD038);
        mouse_page.page = 0x01;
        for c in [short, mouse_page] {
            let s = mouse_script(vec![((0x01, 0x01), reply(0x01, 0x01, &[]))]);
            assert!(run(&s, c).is_empty());
            assert!(s.s().opened.is_empty());
        }
    }

    fn legacy_coll(pid: u16) -> Coll {
        let mut c = coll(VID, pid, 0x8C, 0x01, 0);
        c.feat_len = 1001;
        c
    }

    fn status(ok: u8, vid: u16, pid: u16, battery: u8, charging: u8) -> Vec<u8> {
        let v = vid.to_le_bytes();
        let p = pid.to_le_bytes();
        vec![
            0x51, 0x06, ok, 0, v[0], v[1], p[0], p[1], 0, 0, 0, battery, charging,
        ]
    }

    /// GetFeature 51 reads back the reply once SetFeature 51 06 queued it (zeros before).
    fn receiver_1k(reply: Vec<u8>) -> Fake {
        feature_script(move |log, _, id, len| {
            assert_eq!(id, 0x51);
            let mut out = vec![0u8; len];
            out[0] = id;
            let queued = log.features().iter().any(|f| f[..] == [0x51, 0x06]);
            if queued && !reply.is_empty() {
                out[..reply.len()].copy_from_slice(&reply);
            }
            Some(out)
        })
    }

    #[test]
    fn receiver_1k_status() {
        let s = receiver_1k(status(0x01, VID, 0xD033, 87, 0x00));
        let out = run(&s, legacy_coll(LEGACY_A));
        assert_eq!(s.s().features(), vec![vec![0x51, 0x06]]);
        assert!(s.s().writes().is_empty());
        let d = &out[0];
        assert_eq!(
            (d.key.as_str(), d.name.as_str()),
            ("keychron:D033", "Keychron M3")
        );
        assert_eq!(
            (d.percent, d.charging, d.kind),
            (Some(87), false, Kind::Mouse)
        );

        let s = receiver_1k(status(0x01, VID, 0xD0FF, 100, 0x01));
        let d = &run(&s, legacy_coll(LEGACY_C))[0];
        assert_eq!(
            (d.percent, d.charging, d.name.as_str()),
            (Some(100), true, "Keychron mouse")
        );

        for bad in [
            status(0x00, VID, 0xD033, 50, 0),
            status(0x01, 0x1234, 0xD033, 50, 0),
            status(0x01, VID, 0xD033, 101, 0),
            vec![],
        ] {
            assert!(run(&receiver_1k(bad), legacy_coll(LEGACY_A)).is_empty());
        }
        let mut no_feature = legacy_coll(LEGACY_A);
        no_feature.feat_len = 0;
        for c in [no_feature, legacy_coll(0xD038)] {
            let s = receiver_1k(status(0x01, VID, 0xD033, 87, 0));
            assert!(run(&s, c).is_empty());
            assert!(s.s().features().is_empty());
        }
    }

    fn ultra_coll(page: u16) -> Coll {
        coll(VID, ULTRA_LINK_8K, page, 0x01, 64)
    }

    /// [report id][cmd][payload...]; `bytes` index the payload (cmd at 0).
    fn ultra_reply(id: u8, cmd: u8, bytes: &[(usize, u8)]) -> Vec<u8> {
        let mut r = vec![0u8; 64];
        r[0] = id;
        r[1] = cmd;
        for (i, v) in bytes {
            r[i + 1] = *v;
        }
        r
    }

    fn ultra_slots(slot: usize, status: u8) -> Vec<u8> {
        let off = 2 + slot * 5;
        ultra_reply(
            0xB5,
            0x03,
            &[
                (off, 0x34),
                (off + 1, 0x34),
                (off + 2, 0x49),
                (off + 3, 0xD0),
                (off + 4, status),
            ],
        )
    }

    fn ultra_script(slots: Option<Vec<u8>>, base: Option<Vec<u8>>) -> Fake {
        responder(move |w| {
            assert_eq!(w.len(), 2);
            match (w[0], w[1]) {
                (0xB5, 0x03) => slots.clone().into_iter().collect(),
                (0xB3, 0x06) => base.clone().into_iter().collect(),
                _ => panic!("unexpected request {w:02X?}"),
            }
        })
    }

    #[test]
    fn ultra_link_8k() {
        let s = ultra_script(
            Some(ultra_slots(1, 1)),
            Some(ultra_reply(0xB3, 0x06, &[(19, 0xE4)])),
        );
        let out = run(&s, ultra_coll(0xFFC1));
        assert_eq!(s.s().writes(), vec![vec![0xB5, 0x03], vec![0xB3, 0x06]]);
        let d = &out[0];
        assert_eq!(
            (d.key.as_str(), d.percent, d.charging),
            ("keychron:D028", Some(100), true)
        );
        assert_eq!(
            (d.name.as_str(), d.via.as_str()),
            ("Keychron mouse", RECEIVER)
        );

        let s = ultra_script(
            Some(ultra_slots(0, 1)),
            Some(ultra_reply(0xB3, 0x06, &[(19, 0x37)])),
        );
        let d = &run(&s, ultra_coll(0xFFC1))[0];
        assert_eq!((d.percent, d.charging), (Some(55), false));

        // No connected slot: B3 06 is never sent.
        let s = ultra_script(
            Some(ultra_slots(0, 0)),
            Some(ultra_reply(0xB3, 0x06, &[(19, 50)])),
        );
        assert!(run(&s, ultra_coll(0xFFC1)).is_empty());
        assert_eq!(s.s().writes(), vec![vec![0xB5, 0x03]]);
        let s = ultra_script(None, Some(ultra_reply(0xB3, 0x06, &[(19, 50)])));
        assert!(run(&s, ultra_coll(0xFFC1)).is_empty());
        for bad in [0x00u8, 0x80, 101, 0xFF] {
            let s = ultra_script(
                Some(ultra_slots(2, 1)),
                Some(ultra_reply(0xB3, 0x06, &[(19, bad)])),
            );
            assert!(run(&s, ultra_coll(0xFFC1)).is_empty());
        }
        let s = ultra_script(Some(ultra_slots(0, 1)), None);
        assert!(run(&s, ultra_coll(0xFFC1)).is_empty());

        let s = ultra_script(Some(ultra_slots(0, 1)), None);
        assert!(run(&s, ultra_coll(0xFF0B)).is_empty());
        assert!(s.s().opened.is_empty());
    }

    fn raw_reply(head: &[u8]) -> Vec<u8> {
        let mut r = head.to_vec();
        r.resize(32, 0);
        r
    }

    #[test]
    fn nape_pro_on_link_km() {
        // Slot 1 holds the Nape Pro (pid 4004 big endian), connected.
        let mut slots = raw_reply(&[0xB2]);
        slots[7..12].copy_from_slice(&[0x34, 0x34, 0x40, 0x04, 1]);
        let s = responder(move |w| match &w[1..3] {
            [0xB2, 0] => vec![slots.clone()],
            [0xA7, 0x31] => vec![vec![0xA7, 0x31], raw_reply(&[0xA7, 0x31, 66, 1])],
            _ => vec![],
        });
        let out = run(&s, coll(VID, LINK_KM, 0xFF60, 0x61, 33));
        assert_eq!(s.s().writes()[1], raw_request(&[0xA7, 0x31]));
        let d = &out[0];
        assert_eq!(
            (d.key.as_str(), d.percent, d.charging),
            ("keychron:nape", Some(66), true)
        );
        assert_eq!(d.via, RECEIVER);
    }

    #[test]
    fn link_keyboard_with_battery_patch() {
        // Receiver slot 0: 3434:0A52 connected; A4 lost once while the link wakes up.
        let mut slots = raw_reply(&[0xB2, 1]);
        slots[2..7].copy_from_slice(&[0x34, 0x34, 0x52, 0x0A, 1]);
        let s = per_write(vec![
            vec![slots],
            vec![],
            vec![raw_reply(&[0xA4, 77, 0x10, 0x0F, 1])],
        ]);
        let out = run(&s, coll(VID, 0xD030, 0xFF60, 0x61, 33));
        let d = &out[0];
        assert_eq!(
            (d.key.as_str(), d.name.as_str()),
            ("keychron-kb:0A52", "Keychron K5 Max")
        );
        assert_eq!(
            (d.percent, d.millivolts, d.charging),
            (Some(77), Some(0x0F10), true)
        );
        assert_eq!((d.kind, d.via.as_str()), (Kind::Keyboard, RECEIVER));

        // A keyboard on its cable answers B2 with FF, then A4 itself.
        let s = per_write(vec![
            vec![raw_reply(&[0xFF])],
            vec![raw_reply(&[0xA4, 50, 0, 0, 0])],
        ]);
        let d = &run(&s, coll(VID, 0x0A51, 0xFF60, 0x61, 33))[0];
        assert_eq!(
            (d.key.as_str(), d.via.as_str(), d.percent),
            ("keychron-kb:0A51", CABLE, Some(50))
        );

        // Stock firmware never answers A4.
        let s = per_write(vec![vec![raw_reply(&[0xFF])], vec![raw_reply(&[0xFF])]]);
        assert!(run(&s, coll(VID, 0x0A51, 0xFF60, 0x61, 33)).is_empty());
    }
}

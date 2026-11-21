//! EWEADN S7 / S9 mice on EWEADN's vendor id 372E: the "Gaming 2.4G" receiver 103D (S7 / S9 Max,
//! Pro, Ultra) and the S9 Ultra on its USB cable (1027). 64-byte feature reports with report id 3 on
//! FF00:0001, read back with GET_FEATURE ~100 ms later:
//!   request [03][sum][50][len][00][cmd][sub][00 ...], sum = (sum of the 63 bytes after the id) & FF
//!   reply   [03][50][ack][len][00][sub][data ...], ack 0 = OK
//!
//! 47 00 (checkWirelessOnline): data[0] = 1 when the mouse is linked; 4F 81 (getBasics): data[0]
//! bit 7 = charging, bits 0-6 = percent. Only these two read commands are sent. 372E is shared
//! with AULA and VGN, so only the two EWEADN ids are declared.
//!
//! Ported from BatteryHub (src/drivers/eweadn.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0, from EWEADN's web driver
//! (new-hub.eweadn.cn). BatteryHub keyed the device by its HID path; here the key is the product id.

use std::time::{Duration, Instant};

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::hid::{self, BUDGET, Coll, Dev, Hid, find, hex4, left, pids};
use crate::registry::Match;

const VID: u16 = 0x372E;
const RECEIVER: u16 = 0x103D;
const S9_ULTRA_CABLE: u16 = 0x1027;
const REPORT_ID: u8 = 3;

pub static MATCHES: &[Match] = &[Match {
    vid: VID,
    pids: &[RECEIVER, S9_ULTRA_CABLE],
}];

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    hid::run(api, infos, |hid, colls| poll_with(colls, hid))
}

pub fn request(cmd: u8, sub: u8) -> [u8; 64] {
    let mut q = [0u8; 64];
    q[0] = REPORT_ID;
    q[2] = 0x50; // CMD_TYPE
    q[5] = cmd;
    q[6] = sub;
    q[1] = q[1..].iter().fold(0u8, |a, b| a.wrapping_add(*b));
    q
}

/// Data block of a single-packet OK reply.
pub fn parse(r: &[u8]) -> Option<Vec<u8>> {
    if r.len() < 8 || r[0] != REPORT_ID || r[2] != 0 || r[4] != 0 {
        return None;
    }
    let len = r[3] as usize;
    (len > 0 && 6 + len <= r.len()).then(|| r[6..6 + len].to_vec())
}

/// getBasics data[0]: (percent, charging).
pub fn battery(b: u8) -> Option<(u8, bool)> {
    let level = b & 0x7F;
    (level <= 100).then_some((level, b & 0x80 != 0))
}

fn query(d: &mut dyn Dev, io: &mut dyn Hid, cmd: u8, sub: u8) -> Option<Vec<u8>> {
    if !d.set_feature(&request(cmd, sub)) {
        return None;
    }
    for _ in 0..3 {
        io.sleep(Duration::from_millis(100));
        if let Some(data) = d.get_feature(REPORT_ID).and_then(|r| parse(&r)) {
            return Some(data);
        }
    }
    None
}

fn poll_with(colls: &[Coll], io: &mut dyn Hid) -> Vec<Device> {
    let end = Instant::now() + BUDGET;
    let mut out = Vec::new();
    for pid in pids(colls, VID) {
        if pid != RECEIVER && pid != S9_ULTRA_CABLE {
            continue;
        }
        let cable = pid == S9_ULTRA_CABLE;
        for c in find(colls, VID, pid, |c| {
            c.page == 0xFF00 && c.usage == 1 && c.feat_len == 64
        }) {
            if left(end).is_zero() {
                return out;
            }
            let Some(mut d) = io.open(c) else { continue };
            let key = format!("eweadn:{}:{}", hex4(VID), hex4(pid));
            let (name, via) = if cable {
                ("EWEADN S9 Ultra", "USB cable")
            } else {
                ("EWEADN wireless mouse", "2.4 GHz receiver")
            };
            if !cable {
                let Some(online) = query(d.as_mut(), io, 0x47, 0x00) else {
                    continue;
                };
                if online[0] != 1 {
                    // Receiver present, mouse off or out of range: hidden at once.
                    out.push(Device::switched_off(key, name, Kind::Mouse, via));
                    break;
                }
            }
            let Some((pct, charging)) =
                query(d.as_mut(), io, 0x4F, 0x81).and_then(|data| battery(data[0]))
            else {
                continue;
            };
            let mut s = Device::new(key, name, Kind::Mouse, via);
            s.percent = Some(pct);
            s.charging = charging;
            s.grace = Duration::from_secs(600);
            out.push(s);
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::hid::fake::{self, Fake, iface_coll as coll};

    fn reply(sub: u8, data: &[u8]) -> Vec<u8> {
        let mut r = vec![0u8; 64];
        r[0] = 3;
        r[1] = 0x50;
        r[3] = data.len() as u8;
        r[5] = sub;
        r[6..6 + data.len()].copy_from_slice(data);
        r
    }

    #[test]
    fn request_checksum_covers_the_63_bytes() {
        let q = request(0x4F, 0x81);
        assert_eq!(
            q[..7],
            [
                3,
                (0x50u8 + 0x4F).wrapping_add(0x81),
                0x50,
                0,
                0,
                0x4F,
                0x81
            ]
        );
    }

    #[test]
    fn reads_the_mouse_behind_the_receiver() {
        let mut f = Fake::with(|s, fc, op| {
            if let Some(q) = op.sent() {
                let r = if q[5] == 0x47 {
                    reply(0, &[1])
                } else {
                    reply(0x81, &[0x80 | 57])
                };
                s.push_feature("", r);
            }
            fake::queued(s, fc, op)
        });
        f.s().pad_features = true;
        let out = poll_with(&[coll(VID, RECEIVER, 2, 0xFF00, 1, 0, 0, 64)], &mut f);
        assert_eq!(out.len(), 1);
        assert_eq!((out[0].percent, out[0].charging), (Some(57), true));
        assert_eq!(out[0].key, "eweadn:372E:103D");
    }

    #[test]
    fn offline_mouse_and_bad_replies() {
        let mut f = Fake::with(|s, fc, op| {
            if op.sent().is_some() {
                s.push_feature("", reply(0, &[0]));
            }
            fake::queued(s, fc, op)
        });
        f.s().pad_features = true;
        let out = poll_with(&[coll(VID, RECEIVER, 2, 0xFF00, 1, 0, 0, 64)], &mut f);
        assert_eq!(out.len(), 1);
        assert!(out[0].off && out[0].percent.is_none());
        assert_eq!(out[0].key, "eweadn:372E:103D");
        assert_eq!(
            f.s().sends(),
            1,
            "no battery read while the mouse is offline"
        );
        let mut nak = reply(0x81, &[50]);
        nak[2] = 1;
        assert!(parse(&nak).is_none());
        assert!(battery(0x7F).is_none());
    }
}

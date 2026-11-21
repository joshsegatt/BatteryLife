//! MCHOSE "new" mice on VID 3837: the K7 V2 Ultra+ on its receiver (1014) or USB cable (4028), the
//! other Nordic receivers M HUB groups with it (1018, 1016, 102C, 102D), and the K87 V3 keyboard
//! receiver (1015). One read-only "device info" frame, output / input report 4D:
//!   request [4D][01][01 = read][len 00][cmd 00 09][biz][seq][xor of bytes 2..7]
//!   reply   [4D][01][flags][len][00 09][biz][seq][data ...][xor of bytes 2..7+len]
//!   data[0..1] vendor 3837, [2..3] paired model, [10] link (0 none, 1 cable, 2 receiver),
//!   [11] charging, [12] percent. A mouse that is off answers with len 1 and FF: shown asleep.
//!
//! The K87 V3 uses the same command on FF80:0001 with a 20-byte payload (paired model 2023); it is
//! asked at most every three minutes so an idle keyboard is not kept awake.
//!
//! Ported from BatteryHub (src/drivers/mchose_modern.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0, from MCHOSE's M HUB web
//! driver and github.com/HifumiDaisuki01/MchoseBatteryTray.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::hid::{self, BUDGET, Coll, Dev, Hid, find, hex4, left};
use crate::registry::Match;

const VID: u16 = 0x3837;
const REPORT: u8 = 0x4D;
const CMD_DEVICE_INFO: u16 = 0x0900;
/// First byte of the data block in both the request and the reply.
const DATA: usize = 8;
const KEYBOARD: u16 = 0x1015;
const KEYBOARD_EVERY: Duration = Duration::from_secs(3 * 60);

/// (pid, via, keyboard)
const TARGETS: [(u16, &str, bool); 7] = [
    (0x1014, "2.4 GHz receiver", false),
    (0x1018, "2.4 GHz receiver", false),
    (0x1016, "2.4 GHz receiver", false),
    (0x102C, "2.4 GHz receiver", false),
    (0x102D, "2.4 GHz receiver", false),
    (0x4028, "USB cable", false),
    (KEYBOARD, "2.4 GHz receiver", true),
];

pub static MATCHES: &[Match] = &[Match {
    vid: VID,
    pids: &[0x1014, 0x1018, 0x1016, 0x102C, 0x102D, 0x4028, KEYBOARD],
}];

/// Last keyboard reading by collection, and when it may be asked again.
type KeyboardCache = HashMap<String, (Instant, Device)>;
static KEYBOARDS: Mutex<Option<KeyboardCache>> = Mutex::new(None);

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    let mut cache = KEYBOARDS.lock().unwrap_or_else(|e| e.into_inner());
    hid::run(api, infos, |hid, colls| {
        poll_with(colls, hid, cache.get_or_insert_with(HashMap::new))
    })
}

fn xor(b: &[u8], from: usize, to: usize) -> u8 {
    b.iter().take(to + 1).skip(from).fold(0, |a, x| a ^ x)
}

pub fn request(len: usize) -> Vec<u8> {
    let mut q = vec![0u8; len.max(DATA + 1)];
    q[0] = REPORT;
    q[1] = 0x01;
    q[2] = 0x01;
    q[4..6].copy_from_slice(&CMD_DEVICE_INFO.to_le_bytes());
    q[DATA] = xor(&q, 2, 7);
    q
}

#[derive(Debug, PartialEq)]
pub enum Info {
    /// percent, charging
    Battery(u8, bool),
    /// A valid frame that says the mouse is not linked (off or asleep).
    NotLinked,
}

pub fn parse(r: &[u8], keyboard: bool) -> Option<Info> {
    if r.len() < DATA + 2 || r[0] != REPORT || u16::from_le_bytes([r[4], r[5]]) != CMD_DEVICE_INFO {
        return None;
    }
    let len = r[3] as usize;
    if DATA + len >= r.len() {
        return None;
    }
    // Shorter than the 14-byte payload: the receiver's "not linked" answer (FF in place of the model).
    if len < 14 {
        return (!keyboard || (len == 1 && r[DATA] == 0xFF)).then_some(Info::NotLinked);
    }
    if r[DATA + len] != xor(r, 2, 7 + len) || u16::from_le_bytes([r[DATA], r[DATA + 1]]) != VID {
        return None;
    }
    if keyboard && (len < 20 || r[DATA + 2] != 0x23 || r[DATA + 3] != 0x20) {
        return None;
    }
    if r[DATA + 10] == 0 {
        return Some(Info::NotLinked);
    }
    (r[DATA + 12] <= 100).then_some(Info::Battery(r[DATA + 12], r[DATA + 11] == 1))
}

/// Up to three tries inside one shared 1 s deadline (writes, retry delays and stray reports included).
fn query(
    d: &mut dyn Dev,
    io: &mut dyn Hid,
    out_len: usize,
    keyboard: bool,
    end: Instant,
) -> Option<Info> {
    let deadline = end.min(Instant::now() + Duration::from_millis(1000));
    for attempt in 0..3 {
        if attempt > 0 {
            if left(deadline) <= Duration::from_millis(150) {
                break;
            }
            io.sleep(Duration::from_millis(150));
        }
        if left(deadline).is_zero() {
            break;
        }
        if !d.write(&request(out_len)) {
            continue;
        }
        let until = deadline.min(Instant::now() + Duration::from_millis(250));
        loop {
            let left = left(until);
            if left.is_zero() {
                break;
            }
            let Some(r) = d.read(left) else { break };
            if let Some(info) = parse(&r, keyboard) {
                return Some(info);
            }
        }
    }
    None
}

fn poll_with(colls: &[Coll], io: &mut dyn Hid, keyboards: &mut KeyboardCache) -> Vec<Device> {
    let end = Instant::now() + BUDGET;
    // Forget keyboard receivers that were unplugged.
    keyboards.retain(|path, _| {
        colls
            .iter()
            .any(|c| c.vid == VID && c.pid == KEYBOARD && c.path_str() == *path)
    });
    let mut out = Vec::new();
    for (pid, via, keyboard) in TARGETS {
        for c in find(colls, VID, pid, |c| {
            if keyboard {
                c.page == 0xFF80 && c.usage == 1 && c.out_len == 64 && c.in_len == 64
            } else {
                (c.page == 0xFF01 || c.page == 0xFF0B) && c.out_len >= 64 && c.in_len >= 21
            }
        }) {
            if keyboard
                && let Some((next, s)) = keyboards.get(&c.path_str())
                && Instant::now() < *next
            {
                out.push(s.clone());
                break;
            }
            if left(end).is_zero() {
                return out;
            }
            let Some(mut d) = io.open(c) else { continue };
            d.flush();
            let info = query(d.as_mut(), io, c.out_len as usize, keyboard, end);
            if info.is_none() && !keyboard {
                continue;
            }
            let mut name = c.product_or(if keyboard {
                "MCHOSE K87 V3"
            } else {
                "MCHOSE mouse"
            });
            if keyboard && name.len() > 5 && name.ends_with(" 2.4G") {
                name.truncate(name.len() - 5);
            }
            let mut s = Device::new(
                format!("mchose:{}:{}", hex4(VID), hex4(pid)),
                name,
                if keyboard {
                    Kind::Keyboard
                } else {
                    Kind::Mouse
                },
                via,
            );
            s.grace = Duration::from_secs(1800);
            match info {
                Some(Info::Battery(pct, charging)) => {
                    s.percent = Some(pct);
                    s.charging = charging;
                }
                _ => s.asleep = true,
            }
            if keyboard {
                keyboards.insert(c.path_str(), (Instant::now() + KEYBOARD_EVERY, s.clone()));
            }
            out.push(s);
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::hid::fake::{self, Fake, Op, iface_coll as coll};

    fn collection(pid: u16, page: u16, out_len: u16, in_len: u16) -> Coll {
        let mut c = coll(VID, pid, 2, page, 1, in_len, out_len, 0);
        c.product = if pid == KEYBOARD {
            "MCHOSE K87 V3 2.4G".into()
        } else {
            "MCHOSE K7 V2 Ultra+".into()
        };
        c
    }

    /// Device-info reply as the receiver sends it: header, 16 data bytes, checksum.
    fn reply(link: u8, charge: u8, percent: u8) -> Vec<u8> {
        let mut r = vec![0u8; 64];
        r[..6].copy_from_slice(&[0x4D, 0x01, 0x01, 16, 0, 0x09]);
        r[8] = 0x37;
        r[9] = 0x38;
        r[10] = 0x26;
        r[11] = 0x40;
        r[18] = link;
        r[19] = charge;
        r[20] = percent;
        r[24] = xor(&r, 2, 23);
        r
    }

    fn not_linked() -> Vec<u8> {
        let mut r = vec![0u8; 64];
        r[..6].copy_from_slice(&[0x4D, 0x01, 0x01, 0x01, 0, 0x09]);
        r[8] = 0xFF;
        r
    }

    fn fix_checksum(r: &mut [u8]) {
        let len = r[3] as usize;
        r[8 + len] = 0;
        r[8 + len] = xor(r, 2, 7 + len);
    }

    /// Fake that only accepts the documented request and hands out `replies` in order.
    fn device(replies: Vec<Vec<u8>>) -> Fake {
        let f = Fake::with(|s, fc, op| {
            if let Some(q) = op.sent() {
                assert!(matches!(op, Op::Write(_)));
                assert_eq!(q.len(), 64);
                assert_eq!(q[..9], [0x4D, 1, 1, 0, 0, 9, 0, 0, 8]);
                assert!(q[9..].iter().all(|b| *b == 0));
            }
            fake::queued(s, fc, op)
        });
        for r in replies {
            f.s().push_input("", r);
        }
        f
    }

    fn poll1(replies: Vec<Vec<u8>>, c: Coll) -> (Vec<Device>, Fake) {
        let mut f = device(replies);
        let out = poll_with(&[c], &mut f, &mut HashMap::new());
        (out, f)
    }

    #[test]
    fn reads_receiver_and_cable() {
        let (out, f) = poll1(vec![reply(2, 0, 29)], collection(0x1014, 0xFF01, 64, 64));
        assert_eq!(out.len(), 1);
        let s = &out[0];
        assert_eq!((s.percent, s.charging, s.asleep), (Some(29), false, false));
        assert_eq!(
            (s.name.as_str(), s.key.as_str()),
            ("MCHOSE K7 V2 Ultra+", "mchose:3837:1014")
        );
        assert_eq!((s.kind, s.via.as_str()), (Kind::Mouse, "2.4 GHz receiver"));
        assert_eq!(s.grace, Duration::from_secs(1800));
        assert_eq!(f.s().sends(), 1, "one reply is enough, no retries");

        let (out, _) = poll1(vec![reply(2, 1, 100)], collection(0x1014, 0xFF01, 64, 64));
        assert_eq!((out[0].percent, out[0].charging), (Some(100), true));

        let (out, _) = poll1(vec![reply(1, 1, 25)], collection(0x4028, 0xFF01, 64, 64));
        assert_eq!((out[0].percent, out[0].charging), (Some(25), true));
        assert_eq!(
            (out[0].key.as_str(), out[0].via.as_str()),
            ("mchose:3837:4028", "USB cable")
        );
    }

    #[test]
    fn switched_off_mouse_is_asleep() {
        let (out, f) = poll1(vec![not_linked()], collection(0x1014, 0xFF01, 64, 64));
        assert!(out[0].asleep && !out[0].charging && out[0].percent.is_none());
        assert_eq!(f.s().sends(), 1);

        // The 0x13 echo frame after an unrelated write carries no battery either.
        let mut echo = vec![0u8; 64];
        echo[..4].copy_from_slice(&[0x4D, 1, 1, 1]);
        echo[8] = 0xFF;
        let (out, _) = poll1(
            vec![echo, reply(2, 0, 31)],
            collection(0x1014, 0xFF01, 64, 64),
        );
        assert_eq!(out[0].percent, Some(31));
    }

    #[test]
    fn silence_and_corrupt_frames_report_nothing() {
        let (out, f) = poll1(vec![], collection(0x1014, 0xFF01, 64, 64));
        assert!(out.is_empty());
        assert_eq!(f.s().sends(), 3);

        let mut bad_checksum = reply(2, 0, 29);
        bad_checksum[24] ^= 0xFF;
        let bad_percent = reply(2, 0, 101);
        let mut bad_vendor = reply(2, 0, 29);
        bad_vendor[8] = 0x34;
        fix_checksum(&mut bad_vendor);
        let mut bad_report = reply(2, 0, 29);
        bad_report[0] = 0x4E;
        let (out, f) = poll1(
            vec![bad_checksum, bad_percent, bad_vendor, bad_report],
            collection(0x1014, 0xFF01, 64, 64),
        );
        assert!(out.is_empty());
        assert_eq!(f.s().sends(), 3);

        // Too short for the 14-byte payload: "not linked", like the FF answer.
        let mut short = reply(2, 0, 29);
        short[3] = 12;
        fix_checksum(&mut short);
        let (out, _) = poll1(vec![short], collection(0x1014, 0xFF01, 64, 64));
        assert!(out[0].asleep && out[0].percent.is_none());
    }

    #[test]
    fn only_documented_collections_are_opened() {
        for c in [
            collection(0x1012, 0xFF00, 64, 64), // MagDock: another product
            collection(0x1014, 0x0001, 64, 64), // the mouse collection
            collection(0x1014, 0xFF01, 63, 64), // too short to hold the frame
            collection(0x1014, 0xFF01, 64, 20), // reply cannot be read
            collection(KEYBOARD, 0xFF01, 64, 64),
        ] {
            let (out, f) = poll1(vec![], c);
            assert!(out.is_empty());
            assert_eq!(f.s().opened.len(), 0);
        }
        for pid in [0x1018, 0x102D] {
            let (out, _) = poll1(vec![reply(2, 0, 45)], collection(pid, 0xFF0B, 64, 64));
            assert_eq!(out[0].percent, Some(45));
            assert_eq!(out[0].key, format!("mchose:3837:{pid:04X}"));
        }
    }

    fn keyboard_reply() -> Vec<u8> {
        let mut r = reply(2, 1, 98);
        r[3] = 20;
        r[10] = 0x23;
        r[11] = 0x20;
        r[24] = 0;
        fix_checksum(&mut r);
        r
    }

    #[test]
    fn keyboard_is_asked_at_most_every_three_minutes() {
        let kb = collection(KEYBOARD, 0xFF80, 64, 64);
        let mut cache = HashMap::new();
        let mut f = device(vec![keyboard_reply()]);
        let out = poll_with(std::slice::from_ref(&kb), &mut f, &mut cache);
        assert_eq!((out[0].percent, out[0].charging), (Some(98), true));
        assert_eq!(
            (out[0].kind, out[0].name.as_str()),
            (Kind::Keyboard, "MCHOSE K87 V3")
        );
        assert_eq!(out[0].key, "mchose:3837:1015");

        let mut f = device(vec![]);
        let out = poll_with(std::slice::from_ref(&kb), &mut f, &mut cache);
        assert_eq!(out[0].percent, Some(98));
        assert!(f.s().sends() == 0, "no new request within three minutes");

        // Unplugged: the cache is dropped, so the next poll asks again.
        poll_with(&[], &mut device(vec![]), &mut cache);
        let mut f = device(vec![not_linked()]);
        let out = poll_with(std::slice::from_ref(&kb), &mut f, &mut cache);
        assert!(out[0].asleep && out[0].percent.is_none() && !out[0].charging);
        assert_eq!(f.s().sends(), 1);
    }

    #[test]
    fn keyboard_rejects_frames_it_cannot_trust() {
        let kb = collection(KEYBOARD, 0xFF80, 64, 64);
        for bad in 0..4 {
            let mut r = keyboard_reply();
            match bad {
                0 => r[28] ^= 1,
                1 => {
                    r[10] ^= 1;
                    r[28] ^= 1;
                }
                2 => {
                    r[20] = 101;
                    r[28] ^= 98 ^ 101;
                }
                _ => {
                    r[3] = 12;
                    r[8] = 0;
                }
            }
            let (out, _) = poll1(vec![r], kb.clone());
            assert!(out[0].asleep && out[0].percent.is_none() && !out[0].charging);
        }
        let mut c = kb;
        c.usage = 2;
        let (out, f) = poll1(vec![], c);
        assert!(out.is_empty() && f.s().opened.is_empty());
    }
}

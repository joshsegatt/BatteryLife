//! Machenike L7 Arctic / L7 Mix mouse: receiver 248A:FF4C (interface 1), cable 248A:FF4E (interface 2).
//!
//! Vendor collection 008C:0001 (33-byte input / output, 1001-byte feature). Feature report 51,
//! command 06, exactly 21 bytes as the official app sends it: [0] 51 [1] 06 [4..7] 8A 24 4E FF,
//! [10] mode / link (low 3 bits: 0 cable, 1 receiver; bit 3 wireless link up), [11] percent,
//! [12] charging. Read-state protocol from the official L7 Mix app 1.0.0.8.
//!
//! Ported from BatteryHub (src/drivers/machenike.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0.

use std::time::{Duration, Instant};

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::hidport::{
    BUDGET, CABLE, Coll, ExactFeaturePort, Opener, Port, RECEIVER, RECEIVER_GRACE, colls,
};

pub const VID: u16 = 0x248A;
const RECEIVER_PID: u16 = 0xFF4C;
const CABLE_PID: u16 = 0xFF4E;
pub const PIDS: &[u16] = &[RECEIVER_PID, CABLE_PID];
const STATUS_LEN: usize = 21;

fn is_status_collection(c: &Coll, cable: bool) -> bool {
    c.iface == if cable { 2 } else { 1 }
        && c.page == 0x008C
        && c.usage == 1
        && c.in_len == 33
        && c.out_len == 33
        && c.feat_len == 1001
}

#[derive(Debug, PartialEq)]
pub enum Status {
    /// The receiver is present but the mouse is not linked (e.g. it uses its cable).
    Offline,
    /// (percent, charging)
    Online(u8, bool),
}

pub fn decode(r: &[u8], cable: bool) -> Option<Status> {
    if r.len() != STATUS_LEN
        || r[..2] != [0x51, 0x06]
        || r[4..8] != [0x8A, 0x24, 0x4E, 0xFF]
        || r[10] & 7 != u8::from(!cable)
    {
        return None;
    }
    // The wireless link bit is clear on the cable too; the USB connection itself is authoritative there.
    if !(cable || r[10] & 8 != 0) {
        return Some(Status::Offline);
    }
    if r[11] > 100 || r[12] > 1 {
        return None;
    }
    Some(Status::Online(r[11], r[12] == 1)) // 0 % is valid
}

fn query(port: &mut dyn Port, cable: bool) -> Option<Status> {
    let mut request = vec![0u8; STATUS_LEN];
    request[..2].copy_from_slice(&[0x51, 0x06]);
    let deadline = Instant::now() + Duration::from_millis(200);
    if !port.set_feature(&request) {
        return None;
    }
    // An open vendor app can leave unrelated replies here: accept only this command's status frame.
    for _ in 0..20 {
        port.pause(Duration::from_millis(10));
        if let Some(s) = port
            .get_feature(0x51, STATUS_LEN)
            .and_then(|r| decode(&r, cable))
        {
            return Some(s);
        }
        if Instant::now() >= deadline {
            break;
        }
    }
    None
}

pub fn poll_with(colls: &[Coll], open: &mut Opener) -> Vec<Device> {
    // A live cable reading takes precedence over the receiver's cached / disconnected state.
    for pid in [CABLE_PID, RECEIVER_PID] {
        let cable = pid == CABLE_PID;
        let Some(c) = colls
            .iter()
            .find(|c| c.is(VID, pid) && is_status_collection(c, cable) && c.product == "L7 Mix")
        else {
            continue;
        };
        let Some(mut port) = open(c) else { continue };
        let Some((percent, charging)) = (match query(port.as_mut(), cable) {
            Some(Status::Online(p, ch)) => Some((p, ch)),
            _ => None, // no reply, or the receiver without its mouse: BatteryHub hides it
        }) else {
            continue;
        };
        let via = if cable { CABLE } else { RECEIVER };
        // Same entry when switching between cable and receiver.
        let mut d = Device::new(
            "machenike:l7",
            "Machenike L7 Arctic / L7 Mix",
            Kind::Mouse,
            via,
        );
        d.percent = Some(percent);
        d.charging = charging;
        if !cable {
            d.grace = RECEIVER_GRACE;
        }
        return vec![d];
    }
    Vec::new()
}

pub fn poll(_api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    let deadline = Instant::now() + BUDGET;
    // Report 51 is 21 bytes; 1001 is the largest other report on this collection, so the feature
    // reports are sent with their exact length, as the official application does.
    poll_with(&colls(infos), &mut |c: &Coll| {
        ExactFeaturePort::open(c, deadline).map(|p| Box::new(p) as Box<dyn Port>)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::hidport::fake::{self, coll};

    const WIRELESS: [u8; 21] = [
        0x51, 0x06, 0x01, 0x00, 0x8A, 0x24, 0x4E, 0xFF, 0x05, 0x01, 0x09, 0x40, 0x00, 0, 0, 0, 0,
        0, 0, 0, 0,
    ];
    const CABLED: [u8; 21] = [
        0x51, 0x06, 0x01, 0x00, 0x8A, 0x24, 0x4E, 0xFF, 0x05, 0x01, 0x00, 0x4A, 0x01, 0, 0, 0, 0,
        0, 0, 0, 0,
    ];
    const OFFLINE: [u8; 21] = [
        0x51, 0x06, 0x01, 0x00, 0x8A, 0x24, 0x4E, 0xFF, 0x05, 0x01, 0x01, 0x40, 0x01, 0, 0, 0, 0,
        0, 0, 0, 0,
    ];

    fn collection(cable: bool) -> Coll {
        let mut c = coll(
            VID,
            if cable { CABLE_PID } else { RECEIVER_PID },
            0x008C,
            1,
            33,
        );
        c.iface = if cable { 2 } else { 1 };
        c.feat_len = 1001;
        c.product = "L7 Mix".into();
        c
    }

    /// GetFeature 51 returns `replies` in order, then nothing.
    fn device(replies: Vec<Vec<u8>>) -> fake::Shared {
        let s = fake::script();
        let mut queue = replies.into_iter();
        s.borrow_mut().feature = Some(Box::new(move |_, id, len| {
            assert_eq!((id, len), (0x51, STATUS_LEN));
            queue.next()
        }));
        s
    }

    #[test]
    fn captured_replies() {
        assert_eq!(decode(&WIRELESS, false), Some(Status::Online(64, false)));
        assert_eq!(decode(&CABLED, true), Some(Status::Online(74, true)));
        assert_eq!(decode(&OFFLINE, false), Some(Status::Offline));

        let s = device(vec![WIRELESS.to_vec()]);
        let out = poll_with(&[collection(false)], &mut fake::opener(&s));
        let mut request = vec![0u8; 21];
        request[..2].copy_from_slice(&[0x51, 0x06]);
        assert_eq!(s.borrow().features, vec![request]);
        let d = &out[0];
        assert_eq!(
            (d.percent, d.charging, d.via.as_str()),
            (Some(64), false, RECEIVER)
        );
        assert_eq!((d.key.as_str(), d.kind), ("machenike:l7", Kind::Mouse));

        let s = device(vec![CABLED.to_vec()]);
        let d = &poll_with(&[collection(true)], &mut fake::opener(&s))[0];
        assert_eq!(
            (d.percent, d.charging, d.via.as_str()),
            (Some(74), true, CABLE)
        );
        assert_eq!(d.key, "machenike:l7");
    }

    #[test]
    fn zero_and_full_are_levels_and_offline_is_hidden() {
        for pct in [0u8, 100] {
            let mut r = WIRELESS;
            r[11] = pct;
            assert_eq!(decode(&r, false), Some(Status::Online(pct, false)));
        }
        let s = device(vec![OFFLINE.to_vec()]);
        assert!(poll_with(&[collection(false)], &mut fake::opener(&s)).is_empty());
    }

    #[test]
    fn cable_wins() {
        let s = device(vec![CABLED.to_vec()]);
        let out = poll_with(
            &[collection(true), collection(false)],
            &mut fake::opener(&s),
        );
        assert_eq!((out.len(), s.borrow().features.len()), (1, 1));
        assert_eq!(out[0].via, CABLE);
    }

    #[test]
    fn strict_matching() {
        for field in 0..9 {
            let mut c = collection(false);
            match field {
                0 => c.vid = 0x249A,
                1 => c.pid = 0xFF4D,
                2 => c.iface = 2,
                3 => c.page = 0xFF00,
                4 => c.usage = 2,
                5 => c.in_len = 32,
                6 => c.out_len = 32,
                7 => c.feat_len = 1000,
                _ => c.product = "Unrelated mouse".into(),
            }
            let s = device(vec![WIRELESS.to_vec()]);
            assert!(poll_with(&[c], &mut fake::opener(&s)).is_empty());
            assert!(s.borrow().opened.is_empty());
        }
    }

    #[test]
    fn malformed_replies_skipped_before_a_valid_status() {
        let mut wrong_report = WIRELESS;
        wrong_report[0] = 0x52;
        let mut wrong_command = WIRELESS;
        wrong_command[1] = 4;
        let mut wrong_device = WIRELESS;
        wrong_device[6] = 0x4D;
        let mut wrong_mode = WIRELESS;
        wrong_mode[10] = 0x0F;
        let mut bad_percent = WIRELESS;
        bad_percent[11] = 101;
        let mut bad_charge = WIRELESS;
        bad_charge[12] = 0xFF;
        let s = device(vec![
            wrong_report.to_vec(),
            wrong_command.to_vec(),
            wrong_device.to_vec(),
            wrong_mode.to_vec(),
            bad_percent.to_vec(),
            bad_charge.to_vec(),
            vec![0; 12],
            WIRELESS.to_vec(),
        ]);
        let out = poll_with(&[collection(false)], &mut fake::opener(&s));
        assert_eq!(out[0].percent, Some(64));
        assert!(s.borrow().reads >= 8);
    }

    #[test]
    fn missing_reply_is_bounded() {
        let s = device(vec![]);
        let start = Instant::now();
        assert!(poll_with(&[collection(false)], &mut fake::opener(&s)).is_empty());
        assert!(start.elapsed() < Duration::from_millis(800));
    }
}

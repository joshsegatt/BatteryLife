//! Semico (VID 1A2C) tri-mode keyboards on the AA 55 CC 33 vendor protocol: Attack Shark X98 (dongle
//! 1A2C:8FFF, cable 1A2C:A034) and AJAZZ AK820 Max Plus (dongle 1A2C:8FFF, cable 1A2C:A036).
//!
//! Interface 0, usage page FF02 / usage 0002, 65-byte reports (report id 0 + 64 bytes):
//!   request [00 AA 55 CC 33 0E 00 ...]      read-only status query
//!   reply   [AA 55 CC 33 0E percent state]  state 0 = offline, 1..3 = online
//! State 1 shows both while charging and on battery, so no charging flag is derived from it.
//!
//! Ported from BatteryHub (src/drivers/semico.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0, which credits
//! shrey715/openajazz and AkashPanja/Kraber.

use std::time::{Duration, Instant};

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::hidport::{
    BUDGET, CABLE, Coll, Opener, Port, RECEIVER, RECEIVER_GRACE, colls, hid_opener,
};

pub const VID: u16 = 0x1A2C;
const DONGLE: u16 = 0x8FFF;
const X98_CABLE: u16 = 0xA034;
const AK820_CABLE: u16 = 0xA036;
pub const PIDS: &[u16] = &[DONGLE, X98_CABLE, AK820_CABLE];

fn is_cable(pid: u16) -> bool {
    pid == X98_CABLE || pid == AK820_CABLE
}

fn known_model(pid: u16) -> Option<&'static str> {
    match pid {
        X98_CABLE => Some("Attack Shark X98"),
        AK820_CABLE => Some("AJAZZ AK820 Max Plus"),
        _ => None,
    }
}

#[derive(Debug, PartialEq)]
pub enum Status {
    Offline,
    Online(u8),
}

/// Some(status) for a well-formed status reply, None to keep waiting; Err for a bad reply.
pub fn parse(r: &[u8]) -> Option<Result<Status, ()>> {
    if r.len() < 7 || r[..5] != [0xAA, 0x55, 0xCC, 0x33, 0x0E] {
        return None;
    }
    let (pct, state) = (r[5], r[6]);
    Some(match state {
        0 => Ok(Status::Offline),
        _ if state > 3 || pct > 100 => Err(()),
        _ => Ok(Status::Online(pct)),
    })
}

fn query(port: &mut dyn Port) -> Option<Status> {
    let mut req = vec![0u8; 65];
    req[1..6].copy_from_slice(&[0xAA, 0x55, 0xCC, 0x33, 0x0E]);
    port.flush();
    if !port.write(&req) {
        return None;
    }
    let end = Instant::now() + Duration::from_millis(600);
    loop {
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return None;
        }
        if let Some(status) = parse(&port.read(left)?) {
            return status.ok();
        }
    }
}

fn keyboard_name(c: &Coll, cable_pid: u16) -> String {
    if let Some(n) = known_model(c.pid).or_else(|| known_model(cable_pid)) {
        return n.into();
    }
    let p = c.product.as_str();
    if p.is_empty() || p == "2.4 Wireless Device" || p == "USB Gaming Keyboard" {
        "Wireless keyboard".into()
    } else {
        p.into()
    }
}

pub fn poll_with(colls: &[Coll], open: &mut Opener) -> Vec<Device> {
    let mut products: Vec<u16> = colls
        .iter()
        .filter(|c| c.vid == VID && PIDS.contains(&c.pid))
        .map(|c| c.pid)
        .collect();
    products.sort_unstable();
    products.dedup();
    // BatteryHub keeps the last cable id found in its sorted product list.
    let cable_pid = products
        .iter()
        .copied()
        .rfind(|p| is_cable(*p))
        .unwrap_or(0);
    // The dongle first: an active 2.4 GHz keyboard answers there even when the cable is plugged in
    // for charging (the cable collection times out while the keyboard is in 2.4 GHz mode).
    products.sort_by_key(|p| *p != DONGLE);

    for pid in products {
        for c in colls.iter().filter(|c| {
            c.is(VID, pid)
                && c.iface == 0
                && c.page == 0xFF02
                && c.usage == 0x0002
                && c.in_len == 65
                && c.out_len == 65
        }) {
            let Some(mut port) = open(c) else { continue };
            match query(port.as_mut()) {
                None => continue,
                // Keyboard offline on this link: BatteryHub lists it as disconnected (hidden).
                Some(Status::Offline) => break,
                Some(Status::Online(pct)) => {
                    let via = if is_cable(pid) { CABLE } else { RECEIVER };
                    let mut d = Device::new(
                        "semico:1A2C",
                        keyboard_name(c, cable_pid),
                        Kind::Keyboard,
                        via,
                    );
                    d.percent = Some(pct);
                    d.grace = RECEIVER_GRACE; // keep the last reading if the keyboard sleeps
                    return vec![d];
                }
            }
        }
    }
    Vec::new()
}

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    poll_with(&colls(infos), &mut hid_opener(api, Instant::now() + BUDGET))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::hidport::fake::{self, at, coll};

    fn collection(pid: u16, path: &str) -> Coll {
        let mut c = at(coll(VID, pid, 0xFF02, 0x0002, 65), path);
        c.iface = 0;
        c.product = match pid {
            DONGLE => "2.4 Wireless Device",
            X98_CABLE => "USB Gaming Keyboard",
            _ => "",
        }
        .into();
        c
    }

    fn reply(percent: u8, state: u8) -> Vec<u8> {
        let mut r = vec![0u8; 64];
        r[..7].copy_from_slice(&[0xAA, 0x55, 0xCC, 0x33, 0x0E, percent, state]);
        r
    }

    /// Replies per collection path.
    fn by_path(table: Vec<(&'static str, Vec<Vec<u8>>)>) -> fake::Shared {
        let s = fake::script();
        let mut table = table;
        s.borrow_mut().respond = Some(Box::new(move |path, _| {
            table
                .iter_mut()
                .find(|(p, _)| *p == path)
                .map(|(_, r)| std::mem::take(r))
                .unwrap_or_default()
        }));
        s
    }

    #[test]
    fn dongle_capture() {
        let s = by_path(vec![(
            "dongle",
            vec![vec![0x01, 0x02, 0x03], reply(0x60, 0x01)],
        )]);
        let out = poll_with(&[collection(DONGLE, "dongle")], &mut fake::opener(&s));
        let w = &s.borrow().writes;
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].len(), 65);
        assert_eq!(w[0][..6], [0x00, 0xAA, 0x55, 0xCC, 0x33, 0x0E]);
        let d = &out[0];
        assert_eq!(
            (d.key.as_str(), d.name.as_str()),
            ("semico:1A2C", "Wireless keyboard")
        );
        assert_eq!(
            (d.via.as_str(), d.percent, d.charging),
            (RECEIVER, Some(96), false)
        );
        assert_eq!(d.grace, RECEIVER_GRACE);
    }

    #[test]
    fn dongle_wins_and_cable_names_the_model() {
        let s = by_path(vec![("dongle", vec![reply(0x58, 0x01)])]);
        let colls = [collection(X98_CABLE, "cable"), collection(DONGLE, "dongle")];
        let out = poll_with(&colls, &mut fake::opener(&s));
        assert_eq!(out.len(), 1);
        assert_eq!(
            (out[0].name.as_str(), out[0].percent),
            ("Attack Shark X98", Some(88))
        );
        assert_eq!(out[0].via, RECEIVER);
    }

    #[test]
    fn offline_dongle_falls_back_to_cable() {
        let s = by_path(vec![
            ("dongle", vec![reply(0, 0)]),
            ("cable", vec![reply(0x64, 0x03)]),
        ]);
        let colls = [collection(DONGLE, "dongle"), collection(X98_CABLE, "cable")];
        let out = poll_with(&colls, &mut fake::opener(&s));
        assert_eq!(
            (out[0].name.as_str(), out[0].percent),
            ("Attack Shark X98", Some(100))
        );
        assert_eq!(out[0].via, CABLE);

        // Offline with no cable answering: nothing listed, no invented level.
        let s = by_path(vec![("dongle", vec![reply(0, 0)])]);
        assert!(poll_with(&[collection(DONGLE, "dongle")], &mut fake::opener(&s)).is_empty());
    }

    #[test]
    fn malformed_and_filtered() {
        for r in [reply(101, 2), reply(50, 4)] {
            let s = by_path(vec![("dongle", vec![r])]);
            assert!(poll_with(&[collection(DONGLE, "dongle")], &mut fake::opener(&s)).is_empty());
        }
        let mut wrong_iface = collection(DONGLE, "x");
        wrong_iface.iface = 3;
        let mut wrong_len = collection(DONGLE, "y");
        wrong_len.in_len = 33;
        wrong_len.out_len = 33;
        let colls = [
            at(coll(0x1234, DONGLE, 0xFF02, 2, 65), "other-vid"),
            collection(0x1234, "unknown-pid"),
            wrong_iface,
            at(coll(VID, DONGLE, 0xFF01, 0xFF01, 4), "wrong-page"),
            wrong_len,
        ];
        let s = fake::script();
        assert!(poll_with(&colls, &mut fake::opener(&s)).is_empty());
        assert!(s.borrow().opened.is_empty());
    }
}

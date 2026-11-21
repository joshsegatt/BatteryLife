//! Ninjutso mice:
//!  * Sora V2 on its 1K receiver (1915:AE1C) or USB cable (1915:AE11): feature report 05, command 15
//!    on FFA0:0001 -> [9] battery (0 means 100%), [10] charging, [12] online (always 0 on the cable,
//!    where the mouse counts as connected).
//!  * Ten / Ten Air (093A:EB01 receiver, 093A:E020 cable): feature report 06 (16 bytes) on FF01:0001,
//!    commands 13 (online), 12 (battery %), 11 (charging), value at byte 8.
//!
//! Only these state reads are sent.
//!
//! Ported from BatteryHub (src/drivers/ninjutso.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0, which credits NinjaForce and
//! ninjutso-battery-tray.

use std::time::Duration;

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::hid::{self, Coll, Dev, Hid, find};

pub const VID_NORDIC: u16 = 0x1915;
pub const SORA_PIDS: &[u16] = &[SORA_RECEIVER, SORA_CABLE];
const SORA_RECEIVER: u16 = 0xAE1C;
const SORA_CABLE: u16 = 0xAE11;
pub const VID_PIXART: u16 = 0x093A;
pub const TEN_PIDS: &[u16] = &[0xEB01, 0xE020];
const RECEIVER_GRACE: Duration = Duration::from_secs(30 * 60);

/// The Sora V2 state query (32-byte feature report 05).
pub fn sora_request() -> Vec<u8> {
    let mut q = vec![0u8; 32];
    q[0] = 0x05;
    q[1] = 0x15;
    q[4] = 1;
    q
}

/// (percent, charging) from a Sora V2 state reply, or None when it is not one; `connected == false`
/// is reported as Some(None).
pub fn parse_sora(r: &[u8], cable: bool) -> Option<Option<(u8, bool)>> {
    if r.len() < 13 || r[0] != 0x05 || r[1] != 0x15 || r[9] > 100 {
        return None;
    }
    let connected = cable || r[12] != 0;
    Some(connected.then(|| (if r[9] == 0 { 100 } else { r[9] }, r[10] != 0)))
}

fn poll_sora(hid: &mut dyn Hid, colls: &[Coll], pid: u16) -> Option<Device> {
    let cable = pid == SORA_CABLE;
    let cs = find(colls, VID_NORDIC, pid, |c| {
        c.page == 0xFFA0 && c.usage == 1 && c.feat_len >= 32
    });
    for c in cs {
        let Some(mut d) = hid.open(c) else { continue };
        let mut answered = false;
        let mut reading = None;
        for attempt in 0..4 {
            if attempt > 0 {
                hid.sleep(Duration::from_millis(90));
            }
            if !d.set_feature(&sora_request()) {
                break;
            }
            hid.sleep(Duration::from_millis(15));
            let Some(r) = d.get_feature(0x05).and_then(|r| parse_sora(&r, cable)) else {
                continue;
            };
            answered = true;
            reading = r;
            if reading.is_some() {
                break;
            }
        }
        if !answered {
            continue;
        }
        // Receiver answered, mouse off: not listed, the tracker keeps the last level.
        let (pct, charging) = reading?;
        let mut s = Device::new(
            format!("ninjutso:{VID_NORDIC:04X}:{pid:04X}"),
            "Ninjutso Sora V2",
            Kind::Mouse,
            if cable {
                "USB cable"
            } else {
                "2.4 GHz receiver"
            },
        );
        s.percent = Some(pct);
        s.charging = charging;
        if !cable {
            s.grace = RECEIVER_GRACE;
        }
        return Some(s);
    }
    None
}

/// A Ten / Ten Air query (16-byte feature report 06).
pub fn ten_request(cmd: u8) -> Vec<u8> {
    let mut q = vec![0u8; 16];
    q[0] = 0x06;
    q[1] = cmd;
    q[4] = 1;
    q
}

fn query_ten(hid: &mut dyn Hid, d: &mut dyn Dev, cmd: u8) -> Option<u8> {
    if !d.set_feature(&ten_request(cmd)) {
        return None;
    }
    for _ in 0..3 {
        hid.sleep(Duration::from_millis(30));
        if let Some(r) = d.get_feature(0x06)
            && r.len() == 16
            && r[0] == 0x06
            && r[1] == cmd
        {
            return Some(r[8]);
        }
    }
    None
}

fn poll_ten(hid: &mut dyn Hid, colls: &[Coll], pid: u16) -> Option<Device> {
    let cs = find(colls, VID_PIXART, pid, |c| {
        c.page == 0xFF01 && c.usage == 1 && c.feat_len == 16
    });
    for c in cs {
        let Some(mut d) = hid.open(c) else { continue };
        if query_ten(hid, d.as_mut(), 0x13) != Some(1) {
            continue;
        }
        let Some(pct) = query_ten(hid, d.as_mut(), 0x12).filter(|p| *p <= 100) else {
            continue;
        };
        let Some(charging) = query_ten(hid, d.as_mut(), 0x11) else {
            continue;
        };
        let name = if c.product.is_empty() {
            "Ninjutso Ten / Ten Air"
        } else {
            &c.product
        };
        let cable = pid == 0xE020;
        let mut s = Device::new(
            format!("ninjutso-ten:{pid:04X}"),
            name,
            Kind::Mouse,
            if cable {
                "USB cable"
            } else {
                "2.4 GHz receiver"
            },
        );
        s.percent = Some(pct);
        s.charging = charging != 0;
        if !cable {
            s.grace = RECEIVER_GRACE;
        }
        return Some(s);
    }
    None
}

fn poll_with(hid: &mut dyn Hid, colls: &[Coll]) -> Vec<Device> {
    let mut out = Vec::new();
    for &pid in SORA_PIDS {
        out.extend(poll_sora(hid, colls, pid));
    }
    for &pid in TEN_PIDS {
        out.extend(poll_ten(hid, colls, pid));
    }
    out
}

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    hid::run(api, infos, poll_with)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::hid::fake::{Fake, Op};
    use std::collections::VecDeque;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn sora(pid: u16) -> Coll {
        Coll {
            vid: VID_NORDIC,
            pid,
            page: 0xFFA0,
            usage: 1,
            feat_len: 704,
            ..Default::default()
        }
    }

    fn status(battery: u8, charging: bool, online: bool) -> Vec<u8> {
        let mut r = vec![0u8; 32];
        r[0] = 5;
        r[1] = 0x15;
        r[9] = battery;
        r[10] = charging as u8;
        r[12] = online as u8;
        r
    }

    fn sora_fake(replies: Vec<Vec<u8>>, send_ok: bool) -> Fake {
        let mut q: VecDeque<Vec<u8>> = replies.into();
        Fake::with(move |_, _, op| match op.clone() {
            Op::SetFeature(w) => {
                assert_eq!(w, sora_request()); // only the documented state query
                send_ok.then(Vec::new)
            }
            Op::GetFeature(id, _) => {
                assert_eq!(id, 5);
                q.pop_front()
            }
            _ => None,
        })
    }

    fn poll_sora_run(replies: Vec<Vec<u8>>, colls: &[Coll]) -> (Vec<Device>, usize) {
        let mut f = sora_fake(replies, true);
        let out = poll_with(&mut f, colls);
        (out, f.count(|o| matches!(o, Op::SetFeature(_))))
    }

    #[test]
    fn sora_v2_receiver() {
        let (out, queries) = poll_sora_run(vec![status(32, false, true)], &[sora(0xAE1C)]);
        assert_eq!(queries, 1);
        let s = &out[0];
        assert_eq!((s.percent, s.charging), (Some(32), false));
        assert_eq!(
            (s.name.as_str(), s.key.as_str()),
            ("Ninjutso Sora V2", "ninjutso:1915:AE1C")
        );
        assert_eq!((s.kind, s.via.as_str()), (Kind::Mouse, "2.4 GHz receiver"));

        let (out, _) = poll_sora_run(vec![status(0, true, true)], &[sora(0xAE1C)]);
        assert_eq!((out[0].percent, out[0].charging), (Some(100), true));
        let (out, queries) = poll_sora_run(
            vec![status(32, true, false), status(31, false, true)],
            &[sora(0xAE1C)],
        );
        assert_eq!((out[0].percent, queries), (Some(31), 2));
        // Offline four times: not listed.
        let (out, queries) = poll_sora_run(vec![status(0, true, false); 4], &[sora(0xAE1C)]);
        assert!(out.is_empty());
        assert_eq!(queries, 4);
    }

    #[test]
    fn sora_v2_bad_replies_and_failures() {
        let mut wrong_id = status(32, false, true);
        wrong_id[0] = 4;
        let mut wrong_cmd = status(32, false, true);
        wrong_cmd[1] = 0x28;
        let (out, queries) = poll_sora_run(
            vec![wrong_id, wrong_cmd, vec![0; 12], status(101, false, true)],
            &[sora(0xAE1C)],
        );
        assert!(out.is_empty());
        assert_eq!(queries, 4);
        let (out, queries) = poll_sora_run(vec![], &[sora(0xAE1C)]);
        assert!(out.is_empty());
        assert_eq!(queries, 4);
        let mut f = sora_fake(vec![status(32, false, true)], false);
        assert!(poll_with(&mut f, &[sora(0xAE1C)]).is_empty());
        assert_eq!(f.count(|o| matches!(o, Op::SetFeature(_))), 1);

        let mut other_product = sora(0xAE8C); // 8K receiver is outside the verified scope
        other_product.path = "8k".into();
        let mut other_page = sora(0xAE1C);
        other_page.page = 1;
        let mut other_usage = sora(0xAE1C);
        other_usage.usage = 2;
        let mut short = sora(0xAE1C);
        short.feat_len = 31;
        let mut f = sora_fake(vec![], true);
        assert!(poll_with(&mut f, &[other_product, other_page, other_usage, short]).is_empty());
        assert_eq!(f.s().opened.len(), 0);
    }

    #[test]
    fn sora_v2_cable() {
        // Captured on the cable: 05 15 00 00 01 00 00 00 00 4C 01 00 00 ...; [12] stays 0 there.
        let (out, queries) = poll_sora_run(vec![status(0x4C, true, false)], &[sora(0xAE11)]);
        assert_eq!(queries, 1);
        let s = &out[0];
        assert_eq!((s.percent, s.charging), (Some(76), true));
        assert_eq!(
            (s.key.as_str(), s.via.as_str()),
            ("ninjutso:1915:AE11", "USB cable")
        );
        // Receiver and cable together: one entry each, receiver first.
        let (out, queries) = poll_sora_run(
            vec![status(40, false, true), status(41, true, false)],
            &[sora(0xAE1C), sora(0xAE11)],
        );
        assert_eq!(queries, 2);
        assert_eq!(
            (out[0].via.as_str(), out[0].percent, out[0].charging),
            ("2.4 GHz receiver", Some(40), false)
        );
        assert_eq!(
            (out[1].via.as_str(), out[1].percent, out[1].charging),
            ("USB cable", Some(41), true)
        );
    }

    // ---- Ten / Ten Air ----

    fn ten(pid: u16) -> Coll {
        Coll {
            vid: VID_PIXART,
            pid,
            page: 0xFF01,
            usage: 1,
            feat_len: 16,
            path: "test".into(),
            product: "Ninjutso Ten Air".into(),
            ..Default::default()
        }
    }

    fn ten_reply(cmd: u8, value: u8) -> Vec<u8> {
        let mut r = vec![0u8; 16];
        r[0] = 0x06;
        r[1] = cmd;
        r[8] = value;
        r
    }

    struct Run {
        out: Vec<Device>,
        writes: Vec<Vec<u8>>,
        reads: usize,
        opens: usize,
    }

    fn ten_run(replies: Vec<Vec<u8>>, c: Coll, send_ok: bool, open_ok: bool) -> Run {
        let mut q: VecDeque<Vec<u8>> = replies.into();
        let reads = Arc::new(AtomicUsize::new(0));
        let r = reads.clone();
        let mut f = Fake::with(move |_, _, op| match op.clone() {
            Op::SetFeature(w) => {
                assert_eq!(w, ten_request(w[1]));
                send_ok.then(Vec::new)
            }
            Op::GetFeature(id, _) => {
                assert_eq!(id, 0x06);
                r.fetch_add(1, Ordering::Relaxed);
                q.pop_front()
            }
            _ => None,
        });
        f.s().refuse_open = !open_ok;
        let out = poll_with(&mut f, &[c]);
        let writes = f
            .s()
            .log
            .iter()
            .filter_map(|(_, o)| match o {
                Op::SetFeature(w) => Some(w.clone()),
                _ => None,
            })
            .collect();
        Run {
            out,
            writes,
            reads: reads.load(Ordering::Relaxed),
            opens: f.s().opened.len(),
        }
    }

    #[test]
    fn ten_air() {
        let r = ten_run(
            vec![ten_reply(0x13, 1), ten_reply(0x12, 67), ten_reply(0x11, 0)],
            ten(0xEB01),
            true,
            true,
        );
        let s = &r.out[0];
        assert_eq!((s.percent, s.charging), (Some(67), false));
        assert_eq!((s.name.as_str(), s.kind), ("Ninjutso Ten Air", Kind::Mouse));
        assert_eq!(
            (s.via.as_str(), s.key.as_str()),
            ("2.4 GHz receiver", "ninjutso-ten:EB01")
        );
        let cmds: Vec<u8> = r.writes.iter().map(|w| w[1]).collect();
        assert_eq!(cmds, [0x13, 0x12, 0x11]);

        let r = ten_run(
            vec![ten_reply(0x13, 0), ten_reply(0x12, 67), ten_reply(0x11, 0)],
            ten(0xEB01),
            true,
            true,
        );
        assert!(r.out.is_empty());
        assert_eq!(r.writes.len(), 1);
        let r = ten_run(
            vec![ten_reply(0x13, 1), ten_reply(0x12, 101), ten_reply(0x11, 0)],
            ten(0xEB01),
            true,
            true,
        );
        assert!(r.out.is_empty());
        // A short reply is skipped; the next read of the same query is used.
        let r = ten_run(
            vec![
                vec![0x06, 0],
                ten_reply(0x13, 1),
                ten_reply(0x12, 20),
                ten_reply(0x11, 1),
            ],
            ten(0xEB01),
            true,
            true,
        );
        assert!(r.out[0].charging);
        assert_eq!(r.writes.len(), 3);
        for pct in [0, 100] {
            let r = ten_run(
                vec![ten_reply(0x13, 1), ten_reply(0x12, pct), ten_reply(0x11, 1)],
                ten(0xE020),
                true,
                true,
            );
            assert_eq!((r.out[0].percent, r.out[0].charging), (Some(pct), true));
            assert_eq!(r.out[0].via, "USB cable");
        }
    }

    #[test]
    fn ten_bad_replies_failures_and_filtering() {
        let mut wrong_id = ten_reply(0x13, 1);
        wrong_id[0] = 7;
        for bad in [
            vec![],
            vec![0x06],
            wrong_id,
            ten_reply(0x12, 1),
            vec![0; 17],
        ] {
            let r = ten_run(
                vec![bad.clone(), bad.clone(), bad, ten_reply(0x13, 1)],
                ten(0xEB01),
                true,
                true,
            );
            assert!(r.out.is_empty());
            assert_eq!((r.writes.len(), r.reads), (1, 3));
        }
        let r = ten_run(vec![], ten(0xEB01), true, true);
        assert!(r.out.is_empty());
        assert_eq!(r.reads, 3);
        let r = ten_run(vec![], ten(0xEB01), false, true);
        assert_eq!((r.reads, r.writes.len()), (0, 1));
        let r = ten_run(vec![], ten(0xEB01), true, false);
        assert!(r.writes.is_empty());
        for variant in 0..6 {
            let mut c = ten(0xEB01);
            match variant {
                0 => c.pid = 0xEA01,
                1 => c.page = 0xFF00,
                2 => c.page = 0xFF02,
                3 => c.usage = 2,
                4 => c.feat_len = 64,
                _ => c.feat_len = 0,
            }
            let r = ten_run(vec![], c, true, true);
            assert_eq!((r.opens, r.writes.len()), (0, 0));
        }
    }
}

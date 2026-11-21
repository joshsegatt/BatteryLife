//! HyperX wireless headsets and Pulsefire Haste 2 mice (HP VID 03F0, older Kingston VID 0951): Cloud
//! III / II Core / III S / II / Flight / Flight S / Alpha / Alpha 2 / Stinger Core Wireless and the
//! Haste 2 Wireless dongles. Each model family has its own read-only battery query on a vendor
//! collection (see the comment on each reader).
//!
//! Ported from BatteryHub (src/drivers/hyperx.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0, which credits Medprj/HyperXWM,
//! LennardKittner/HyperHeadset, davidsantanaalves/HyperBatteryMonitor, PMarinov1994/ngenuity-linux-cli
//! and Sapd/HeadsetControl.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind, interpolate};
use crate::drivers::hid_io::{self, Coll, Io, Open, Outcome, find, query};

pub const VID_HP: u16 = 0x03F0;
pub const VID_KINGSTON: u16 = 0x0951;
const REPLY_MS: u64 = hid_io::MAX_REPLY_MS;

/// The Haste 2 reports about 40% for a few seconds after power-on; readings closer than this to the
/// link coming up are not trusted.
const HASTE_SETTLE: Duration = Duration::from_secs(6);

/// Kept across polls (BatteryHub's function statics).
#[derive(Default)]
pub struct State {
    haste_link_up: Option<Instant>,
    haste_polled: bool,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn base(pid: u16, name: &str) -> Device {
    Device::new(
        format!("hyperx:{pid:04X}"),
        name,
        Kind::Headset,
        "2.4 GHz receiver",
    )
}

/// Cloud III Wireless: report 66 <cmd>; 82 -> [2] connected, 8A -> [2] charging, 89 -> [2..3] mV, [4] %.
fn cloud3(io: &mut dyn Io, c: &Coll, _: &mut State) -> Outcome {
    let mut q = |cmd: u8| {
        query(io, &[0x66, cmd], REPLY_MS, |r| {
            r.len() >= 5 && r[0] == 0x66 && r[1] == cmd
        })
    };
    let Some(r) = q(0x82) else {
        return Outcome::NoAnswer;
    };
    if r[2] == 0 {
        return Outcome::Off;
    }
    let name = if matches!(c.pid, 0x069F | 0x0995) {
        "HyperX Cloud II Core Wireless"
    } else {
        "HyperX Cloud III Wireless"
    };
    let mut d = base(c.pid, name);
    if let Some(r) = q(0x8A) {
        d.charging = r[2] == 1;
    }
    if let Some(r) = q(0x89).filter(|r| r[4] <= 100) {
        d.percent = Some(r[4]);
        d.millivolts = Some(u16::from_be_bytes([r[2], r[3]]));
    }
    Outcome::Found(d)
}

/// Cloud II Wireless (HP): report 06 FF BB <cmd>; 02 -> [7] %, 03 -> [4] charging.
fn cloud2(io: &mut dyn Io, c: &Coll, _: &mut State) -> Outcome {
    let mut q = |cmd: u8| {
        query(io, &[0x06, 0xFF, 0xBB, cmd], REPLY_MS, |r| {
            r.len() >= 8 && r[..4] == [0x06, 0xFF, 0xBB, cmd]
        })
    };
    let Some(r) = q(0x02).filter(|r| r[7] <= 100) else {
        return Outcome::NoAnswer;
    };
    let mut d = base(c.pid, "HyperX Cloud II Wireless");
    d.percent = Some(r[7]);
    d.millivolts = Some(u16::from_be_bytes([r[5], r[6]]));
    if let Some(r) = q(0x03) {
        d.charging = r[4] == 1;
    }
    Outcome::Found(d)
}

/// Cloud II Wireless (Kingston 0951:1718) and Cloud Flight S (0951:16EA/16EB): fixed 17-byte request,
/// reply 0B 00 BB <cmd>; 02 -> [7] %, 03 -> [4] charging.
fn cloud2_kingston(io: &mut dyn Io, c: &Coll, _: &mut State) -> Outcome {
    let mut q = |cmd: u8| {
        let req = [
            0x06, 0x00, 0x02, 0x00, 0x9A, 0x00, 0x00, 0x68, 0x4A, 0x8E, 0x0A, 0x00, 0x00, 0x00,
            0xBB, cmd, 0x00,
        ];
        query(io, &req, REPLY_MS, |r| {
            r.len() >= 8 && r[0] == 0x0B && r[2] == 0xBB && r[3] == cmd
        })
    };
    let Some(r) = q(0x02).filter(|r| r[7] <= 100) else {
        return Outcome::NoAnswer;
    };
    let name = if matches!(c.pid, 0x16EA | 0x16EB) {
        "HyperX Cloud Flight S"
    } else {
        "HyperX Cloud II Wireless"
    };
    let mut d = base(c.pid, name);
    d.percent = Some(r[7]);
    if let Some(r) = q(0x03) {
        d.charging = r[4] == 1;
    }
    Outcome::Found(d)
}

/// Cloud Alpha Wireless: 21 BB <cmd>; 03 -> [3]==1 offline, 0C -> [3]==1 charging, 0B -> [3] %.
fn cloud_alpha(io: &mut dyn Io, c: &Coll, _: &mut State) -> Outcome {
    let mut q = |cmd: u8| {
        query(io, &[0x21, 0xBB, cmd], REPLY_MS, |r| {
            r.len() >= 4 && r[0] == 0x21 && r[2] == cmd
        })
    };
    let Some(r) = q(0x03) else {
        return Outcome::NoAnswer;
    };
    if r[3] == 0x01 {
        return Outcome::Off;
    }
    let mut d = base(c.pid, "HyperX Cloud Alpha Wireless");
    if let Some(r) = q(0x0C) {
        d.charging = r[3] == 0x01;
    }
    if let Some(r) = q(0x0B).filter(|r| r[3] <= 100) {
        d.percent = Some(r[3]);
    }
    Outcome::Found(d)
}

/// [cmd][sub] request, reply [cmd+1][sub]..; used by Cloud Alpha 2 and the Haste 2.
fn alpha2_query(io: &mut dyn Io, cmd: u8, sub: u8) -> Option<Vec<u8>> {
    query(io, &[cmd, sub], REPLY_MS, |r| {
        r.len() >= 8 && r[0] == cmd.wrapping_add(1) && r[1] == sub
    })
}

/// Cloud Alpha 2 Wireless: 52 01 -> [2] connected, 50 02 -> [2] %, [3] charging, [6..7] mV (LE).
fn cloud_alpha2(io: &mut dyn Io, c: &Coll, _: &mut State) -> Outcome {
    let Some(r) = alpha2_query(io, 0x52, 0x01) else {
        return Outcome::NoAnswer;
    };
    if r[2] == 0 {
        return Outcome::Off;
    }
    let mut d = base(c.pid, "HyperX Cloud Alpha 2 Wireless");
    if let Some(r) = alpha2_query(io, 0x50, 0x02).filter(|r| r[2] <= 100) {
        d.percent = Some(r[2]);
        d.charging = r[3] == 0x01;
        d.millivolts = Some(u16::from_le_bytes([r[6], r[7]]));
    }
    Outcome::Found(d)
}

/// Pulsefire Haste 2 (Core / Mini) Wireless: NGENUITY's battery query, the same 52 01 / 50 02 pair as
/// Cloud Alpha 2. After a reconnect the link reply can be lost in unrelated reports, so a missing link
/// reply is not an error: the battery query decides.
fn haste(io: &mut dyn Io, c: &Coll, st: &mut State) -> Outcome {
    let link = alpha2_query(io, 0x52, 0x01);
    let linked = link.as_ref().is_none_or(|r| r[2] != 0);
    let battery = if linked {
        alpha2_query(io, 0x50, 0x02).filter(|r| r[2] <= 100)
    } else {
        None
    };
    if link.is_none() && battery.is_none() {
        return Outcome::NoAnswer;
    }

    let now = Instant::now();
    if !linked {
        st.haste_link_up = None;
    } else if st.haste_link_up.is_none() {
        // The first poll after start assumes the mouse has been on for a while.
        st.haste_link_up = Some(if st.haste_polled {
            now
        } else {
            now.checked_sub(HASTE_SETTLE).unwrap_or(now)
        });
    }
    st.haste_polled = true;
    if !linked {
        return Outcome::Off;
    }
    let settled = st
        .haste_link_up
        .is_some_and(|t| now.duration_since(t) >= HASTE_SETTLE);

    let name = match c.pid {
        0x0AB5 => "HyperX Pulsefire Haste 2 Core Wireless",
        0x0DA0 => "HyperX Pulsefire Haste 2 Mini",
        _ => "HyperX Pulsefire Haste 2 Wireless",
    };
    let mut d = base(c.pid, name);
    d.kind = Kind::Mouse;
    // An unsettled (or missing) reading stays unknown, so the last level is kept meanwhile.
    d.grace = Duration::from_secs(60);
    if let Some(r) = battery.filter(|_| settled) {
        d.percent = Some(r[2]);
        d.charging = r[3] == 0x01 || r[3] == 0x02;
        let mv = u16::from_le_bytes([r[6], r[7]]);
        d.millivolts = (mv != 0).then_some(mv);
    }
    Outcome::Found(d)
}

/// Cloud Flight battery percent from the reported millivolts (HeadsetControl's curve).
pub fn cloud_flight_percent(mv: i32) -> u8 {
    if mv <= 3648 {
        (0.00125 * f64::from(mv)) as u8
    } else if mv > 3975 {
        100
    } else {
        let v = f64::from(mv);
        let p = 0.000_000_025_475_05 * v.powi(4) - 0.000_390_029_9 * v.powi(3) + 2.238_321 * v * v
            - 5706.256 * v
            + 5_452_299.0;
        p.round().clamp(0.0, 100.0) as u8
    }
}

/// Cloud Flight: 21 FF 05 -> [3..4] mV (above 0x100B means charging).
fn cloud_flight(io: &mut dyn Io, c: &Coll, _: &mut State) -> Outcome {
    let Some(r) = query(io, &[0x21, 0xFF, 0x05], REPLY_MS, |r| r.len() >= 5) else {
        return Outcome::NoAnswer;
    };
    let mv = u16::from_be_bytes([r[3], r[4]]);
    let mut d = base(c.pid, "HyperX Cloud Flight");
    d.millivolts = Some(mv);
    if mv > 0x100B {
        d.charging = true;
    } else {
        d.percent = Some(cloud_flight_percent(i32::from(mv)));
    }
    Outcome::Found(d)
}

/// Cloud III S Wireless: report 0C [02 03 01 00 <cmd>]; reply [5] = cmd, [6] = value (FF = invalid).
/// 02 -> [6] == 2 connected, 06 -> [6] %, 48 -> [6] 0 discharging / 1 charging / 2 full.
fn cloud3s(io: &mut dyn Io, c: &Coll, _: &mut State) -> Outcome {
    let mut q = |cmd: u8| {
        query(io, &[0x0C, 0x02, 0x03, 0x01, 0x00, cmd], REPLY_MS, |r| {
            r.len() >= 7 && r[0] == 0x0C && r[5] == cmd && r[6] != 0xFF
        })
    };
    let Some(r) = q(0x02) else {
        return Outcome::NoAnswer;
    };
    if r[6] != 2 {
        return Outcome::Off;
    }
    let mut d = base(c.pid, "HyperX Cloud III S Wireless");
    if let Some(r) = q(0x06).filter(|r| r[6] <= 100) {
        d.percent = Some(r[6]);
    }
    if let Some(r) = q(0x48) {
        d.charging = r[6] == 1 || r[6] == 2;
    }
    Outcome::Found(d)
}

const STINGER_CONNECTION: [u8; 20] = [
    0xFF, 0x07, 0x00, 0xFD, 0x04, 0x00, 0x00, 0x02, 0x82, 0x03, 0x05, 0x29, 0x0F, 0x00, 0x00, 0x03,
    0x68, 0x10, 0x00, 0x00,
];
const STINGER_BATTERY: [u8; 12] = [
    0xFF, 0x07, 0x00, 0xFD, 0x04, 0x0C, 0xF1, 0x02, 0x01, 0x04, 0xF0, 0x0C,
];

/// Stinger Core battery percent: separate mV curves (10% steps) while charging and on battery.
pub fn stinger_percent(mv: i32, charging: bool) -> u8 {
    const PCT: [i32; 11] = [100, 90, 80, 70, 60, 50, 40, 30, 20, 10, 0];
    const CHARGING: [i32; 11] = [
        4130, 4090, 4050, 4000, 3967, 3946, 3930, 3916, 3871, 3831, 3400,
    ];
    const DISCHARGING: [i32; 11] = [
        4121, 4042, 3954, 3905, 3838, 3785, 3763, 3738, 3700, 3651, 3517,
    ];
    interpolate(mv, if charging { &CHARGING } else { &DISCHARGING }, &PCT)
}

/// Cloud Stinger Core Wireless (+ DTS, Cloud Core Wireless DTS): SetFeature(command), wait 20 ms,
/// GetFeature(FF). Connection reply [11] == 1 = headset on; battery reply [9] = 03 discharging /
/// 05 charging / 06 full, [11..12] = mV (LE). The PlayStation and DTS receivers can hand back a
/// previous report, so their reply is read again (up to 10 x 20 ms) until it looks right.
fn stinger_core(io: &mut dyn Io, c: &Coll, _: &mut State) -> Outcome {
    let ps = matches!(c.pid, 0x0A93 | 0x1713 | 0x173F | 0x0A8E);
    // (whether a reply was accepted, the last reply read)
    let mut exchange = |cmd: &[u8], good: fn(&[u8]) -> bool| -> (bool, Option<Vec<u8>>) {
        if !io.set_feature(cmd) {
            return (false, None);
        }
        let mut last = None;
        for _ in 0..(if ps { 10 } else { 1 }) {
            io.sleep(20);
            match io.get_feature(0xFF) {
                Some(r) if r.len() >= 13 => {
                    let ok = !ps || good(&r);
                    last = Some(r);
                    if ok {
                        return (true, last);
                    }
                }
                _ => return (false, last),
            }
        }
        (false, last)
    };
    let fresh = |r: &[u8]| !(r[1] == 0x01 && r[2] == 0x00);
    let battery = |r: &[u8]| r[0] == 0xFF && r[1] == 0x12 && r[2] == 0x05;

    // The connection reply's header isn't documented, so after 10 stale-looking reads the last is used.
    let r = match exchange(&STINGER_CONNECTION, fresh) {
        (true, Some(r)) => r,
        (false, Some(r)) if ps && !fresh(&r) => r,
        _ => return Outcome::NoAnswer,
    };
    if r[11] != 0x01 {
        return Outcome::Off;
    }
    let name = match c.pid {
        0x173F => "HyperX Cloud Core Wireless",
        0x0A8E => "HyperX Cloud Stinger Core Wireless DTS",
        _ => "HyperX Cloud Stinger Core Wireless",
    };
    let mut d = base(c.pid, name);
    if let (true, Some(r)) = exchange(&STINGER_BATTERY, battery)
        && matches!(r[9], 0x03 | 0x05 | 0x06)
    {
        let mv = u16::from_le_bytes([r[11], r[12]]);
        d.millivolts = Some(mv);
        d.charging = r[9] == 0x05;
        d.percent = Some(if r[9] == 0x06 {
            100
        } else {
            stinger_percent(i32::from(mv), d.charging)
        });
    }
    Outcome::Found(d)
}

type Reader = fn(&mut dyn Io, &Coll, &mut State) -> Outcome;

struct Model {
    vid: u16,
    pids: &'static [u16],
    /// Which collection carries the protocol.
    accept: fn(&Coll) -> bool,
    read: Reader,
}

const MODELS: &[Model] = &[
    Model {
        vid: VID_HP,
        pids: &[0x05B7, 0x0C9D],
        accept: |c| c.out_len == 62 && c.in_len == 62,
        read: cloud3,
    },
    // Cloud II Core Wireless: the Cloud III protocol on 64-byte reports.
    Model {
        vid: VID_HP,
        pids: &[0x069F, 0x0995],
        accept: |c| c.page >= 0xFF00 && c.out_len >= 62 && c.in_len >= 62,
        read: cloud3,
    },
    // Cloud III S: report 0C lives on the 64/64-byte collection (usage page 01C0).
    Model {
        vid: VID_HP,
        pids: &[0x06BE, 0x02CC],
        accept: |c| c.out_len == 64 && c.in_len == 64,
        read: cloud3s,
    },
    Model {
        vid: VID_HP,
        pids: &[0x0696, 0x018B, 0x0D93, 0x0C8C],
        accept: |c| c.page == 0xFF90 && c.usage == 0x0303,
        read: cloud2,
    },
    Model {
        vid: VID_KINGSTON,
        pids: &[0x1718],
        accept: |c| c.out_len == 62,
        read: cloud2_kingston,
    },
    Model {
        vid: VID_KINGSTON,
        pids: &[0x16EA, 0x16EB],
        accept: |c| c.page == 0xFF13 && c.usage == 0x0001 && c.in_len == 62 && c.out_len == 62,
        read: cloud2_kingston,
    },
    Model {
        vid: VID_HP,
        pids: &[0x098D],
        accept: |c| c.page >= 0xFF00 && c.out_len >= 3,
        read: cloud_alpha,
    },
    Model {
        vid: VID_HP,
        pids: &[0x08BE],
        accept: |c| c.page == 0xFF13 && c.usage == 0xFF00,
        read: cloud_alpha2,
    },
    Model {
        vid: VID_HP,
        pids: &[0x0AB5, 0x0D97, 0x0F98, 0x0DA0],
        accept: |c| c.page >= 0xFF00 && c.out_len >= 2 && c.in_len >= 8,
        read: haste,
    },
    Model {
        vid: VID_KINGSTON,
        pids: &[0x16C4, 0x1723, 0x1749],
        accept: |c| c.page >= 0xFF00 && c.out_len >= 3,
        read: cloud_flight,
    },
    Model {
        vid: VID_HP,
        pids: &[0x0E90],
        accept: |c| c.page >= 0xFF00 && c.out_len >= 3,
        read: cloud_flight,
    },
    Model {
        vid: VID_HP,
        pids: &[0x0A93, 0x0A8E],
        accept: |c| c.page == 0xFF00 && c.usage == 0x0001 && c.feat_len >= 32,
        read: stinger_core,
    },
    Model {
        vid: VID_KINGSTON,
        pids: &[0x170B, 0x1713, 0x173F],
        accept: |c| c.page == 0xFF00 && c.usage == 0x0001 && c.feat_len >= 32,
        read: stinger_core,
    },
];

/// Product ids this driver handles, per vendor.
pub const HP_PIDS: &[u16] = &[
    0x05B7, 0x0C9D, 0x069F, 0x0995, 0x06BE, 0x02CC, 0x0696, 0x018B, 0x0D93, 0x0C8C, 0x098D, 0x08BE,
    0x0AB5, 0x0D97, 0x0F98, 0x0DA0, 0x0E90, 0x0A93, 0x0A8E,
];
pub const KINGSTON_PIDS: &[u16] = &[
    0x1718, 0x16EA, 0x16EB, 0x16C4, 0x1723, 0x1749, 0x170B, 0x1713, 0x173F,
];

pub fn poll_colls(colls: &[Coll], open: &mut Open<'_>, st: &mut State) -> Vec<Device> {
    let mut out = Vec::new();
    for m in MODELS {
        for &pid in m.pids {
            for c in find(colls, m.vid, pid, m.accept) {
                let Some(mut io) = open(c) else { continue };
                if (m.read)(io.as_mut(), c, st).settle(&mut out) {
                    break;
                }
            }
        }
    }
    out
}

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    let mut guard = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let st = guard.get_or_insert_with(State::default);
    hid_io::run(api, infos, |colls, open| poll_colls(colls, open, st))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::hid_io::fake::Fake;
    use std::cell::RefCell;

    fn run(colls: &[Coll], fake: &Fake, st: &mut State) -> (Vec<Device>, usize) {
        let opened = RefCell::new(Vec::new());
        let out = poll_colls(colls, &mut *fake.opener(&opened), st);
        let n = opened.borrow().len();
        (out, n)
    }

    // ---- Cloud Flight S (tests/hyperx_cloud_flight_s_driver_test.cpp) ----

    fn flight_s(pid: u16) -> Coll {
        let mut c = Coll::new(VID_KINGSTON, pid, 0xFF13, 0x0001, 62, 62);
        c.iface = 5;
        c
    }

    fn kingston_reply(cmd: u8, value: u8, valid: bool) -> Vec<u8> {
        let mut r = vec![0u8; 62];
        r[0] = if valid { 0x0B } else { 0x09 };
        r[2] = 0xBB;
        r[3] = cmd;
        r[if cmd == 0x02 { 7 } else { 4 }] = value;
        r
    }

    /// Answers each 17-byte request by its command byte (r[15]).
    fn kingston(script: Vec<(u8, Vec<Vec<u8>>)>) -> Fake {
        let f = Fake::new();
        f.replies(move |w| {
            assert!(w.len() == 17 && w[14] == 0xBB);
            script
                .iter()
                .find(|(cmd, _)| *cmd == w[15])
                .map(|(_, r)| r.clone())
                .unwrap_or_default()
        });
        f
    }

    fn check_requests(f: &Fake) {
        let s = f.s();
        assert_eq!(s.writes.len(), 2);
        for (i, w) in s.writes.iter().enumerate() {
            assert_eq!(
                w[..15],
                [
                    0x06, 0x00, 0x02, 0x00, 0x9A, 0x00, 0x00, 0x68, 0x4A, 0x8E, 0x0A, 0x00, 0x00,
                    0x00, 0xBB
                ]
            );
            assert_eq!(w[15], if i == 0 { 0x02 } else { 0x03 });
            assert_eq!(w[16], 0);
        }
    }

    fn device(pct: u8, charging: u8) -> Vec<(u8, Vec<Vec<u8>>)> {
        vec![
            (0x02, vec![kingston_reply(0x02, pct, true)]),
            (0x03, vec![kingston_reply(0x03, charging, true)]),
        ]
    }

    #[test]
    fn cloud_flight_s_level_and_charging() {
        let f = kingston(device(74, 1));
        let (out, opens) = run(&[flight_s(0x16EA)], &f, &mut State::default());
        check_requests(&f);
        assert_eq!(opens, 1);
        assert_eq!(out.len(), 1);
        let d = &out[0];
        assert_eq!(d.key, "hyperx:16EA");
        assert_eq!(d.name, "HyperX Cloud Flight S");
        assert_eq!(
            (d.kind, d.via.as_str()),
            (Kind::Headset, "2.4 GHz receiver")
        );
        assert_eq!((d.percent, d.charging), (Some(74), true));

        let f = kingston(device(41, 0));
        let (out, _) = run(&[flight_s(0x16EB)], &f, &mut State::default());
        assert_eq!(out[0].key, "hyperx:16EB");
        assert_eq!((out[0].percent, out[0].charging), (Some(41), false));
    }

    #[test]
    fn cloud_flight_s_opens_only_the_vendor_collection() {
        let consumer = Coll::new(VID_KINGSTON, 0x16EA, 0x000C, 1, 62, 62);
        let consumer2 = Coll::new(VID_KINGSTON, 0x16EA, 0x000C, 1, 2, 0);
        let f = kingston(device(88, 0));
        let (out, opens) = run(
            &[consumer, consumer2, flight_s(0x16EA), flight_s(0x16EA)],
            &f,
            &mut State::default(),
        );
        assert_eq!((opens, out.len(), out[0].percent), (1, 1, Some(88)));
        check_requests(&f);
    }

    #[test]
    fn cloud_flight_s_skips_stale_replies_and_survives_missing_charging() {
        let f = kingston(vec![
            (
                0x02,
                vec![
                    kingston_reply(0x03, 1, true),
                    kingston_reply(0x02, 60, false),
                    kingston_reply(0x02, 65, true),
                ],
            ),
            (0x03, vec![kingston_reply(0x03, 0, true)]),
        ]);
        let (out, _) = run(&[flight_s(0x16EA)], &f, &mut State::default());
        assert_eq!(out[0].percent, Some(65));

        let f = kingston(vec![(0x02, vec![kingston_reply(0x02, 52, true)])]);
        let (out, _) = run(&[flight_s(0x16EA)], &f, &mut State::default());
        assert_eq!((out[0].percent, out[0].charging), (Some(52), false));
        check_requests(&f);
    }

    #[test]
    fn cloud_flight_s_failures_list_nothing() {
        let f = kingston(vec![]);
        let (out, _) = run(&[flight_s(0x16EA)], &f, &mut State::default());
        assert!(out.is_empty());
        assert_eq!(f.s().writes.len(), 1);
        for bad in [
            kingston_reply(0x02, 101, true),
            kingston_reply(0x02, 80, false),
        ] {
            let f = kingston(vec![(0x02, vec![bad])]);
            assert!(
                run(&[flight_s(0x16EA)], &f, &mut State::default())
                    .0
                    .is_empty()
            );
        }
    }

    #[test]
    fn cloud_flight_s_strict_collection_matching() {
        let mut wrong_vid = flight_s(0x16EA);
        wrong_vid.vid = VID_HP;
        for c in [
            Coll::new(VID_KINGSTON, 0x16EA, 0x000C, 1, 62, 62),
            Coll::new(VID_KINGSTON, 0x16EA, 0xFF13, 2, 62, 62),
            Coll::new(VID_KINGSTON, 0x16EA, 0xFF13, 1, 61, 62),
            Coll::new(VID_KINGSTON, 0x16EA, 0xFF13, 1, 62, 61),
            wrong_vid,
            flight_s(0x16EC),
        ] {
            let f = kingston(device(83, 0));
            let (out, opens) = run(&[c], &f, &mut State::default());
            assert!(out.is_empty() && opens == 0 && f.s().writes.is_empty());
        }
        // Kingston Cloud II Wireless keeps the same protocol, on any page.
        let f = kingston(device(76, 0));
        let c = Coll::new(VID_KINGSTON, 0x1718, 0x000C, 1, 62, 62);
        let (out, _) = run(&[c], &f, &mut State::default());
        assert_eq!(out[0].name, "HyperX Cloud II Wireless");
        assert_eq!(out[0].percent, Some(76));
    }

    // ---- Haste 2 (tests/hyperx_haste_driver_test.cpp) ----

    fn haste_coll(pid: u16, page: u16) -> Coll {
        Coll::new(VID_HP, pid, page, 1, 64, 64)
    }

    fn report(id: u8, sub: u8, bytes: &[(usize, u8)]) -> Vec<u8> {
        let mut r = vec![0u8; 64];
        r[0] = id;
        r[1] = sub;
        for &(i, v) in bytes {
            r[i] = v;
        }
        r
    }
    fn link(linked: u8) -> Vec<u8> {
        report(0x53, 0x01, &[(2, linked)])
    }
    fn battery(pct: u8, charge: u8) -> Vec<u8> {
        report(0x51, 0x02, &[(2, pct), (3, charge)])
    }

    /// The dongle floods unrelated reports before each reply.
    fn dongle(link: Option<Vec<u8>>, battery: Vec<u8>) -> Fake {
        let f = Fake::new();
        f.replies(move |w| {
            assert_eq!(w.len(), 2);
            match (w[0], w[1]) {
                (0x52, 0x01) => {
                    let mut r = vec![report(0x44, 0x01, &[]), report(0x45, 0x02, &[])];
                    r.extend(link.clone());
                    r
                }
                (0x50, 0x02) => vec![report(0xFB, 0x01, &[(2, 1)]), battery.clone()],
                _ => vec![],
            }
        });
        f
    }

    const ASK_LINK: [u8; 2] = [0x52, 0x01];
    const ASK_BATTERY: [u8; 2] = [0x50, 0x02];

    #[test]
    fn haste_level_name_and_charging() {
        let core = haste_coll(0x0AB5, 0xFF00);
        let mut st = State::default();
        let f = dongle(Some(link(1)), battery(70, 0));
        let (out, _) = run(std::slice::from_ref(&core), &f, &mut st);
        assert_eq!(f.s().writes, vec![ASK_LINK.to_vec(), ASK_BATTERY.to_vec()]);
        let d = &out[0];
        assert_eq!(d.key, "hyperx:0AB5");
        assert_eq!(d.name, "HyperX Pulsefire Haste 2 Core Wireless");
        assert_eq!(
            (d.kind, d.percent, d.charging),
            (Kind::Mouse, Some(70), false)
        );

        for (charge, pct, charging) in [(1, 55, true), (2, 100, true), (3, 40, false)] {
            let f = dongle(Some(link(1)), battery(pct, charge));
            let (out, _) = run(std::slice::from_ref(&core), &f, &mut st);
            assert_eq!((out[0].percent, out[0].charging), (Some(pct), charging));
        }
        let f = dongle(
            Some(link(1)),
            report(0x51, 0x02, &[(2, 80), (6, 0xA4), (7, 0x0F)]),
        );
        assert_eq!(
            run(std::slice::from_ref(&core), &f, &mut st).0[0].millivolts,
            Some(0x0FA4)
        );
    }

    #[test]
    fn haste_missing_link_reply_and_bad_level() {
        let core = haste_coll(0x0AB5, 0xFF00);
        let mut st = State::default();
        let f = dongle(None, battery(70, 0));
        let (out, _) = run(std::slice::from_ref(&core), &f, &mut st);
        assert_eq!(f.s().writes, vec![ASK_LINK.to_vec(), ASK_BATTERY.to_vec()]);
        assert_eq!(out[0].percent, Some(70));

        let f = dongle(Some(link(1)), battery(101, 0));
        let (out, _) = run(std::slice::from_ref(&core), &f, &mut st);
        assert_eq!((out.len(), out[0].percent), (1, None));

        // No reply at all: nothing listed.
        let (out, _) = run(std::slice::from_ref(&core), &Fake::new(), &mut st);
        assert!(out.is_empty());
    }

    #[test]
    fn haste_other_dongles_and_filters() {
        let mut st = State::default();
        for (pid, page, name, pct) in [
            (0x0D97, 0xFF00, "HyperX Pulsefire Haste 2 Wireless", 33),
            (0x0F98, 0xFF00, "HyperX Pulsefire Haste 2 Wireless", 34),
            (0x0DA0, 0xFF90, "HyperX Pulsefire Haste 2 Mini", 21),
        ] {
            let f = dongle(Some(link(1)), battery(pct, 0));
            let (out, _) = run(&[haste_coll(pid, page)], &f, &mut st);
            assert_eq!((out[0].name.as_str(), out[0].percent), (name, Some(pct)));
            assert_eq!(out[0].key, format!("hyperx:{pid:04X}"));
        }
        for c in [
            haste_coll(0x0AB5, 0x0001),
            Coll::new(VID_HP, 0x0AB5, 0xFF00, 1, 4, 64),
            Coll::new(VID_HP, 0x0AB5, 0xFF00, 1, 64, 1),
        ] {
            let f = dongle(Some(link(1)), battery(70, 0));
            let (out, opens) = run(&[c], &f, &mut st);
            assert!(out.is_empty() && opens == 0 && f.s().writes.is_empty());
        }
    }

    #[test]
    fn haste_link_down_then_settle_window() {
        let core = haste_coll(0x0AB5, 0xFF00);
        let mut st = State::default();
        // Link down: nothing listed and no battery query sent.
        let f = dongle(Some(link(0)), battery(70, 0));
        let (out, _) = run(std::slice::from_ref(&core), &f, &mut st);
        assert_eq!(f.s().writes, vec![ASK_LINK.to_vec()]);
        assert!(out.is_empty());
        // Link up right after: the power-on reading stays unknown.
        let f = dongle(Some(link(1)), battery(40, 0));
        let (out, _) = run(std::slice::from_ref(&core), &f, &mut st);
        assert_eq!((out.len(), out[0].percent), (1, None));
    }

    // ---- Stinger Core (tests/hyperx_stinger_driver_test.cpp) ----

    fn stinger_coll(vid: u16, pid: u16) -> Coll {
        let mut c = Coll::new(vid, pid, 0xFF00, 1, 64, 64);
        c.feat_len = 64;
        c.iface = 3;
        c
    }
    fn stinger_reply(kind: u8, command: u8, code: u8, mv: u16) -> Vec<u8> {
        let mut r = vec![0u8; 64];
        r[0] = 0xFF;
        r[1] = kind;
        r[2] = command;
        r[9] = code;
        r[11] = mv as u8;
        r[12] = (mv >> 8) as u8;
        r
    }
    fn stinger_link(on: bool) -> Vec<u8> {
        stinger_reply(1, 0, 0, u16::from(on))
    }
    fn stinger_battery(code: u8, mv: u16) -> Vec<u8> {
        stinger_reply(0x12, 5, code, mv)
    }
    fn stinger(links: Vec<Vec<u8>>, batteries: Vec<Vec<u8>>) -> Fake {
        let f = Fake::new();
        f.s().on_set_feature = Some(Box::new(move |w| {
            assert!(w == STINGER_CONNECTION || w == STINGER_BATTERY);
            if w == STINGER_CONNECTION {
                links.clone()
            } else {
                batteries.clone()
            }
        }));
        f
    }

    #[test]
    fn stinger_dts_skips_stale_replies() {
        let dts = stinger_coll(VID_HP, 0x0A8E);
        let f = stinger(
            vec![stinger_link(true); 10],
            vec![stinger_link(true), stinger_battery(3, 3954)],
        );
        let (out, _) = run(std::slice::from_ref(&dts), &f, &mut State::default());
        let d = &out[0];
        assert_eq!(d.key, "hyperx:0A8E");
        assert_eq!(d.name, "HyperX Cloud Stinger Core Wireless DTS");
        assert_eq!(
            (d.kind, d.charging, d.percent, d.millivolts),
            (Kind::Headset, false, Some(80), Some(3954))
        );
        let s = f.s();
        assert_eq!((s.set_features.len(), s.feature_reads), (2, 12));
        drop(s);

        let f = stinger(vec![stinger_link(true)], vec![stinger_battery(5, 4050)]);
        let out = run(std::slice::from_ref(&dts), &f, &mut State::default()).0;
        assert_eq!((out[0].charging, out[0].percent), (true, Some(80)));
        let f = stinger(vec![stinger_link(true)], vec![stinger_battery(6, 4130)]);
        let out = run(std::slice::from_ref(&dts), &f, &mut State::default()).0;
        assert_eq!((out[0].charging, out[0].percent), (false, Some(100)));

        // Headset off: nothing listed, no battery request.
        let f = stinger(
            vec![stinger_link(false); 10],
            vec![stinger_battery(3, 3954)],
        );
        assert!(
            run(std::slice::from_ref(&dts), &f, &mut State::default())
                .0
                .is_empty()
        );
        assert_eq!(f.s().set_features.len(), 1);
    }

    #[test]
    fn stinger_invalid_replies_never_become_readings() {
        let dts = stinger_coll(VID_HP, 0x0A8E);
        for invalid in [
            stinger_link(true),
            stinger_reply(0x12, 6, 3, 3954),
            stinger_reply(0x11, 5, 3, 3954),
            stinger_battery(0xFF, 3954),
            vec![0xFF, 0x12, 5],
        ] {
            let f = stinger(vec![stinger_link(true)], vec![invalid; 12]);
            let out = run(std::slice::from_ref(&dts), &f, &mut State::default()).0;
            assert_eq!(
                (out.len(), out[0].percent, out[0].millivolts),
                (1, None, None)
            );
            assert!(f.s().feature_reads <= 12);
        }
        let f = stinger(vec![], vec![]);
        assert!(
            run(std::slice::from_ref(&dts), &f, &mut State::default())
                .0
                .is_empty()
        );
    }

    #[test]
    fn stinger_filters_and_legacy_receivers() {
        let dts = stinger_coll(VID_HP, 0x0A8E);
        let mut wrong = vec![];
        for page in [0x000C, 0xFFC0] {
            let mut c = dts.clone();
            c.page = page;
            wrong.push(c);
        }
        let mut c = dts.clone();
        c.usage = 2;
        wrong.push(c);
        let mut c = dts.clone();
        c.feat_len = 2;
        wrong.push(c);
        wrong.push(stinger_coll(VID_HP, 0x0591));
        wrong.push(stinger_coll(VID_KINGSTON, 0x0A8E));
        for c in wrong {
            let f = stinger(vec![stinger_link(true)], vec![stinger_battery(3, 3954)]);
            let (out, opens) = run(&[c], &f, &mut State::default());
            assert!(out.is_empty() && opens == 0);
        }

        // Original Kingston receiver: one read, header not checked.
        let f = stinger(vec![stinger_link(true)], vec![stinger_reply(0, 0, 3, 3954)]);
        let out = run(
            &[stinger_coll(VID_KINGSTON, 0x170B)],
            &f,
            &mut State::default(),
        )
        .0;
        assert_eq!((out[0].percent, f.s().feature_reads), (Some(80), 2));
        for (vid, pid) in [(VID_HP, 0x0A93), (VID_KINGSTON, 0x1713)] {
            let f = stinger(
                vec![stinger_link(true)],
                vec![stinger_link(true), stinger_battery(3, 3954)],
            );
            let out = run(&[stinger_coll(vid, pid)], &f, &mut State::default()).0;
            assert_eq!(out[0].percent, Some(80));
            assert_eq!(out[0].name, "HyperX Cloud Stinger Core Wireless");
        }
    }

    #[test]
    fn cloud3_and_flight_curves() {
        // Cloud III: connected, charging, 3900 mV / 77 %.
        let f = Fake::new();
        f.replies(|w| match w[1] {
            0x82 => vec![vec![0x66, 0x82, 1, 0, 0]],
            0x8A => vec![vec![0x66, 0x8A, 1, 0, 0]],
            0x89 => vec![vec![0x66, 0x89, 0x0F, 0x3C, 77]],
            _ => vec![],
        });
        let out = run(
            &[Coll::new(VID_HP, 0x0C9D, 0xFF90, 1, 62, 62)],
            &f,
            &mut State::default(),
        )
        .0;
        assert_eq!(
            (out[0].percent, out[0].charging, out[0].millivolts),
            (Some(77), true, Some(3900))
        );
        assert_eq!(out[0].name, "HyperX Cloud III Wireless");
        assert_eq!(cloud_flight_percent(3000), 3);
        assert_eq!(cloud_flight_percent(4000), 100);
        assert!((1..=100).contains(&cloud_flight_percent(3800)));
    }
}

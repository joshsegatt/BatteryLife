//! Darmoshark mice:
//!  * M3 / M3S on their Telink receivers (248A:FF30 / FF31), collection 008C:0001 on interface 1
//!    (33-byte reports, 1001-byte feature): SET_FEATURE [51][opcode], then GET_FEATURE 51. Opcode 03
//!    (bond) must name the paired mouse (248A:FF12 / FF18) before opcode 06 (identify) is trusted:
//!    [2] = 1, [4..7] ids, [11] percent. An input [54][E4][02] means the mouse did not answer.
//!  * M3S Pro on the Nordic "1K NRF Receiver" (1915:0723), FF0A:0001 on interface 3, 64-byte frames
//!    without report id whose bytes sum to A1: request [41][00][81][01] .. [csum], reply
//!    [41][00][8C][01][..][..][percent].
//!
//! 248A and 1915 are shared vendor ids, so only these receivers are declared.
//!
//! Ported from BatteryHub (src/drivers/darmoshark.cpp, PollDarmoshark and PollDarmosharkNrf, commit
//! fd2267d), https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0, which cites
//! github.com/Barba2k2/darmoshark-m3-configurator (PROTOCOL.md). The NRF reply wait is 500 ms
//! (BatteryHub: 600 ms).

use std::time::{Duration, Instant};

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::hid_dev::{BUDGET, Coll, Dev, HidIo, Io, colls, find, hex4, ms_left};
use crate::registry::Match;

const VID: u16 = 0x248A;
const FEATURE: u8 = 0x51;
const BOND: u8 = 0x03;
const IDENTIFY: u8 = 0x06;

/// (receiver, mouse, name)
const MODELS: [(u16, u16, &str); 2] = [
    (0xFF30, 0xFF12, "Darmoshark M3"),
    (0xFF31, 0xFF18, "Darmoshark M3S"),
];

const NRF_VID: u16 = 0x1915;
const NRF_RECEIVER: u16 = 0x0723;
const NRF_FRAME: usize = 64;
const NRF_SUM: u8 = 0xA1;

pub static MATCHES: &[Match] = &[
    Match {
        vid: VID,
        pids: &[0xFF30, 0xFF31],
    },
    Match {
        vid: NRF_VID,
        pids: &[NRF_RECEIVER],
    },
];

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    poll_with(&colls(infos), &mut HidIo(api))
}

fn id_at(r: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([r[at], r[at + 1]])
}

/// The bond reply names this vendor and mouse, and says it is bonded.
pub fn bond_ok(r: &[u8], mouse: u16) -> bool {
    r.len() >= 12 && id_at(r, 3) == VID && id_at(r, 5) == mouse && r[7] != 0
}

/// Percent from the identify reply.
pub fn identify_percent(r: &[u8], mouse: u16) -> Option<u8> {
    (r.len() >= 12 && r[2] == 1 && id_at(r, 4) == VID && id_at(r, 6) == mouse && r[11] <= 100)
        .then(|| r[11])
}

/// One feature exchange; None when the receiver reports the mouse silent or time runs out.
fn query(d: &mut dyn Dev, opcode: u8, receiver_end: Instant) -> Option<Vec<u8>> {
    if Instant::now() >= receiver_end {
        return None;
    }
    let end = receiver_end.min(Instant::now() + Duration::from_millis(500));
    d.flush();
    if Instant::now() >= end || !d.set_feature(&[FEATURE, opcode]) {
        return None;
    }
    while Instant::now() < end {
        let wait = ms_left(end).clamp(1, 20);
        if let Some(i) = d.read(wait)
            && i.len() >= 3
            && i[..3] == [0x54, 0xE4, 2]
        {
            return None;
        }
        if Instant::now() >= end {
            return None;
        }
        if let Some(r) = d.get_feature(FEATURE)
            && r.len() >= 12
            && r[0] == FEATURE
            && r[1] == opcode
        {
            return (Instant::now() < end).then_some(r);
        }
    }
    None
}

/// The bond query replaces any earlier battery reply in the receiver's feature buffer.
fn read_battery(d: &mut dyn Dev, mouse: u16, end: Instant) -> Option<u8> {
    let bond = query(d, BOND, end)?;
    if !bond_ok(&bond, mouse) {
        return None;
    }
    identify_percent(&query(d, IDENTIFY, end)?, mouse)
}

pub fn nrf_checksum(frame: &[u8]) -> u8 {
    let sum = frame[..NRF_FRAME - 1]
        .iter()
        .fold(0u8, |a, b| a.wrapping_add(*b));
    NRF_SUM.wrapping_sub(sum)
}

/// Output report: report id 0, then the 64-byte frame.
pub fn nrf_request() -> Vec<u8> {
    let mut q = vec![0u8; NRF_FRAME + 1];
    q[1] = 0x41;
    q[3] = 0x81;
    q[4] = 0x01;
    q[NRF_FRAME] = nrf_checksum(&q[1..]);
    q
}

pub fn nrf_parse(r: &[u8]) -> Option<u8> {
    (r.len() >= NRF_FRAME
        && r[0] == 0x41
        && r[2] == 0x8C
        && r[3] == 0x01
        && r[NRF_FRAME - 1] == nrf_checksum(r)
        && r[6] <= 100)
        .then(|| r[6])
}

fn nrf_read(d: &mut dyn Dev, end: Instant) -> Option<u8> {
    d.flush();
    if !d.write(&nrf_request()) {
        return None;
    }
    let end = end.min(Instant::now() + Duration::from_millis(500));
    while ms_left(end) > 0 {
        if let Some(p) = d.read(ms_left(end).min(100)).and_then(|r| nrf_parse(&r)) {
            return Some(p);
        }
    }
    None
}

fn mouse(key: String, name: &str, pct: u8) -> Device {
    let mut s = Device::new(key, name, Kind::Mouse, "2.4 GHz receiver");
    s.percent = Some(pct);
    // BatteryHub sets no grace here; a receiver mouse that sleeps keeps its last value 30 min.
    s.grace = Duration::from_secs(1800);
    s
}

fn poll_with(colls: &[Coll], io: &mut dyn Io) -> Vec<Device> {
    let end = Instant::now() + BUDGET;
    let mut out: Vec<Device> = Vec::new();
    for (receiver, mouse_pid, name) in MODELS {
        for c in find(colls, VID, receiver, |c| {
            c.iface == 1
                && c.page == 0x008C
                && c.usage == 1
                && c.in_len == 33
                && c.out_len == 33
                && c.feat_len == 1001
        }) {
            let deadline = end.min(Instant::now() + Duration::from_millis(1000));
            let Some(mut d) = io.open(c) else { continue };
            // Leave time to reread the bond and retry if the first battery request goes unanswered.
            let first = deadline.min(Instant::now() + Duration::from_millis(300));
            let mut pct = read_battery(d.as_mut(), mouse_pid, first);
            if pct.is_none() && Instant::now() < deadline {
                pct = read_battery(d.as_mut(), mouse_pid, deadline);
            }
            let Some(pct) = pct else { continue };
            // A second identical receiver gets "#2" (BatteryHub's StableKey).
            let base = format!("darmoshark:{}", hex4(receiver));
            let n = out
                .iter()
                .filter(|o| o.key == base || o.key.starts_with(&format!("{base}#")))
                .count();
            let key = if n == 0 {
                base
            } else {
                format!("{base}#{}", n + 1)
            };
            out.push(mouse(key, name, pct));
            break;
        }
    }
    for c in find(colls, NRF_VID, NRF_RECEIVER, |c| {
        c.iface == 3 && c.page == 0xFF0A && c.usage == 1 && c.in_len == 65 && c.out_len == 65
    }) {
        if ms_left(end) == 0 {
            break;
        }
        if let Some(pct) = io.open(c).and_then(|mut d| nrf_read(d.as_mut(), end)) {
            out.push(mouse(
                format!("darmoshark:{}:{}", hex4(NRF_VID), hex4(NRF_RECEIVER)),
                "Darmoshark M3S Pro",
                pct,
            ));
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::hid_dev::fake::{Fake, Sent, coll};

    fn bond(mouse: u16) -> Vec<u8> {
        let mut r = vec![0u8; 1001];
        r[0] = FEATURE;
        r[1] = BOND;
        r[3..5].copy_from_slice(&VID.to_le_bytes());
        r[5..7].copy_from_slice(&mouse.to_le_bytes());
        r[7] = 1;
        r
    }

    fn identify(mouse: u16, pct: u8) -> Vec<u8> {
        let mut r = vec![0u8; 1001];
        r[0] = FEATURE;
        r[1] = IDENTIFY;
        r[2] = 1;
        r[4..6].copy_from_slice(&VID.to_le_bytes());
        r[6..8].copy_from_slice(&mouse.to_le_bytes());
        r[11] = pct;
        r
    }

    #[test]
    fn m3_reads_bond_then_identify() {
        let mut f = Fake::with(|s, _, kind, q| {
            assert_eq!(kind, Sent::Feature);
            assert_eq!(q[0], FEATURE);
            let r = if q[1] == BOND {
                bond(0xFF12)
            } else {
                identify(0xFF12, 63)
            };
            s.push_feature("", r);
        });
        let rx = coll(VID, 0xFF30, 1, 0x008C, 1, 33, 33, 1001);
        let out = poll_with(&[rx], &mut f);
        assert_eq!(out.len(), 1);
        assert_eq!(
            (out[0].name.as_str(), out[0].percent),
            ("Darmoshark M3", Some(63))
        );
        assert_eq!(out[0].key, "darmoshark:FF30");
    }

    #[test]
    fn bond_to_another_mouse_is_rejected() {
        assert!(!bond_ok(&bond(0xFF18), 0xFF12));
        assert_eq!(identify_percent(&identify(0xFF12, 101), 0xFF12), None);
        assert_eq!(identify_percent(&identify(0xFF18, 50), 0xFF18), Some(50));
    }

    #[test]
    fn nrf_frames_sum_to_a1() {
        let q = nrf_request();
        assert_eq!(q.len(), 65);
        let sum = q[1..].iter().fold(0u8, |a, b| a.wrapping_add(*b));
        assert_eq!(sum, 0xA1);
        let mut r = vec![0u8; 64];
        r[..4].copy_from_slice(&[0x41, 0, 0x8C, 1]);
        r[6] = 88;
        r[63] = nrf_checksum(&r);
        assert_eq!(nrf_parse(&r), Some(88));
        r[63] ^= 1;
        assert_eq!(nrf_parse(&r), None);
    }
}

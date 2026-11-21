//! Scyrox V12M on its 2.4 GHz receiver 395A:0006 (only this id: the V6S / V8S on the same vendor id
//! answer a different status command). Read-only profile read on FF00:0002 (64-byte reports):
//! output report 08 [08][01][00 ...][09] (checksum = low byte of 08 + 01). The receiver answers with
//! a stream of 64-byte input reports 08; the active profile's frame has [1] = 01, [3] != 0,
//! [4] = [5] = profile 0..3, [7] = DPI stage count 1..5, [62] = 0, and the battery percent at [20].
//! No charging field is confirmed, so charging is not shown.
//!
//! Ported from BatteryHub (src/drivers/scyrox.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0, which credits mousedock and
//! BrimDeck (from Scyrox's S-Center 2.0 web driver).

use std::time::Duration;

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::report_io::{self, Coll, Dev, Hid, MAX_WAIT, device, find};

pub const VID: u16 = 0x395A;
pub const PID_V12M_RECEIVER: u16 = 0x0006;

pub fn request() -> Vec<u8> {
    let mut req = vec![0u8; 64];
    req[0] = 0x08;
    req[1] = 0x01; // read profiles (current configuration)
    req[63] = 0x09; // low byte of 08 + 01
    req
}

#[derive(Debug, PartialEq)]
pub enum Frame {
    /// Axis frames, other replies, inactive profiles: keep reading.
    Skip,
    /// End of the stream, or a bad level.
    Stop,
    Battery(u8),
}

pub fn parse(r: &[u8]) -> Frame {
    if r.len() != 64 || r[0] != 0x08 || r[1] != 0x01 {
        return Frame::Skip;
    }
    if r[3] == 0 {
        return Frame::Stop; // end of the stream (or our own request echoed back)
    }
    if r[62] == 1 || r[4] > 3 || r[4] != r[5] || !(1..=5).contains(&r[7]) {
        return Frame::Skip; // not the active profile
    }
    if r[20] > 100 {
        return Frame::Stop;
    }
    Frame::Battery(r[20])
}

fn read_battery(hid: &dyn Hid, d: &mut dyn Dev) -> Option<u8> {
    d.flush();
    if !d.write(&request()) {
        return None;
    }
    let end = hid.now() + MAX_WAIT;
    let mut pct = None;
    loop {
        let left = end.saturating_sub(hid.now());
        if left.is_zero() {
            break;
        }
        let Some(r) = d.read(left) else { break };
        match parse(&r) {
            Frame::Skip => {}
            Frame::Stop => break,
            Frame::Battery(p) => {
                pct = Some(p);
                break;
            }
        }
    }
    d.flush(); // drop the rest of the profile stream
    pct
}

fn poll_with(hid: &mut dyn Hid, colls: &[Coll]) -> Vec<Device> {
    let cs = find(colls, VID, PID_V12M_RECEIVER, |c| {
        c.page == 0xFF00 && c.usage == 0x0002 && c.output == 64 && c.input == 64
    });
    for c in cs {
        let Some(mut d) = hid.open(c) else { continue };
        // None: mouse asleep / off, or no valid reply.
        let Some(pct) = read_battery(hid, d.as_mut()) else {
            continue;
        };
        let mut s = device(
            format!("scyrox:{PID_V12M_RECEIVER:04X}"),
            "Scyrox V12M",
            Kind::Mouse,
            "2.4 GHz receiver",
        );
        s.grace = Duration::from_secs(30 * 60);
        s.percent = Some(pct);
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
    use std::collections::VecDeque;

    fn frame(has_more: u8, profile: u8, active: u8, stages: u8, pct: u8) -> Vec<u8> {
        let mut r = vec![0u8; 64];
        r[0] = 0x08;
        r[1] = 0x01;
        r[3] = has_more;
        r[4] = active;
        r[5] = profile;
        r[7] = stages;
        r[20] = pct;
        r
    }

    fn fake(stream: Vec<Vec<u8>>) -> Fake {
        let mut q: VecDeque<Vec<u8>> = VecDeque::new();
        let mut stream = Some(stream);
        Fake::new(move |_, op| match op {
            Op::Write(w) => {
                assert_eq!(w, request());
                q.extend(stream.take().unwrap_or_default());
                Some(vec![])
            }
            Op::Read(_) => q.pop_front(),
            Op::Flush => {
                q.clear();
                Some(vec![])
            }
            _ => None,
        })
    }

    fn coll() -> Coll {
        Coll {
            vid: VID,
            pid: PID_V12M_RECEIVER,
            page: 0xFF00,
            usage: 0x0002,
            input: 64,
            output: 64,
            ..Default::default()
        }
    }

    #[test]
    fn active_profile_frame_carries_the_battery() {
        let mut axis = vec![0u8; 64];
        axis[0] = 0x08;
        axis[1] = 0x23;
        let stream = vec![
            frame(1, 0, 2, 4, 50), // profile 0 while profile 2 is active: skipped
            axis,
            frame(1, 2, 2, 4, 77),
            frame(0, 0, 0, 0, 0),
        ];
        let out = poll_with(&mut fake(stream), &[coll()]);
        assert_eq!(out[0].percent, Some(77));
        assert_eq!(
            (out[0].key.as_str(), out[0].name.as_str()),
            ("scyrox:0006", "Scyrox V12M")
        );
        assert!(!out[0].charging);
    }

    #[test]
    fn end_of_stream_error_flag_and_bad_level() {
        assert!(poll_with(&mut fake(vec![frame(0, 0, 0, 1, 50)]), &[coll()]).is_empty());
        let mut err = frame(1, 1, 1, 3, 50);
        err[62] = 1;
        assert!(poll_with(&mut fake(vec![err]), &[coll()]).is_empty());
        assert!(poll_with(&mut fake(vec![frame(1, 1, 1, 3, 101)]), &[coll()]).is_empty());
        let mut other = coll();
        other.output = 65;
        let mut f = fake(vec![]);
        assert!(poll_with(&mut f, &[other]).is_empty());
        assert_eq!(f.opens.get(), 0);
    }
}

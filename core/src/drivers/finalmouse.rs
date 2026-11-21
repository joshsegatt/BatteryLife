//! Finalmouse UltralightX (ULX) and Starlight X (SLX) on their 2.4 GHz dongles (VID 361D).
//! Frames on the vendor collection (page FF00), zero-padded: [report id][2 + n][0x80 | command][n]
//! [payload]; an empty payload is a GET, replies come back with the plain command.
//!  * ULX dongle 0100 (FF00:0001): output report 04, replies on input report 05.
//!  * SLX dongles 0300 / 0301 (FF00, usage not pinned down): output 01, replies on input 02.
//!
//! Only GETs are sent: the read xpanel starts with (ULX 21, SLX 57), then wake/all (96), which makes
//! the dongle push its status burst: 5 = mV u16 LE, 38 = [percent][mV u16 LE], 37 = charging,
//! 36 = link (0 = no link to the mouse). The mice on their own cable are not covered.
//!
//! Ported from BatteryHub (src/drivers/finalmouse.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0, which credits OpenMouse
//! mouse-protocol and FinalmousePollingRateSwitcher (both from Finalmouse's xpanel).

use std::time::Duration;

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::report_io::{self, Coll, Dev, Hid, MAX_WAIT, device, find};

pub const VID: u16 = 0x361D;
pub const PIDS: &[u16] = &[0x0100, 0x0300, 0x0301];
const VOLTAGE: u8 = 5;
const LINK: u8 = 36;
const CHARGING: u8 = 37;
const STATE_OF_CHARGE: u8 = 38;
const WAKE_ALL: u8 = 96;

struct Model {
    pid: u16,
    name: &'static str,
    slx: bool,
    out_id: u8,
    in_id: u8,
    /// The read xpanel sends before wake/all.
    first_get: u8,
}

const MODELS: &[Model] = &[
    Model {
        pid: 0x0100,
        name: "Finalmouse UltralightX",
        slx: false,
        out_id: 0x04,
        in_id: 0x05,
        first_get: 21,
    },
    Model {
        pid: 0x0300,
        name: "Finalmouse Starlight X",
        slx: true,
        out_id: 0x01,
        in_id: 0x02,
        first_get: 57,
    },
    Model {
        pid: 0x0301,
        name: "Finalmouse Starlight X",
        slx: true,
        out_id: 0x01,
        in_id: 0x02,
        first_get: 57,
    },
];

/// xpanel's voltage curve: linear between the calibration points, rounded to the nearest percent.
pub fn percent_from_millivolts(mv: u16) -> u8 {
    const MV: [u16; 7] = [3000, 3620, 3660, 3740, 3880, 4170, 4380];
    const PCT: [f64; 7] = [0.2, 5.0, 10.0, 25.0, 50.0, 75.0, 100.0];
    if mv <= MV[0] {
        return 0;
    }
    for i in 1..MV.len() {
        if mv > MV[i] {
            continue;
        }
        let f = f64::from(mv - MV[i - 1]) / f64::from(MV[i] - MV[i - 1]);
        return (PCT[i - 1] + (PCT[i] - PCT[i - 1]) * f + 0.5).floor() as u8;
    }
    100
}

#[derive(Debug, Default, PartialEq)]
pub struct Status {
    pub soc: Option<u8>,
    pub mv: Option<u16>,
    pub charging: Option<u8>,
    pub link: Option<u8>,
}

fn valid_mv(mv: u16) -> Option<u16> {
    (mv > 0 && mv < 5000).then_some(mv)
}

/// Folds one input report of the status burst into `st`.
pub fn decode(r: &[u8], in_id: u8, st: &mut Status) {
    if r.len() < 4 || r[0] != in_id {
        return;
    }
    let inner = usize::from(r[1]).min(r.len() - 2);
    if inner < 2 {
        return;
    }
    let n = usize::from(r[3]).min(inner - 2);
    let p = &r[4..];
    match r[2] {
        VOLTAGE if n >= 2 => {
            if let Some(mv) = valid_mv(u16::from_le_bytes([p[0], p[1]])) {
                st.mv = Some(mv);
            }
        }
        STATE_OF_CHARGE if n >= 3 && p[0] <= 100 => {
            st.soc = Some(p[0]);
            if let Some(mv) = valid_mv(u16::from_le_bytes([p[1], p[2]])) {
                st.mv = Some(mv);
            }
        }
        CHARGING if n >= 1 => st.charging = Some(p[0]),
        LINK if n >= 1 => st.link = Some(p[0]),
        _ => {}
    }
}

fn get(d: &mut dyn Dev, report_id: u8, command: u8) -> bool {
    d.write(&[report_id, 2, 0x80 | command, 0])
}

fn query(hid: &dyn Hid, d: &mut dyn Dev, m: &Model) -> Option<Status> {
    if !get(d, m.out_id, m.first_get) || !get(d, m.out_id, WAKE_ALL) {
        return None;
    }
    // xpanel waits for the burst. Once the level is in, the charging state normally follows in the
    // same burst, so only wait briefly for it. The SLX's own percentage (38) beats its voltage.
    let mut st = Status::default();
    let mut end = hid.now() + MAX_WAIT;
    let mut shortened = false;
    loop {
        let level = st.soc.is_some() || (!m.slx && st.mv.is_some());
        if level && st.charging.is_some() {
            break;
        }
        if level && !shortened {
            end = end.min(hid.now() + Duration::from_millis(150));
            shortened = true;
        }
        let left = end.saturating_sub(hid.now());
        if left.is_zero() {
            break;
        }
        let Some(r) = d.read(left) else { break };
        decode(&r, m.in_id, &mut st);
    }
    Some(st)
}

fn poll_with(hid: &mut dyn Hid, colls: &[Coll]) -> Vec<Device> {
    let mut out = Vec::new();
    for m in MODELS {
        let mut cs = find(colls, VID, m.pid, |c| {
            c.page == 0xFF00 && (m.slx || c.usage == 1) && c.input >= 4 && c.output >= 4
        });
        // The SLX vendor usage is not known; try usage 1 (the ULX one) first.
        cs.sort_by_key(|c| c.usage != 1);
        for c in cs {
            let Some(mut d) = hid.open(c) else { continue };
            let Some(st) = query(hid, d.as_mut(), m) else {
                continue;
            };
            let mut s = device(
                format!("finalmouse:{:04X}", m.pid),
                m.name,
                Kind::Mouse,
                "2.4 GHz receiver",
            );
            if let Some(soc) = st.soc {
                s.percent = Some(soc);
            } else if let Some(mv) = st.mv {
                s.percent = Some(percent_from_millivolts(mv));
            } else if st.link == Some(0) {
                s.asleep = true; // the dongle answered, the mouse is asleep or off
            } else {
                continue; // no answer: the tracker keeps the last reading during the grace period
            }
            s.millivolts = st.mv;
            s.charging = st.charging == Some(1);
            s.grace = Duration::from_secs(30 * 60);
            out.push(s);
            break;
        }
    }
    out
}

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    report_io::run(api, infos, poll_with)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::report_io::fake::{Fake, Op};
    use std::collections::HashMap;

    fn coll(pid: u16, path: &str, usage: u16, page: u16) -> Coll {
        Coll {
            vid: VID,
            pid,
            page,
            usage,
            input: 64,
            output: 64,
            path: path.into(),
            ..Default::default()
        }
    }

    /// Input report as a read hands it over: [id][2 + n][command][n][payload], zero-padded.
    fn reply(id: u8, command: u8, payload: &[u8]) -> Vec<u8> {
        let mut r = vec![0u8; 64];
        r[0] = id;
        r[1] = 2 + payload.len() as u8;
        r[2] = command;
        r[3] = payload.len() as u8;
        r[4..4 + payload.len()].copy_from_slice(payload);
        r
    }
    fn ulx(command: u8, payload: &[u8]) -> Vec<u8> {
        reply(0x05, command, payload)
    }
    fn slx(command: u8, payload: &[u8]) -> Vec<u8> {
        reply(0x02, command, payload)
    }
    fn mv(v: u16) -> [u8; 2] {
        v.to_le_bytes()
    }
    fn get_req(id: u8, command: u8) -> Vec<u8> {
        vec![id, 2, 0x80 | command, 0]
    }

    /// Scripted dongle: writing wake/all on a collection queues that collection's burst.
    fn dongle(
        bursts: HashMap<&'static str, Vec<Vec<u8>>>,
        reject: &'static [&'static str],
    ) -> Fake {
        let mut pending: Vec<Vec<u8>> = Vec::new();
        Fake::new(move |c, op| match op {
            Op::Write(w) => {
                assert!(w.len() == 4 && w[1] == 2 && w[2] & 0x80 != 0 && w[3] == 0);
                if reject.contains(&c.path.as_str()) {
                    return None;
                }
                if w[2] == 0x80 | 96 {
                    pending = bursts.get(c.path.as_str()).cloned().unwrap_or_default();
                }
                Some(vec![])
            }
            Op::Read(_) => (!pending.is_empty()).then(|| pending.remove(0)),
            _ => None,
        })
    }

    fn poll(colls: &[Coll], bursts: Vec<(&'static str, Vec<Vec<u8>>)>) -> (Vec<Device>, Fake) {
        let mut f = dongle(bursts.into_iter().collect(), &[]);
        (poll_with(&mut f, colls), f)
    }

    fn writes(f: &Fake) -> Vec<(String, Vec<u8>)> {
        f.log
            .borrow()
            .iter()
            .filter_map(|(p, o)| match o {
                Op::Write(w) => Some((p.clone(), w.clone())),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn ultralight_x_voltage_and_charging() {
        let (out, f) = poll(
            &[coll(0x0100, "ulx", 1, 0xFF00)],
            vec![(
                "ulx",
                vec![
                    reply(0x03, 10, b"1.0\0"), // dongle firmware on report 03: ignored
                    ulx(3, &mv(1600)),         // DPI
                    ulx(5, &mv(3880)),
                    ulx(37, &[1]),
                ],
            )],
        );
        let w = writes(&f);
        assert_eq!(w.len(), 2);
        assert_eq!(
            (w[0].1.clone(), w[1].1.clone()),
            (get_req(4, 21), get_req(4, 96))
        );
        let s = &out[0];
        assert_eq!(
            (s.percent, s.millivolts, s.charging, s.asleep),
            (Some(50), Some(3880), true, false)
        );
        assert_eq!(
            (s.name.as_str(), s.key.as_str()),
            ("Finalmouse UltralightX", "finalmouse:0100")
        );
        assert_eq!((s.kind, s.via.as_str()), (Kind::Mouse, "2.4 GHz receiver"));
        assert_eq!(s.grace, Duration::from_secs(1800));
    }

    #[test]
    fn xpanel_curve_and_state_of_charge() {
        let ulx_c = coll(0x0100, "ulx", 1, 0xFF00);
        for (v, pct) in [
            (2950, 0),
            (3000, 0),
            (3640, 8),
            (3880, 50),
            (4276, 88),
            (4380, 100),
            (4450, 100),
        ] {
            let (out, _) = poll(
                std::slice::from_ref(&ulx_c),
                vec![("ulx", vec![ulx(37, &[0]), ulx(5, &mv(v))])],
            );
            assert_eq!(
                (out[0].percent, out[0].charging),
                (Some(pct), false),
                "{v} mV"
            );
        }
        // A level without a charging report is still shown.
        let (out, _) = poll(
            std::slice::from_ref(&ulx_c),
            vec![("ulx", vec![ulx(5, &mv(4170))])],
        );
        assert_eq!((out[0].percent, out[0].charging), (Some(75), false));
        // A state of charge (38) wins over the voltage curve.
        let (out, _) = poll(
            std::slice::from_ref(&ulx_c),
            vec![(
                "ulx",
                vec![ulx(5, &mv(3880)), ulx(38, &[64, 0x2C, 0x0F]), ulx(37, &[0])],
            )],
        );
        assert_eq!((out[0].percent, out[0].millivolts), (Some(64), Some(3884)));
    }

    #[test]
    fn link_down_is_asleep_and_bad_frames_are_ignored() {
        let ulx_c = coll(0x0100, "ulx", 1, 0xFF00);
        let (out, _) = poll(
            std::slice::from_ref(&ulx_c),
            vec![("ulx", vec![ulx(36, &[0]), ulx(5, &mv(0))])],
        );
        assert!(out[0].asleep && out[0].percent.is_none() && out[0].millivolts.is_none());
        let mut truncated = ulx(5, &mv(3880));
        truncated[1] = 3; // inner length too short for the 2-byte payload
        for burst in [
            vec![ulx(36, &[1])],
            vec![ulx(5, &[0x28])],
            vec![ulx(5, &mv(6000))],
            vec![ulx(38, &[101, 0x2C, 0x0F])],
            vec![slx(5, &mv(3880)), slx(37, &[1])], // SLX report id on the ULX dongle
            vec![reply(0x05, 5, &[])],
            vec![],
            vec![truncated],
        ] {
            let (out, _) = poll(std::slice::from_ref(&ulx_c), vec![("ulx", burst)]);
            assert!(out.is_empty());
        }
    }

    #[test]
    fn starlight_x() {
        let (out, f) = poll(
            &[coll(0x0300, "slx", 9, 0xFF00)],
            vec![(
                "slx",
                vec![
                    ulx(5, &mv(4380)), // ULX report id: ignored on SLX
                    slx(16, &mv(1600)),
                    slx(5, &mv(3880)),
                    slx(38, &[76, 0x44, 0x0F]),
                    slx(37, &[0]),
                ],
            )],
        );
        let w = writes(&f);
        assert_eq!(
            (w[0].1.clone(), w[1].1.clone()),
            (get_req(1, 57), get_req(1, 96))
        );
        assert_eq!(
            (out[0].percent, out[0].millivolts),
            (Some(76), Some(0x0F44))
        );
        assert_eq!(
            (out[0].name.as_str(), out[0].key.as_str()),
            ("Finalmouse Starlight X", "finalmouse:0300")
        );
        assert!(!out[0].charging);
        // Voltage only: xpanel's curve once the burst is over.
        let (out, _) = poll(
            &[coll(0x0301, "slx2", 1, 0xFF00)],
            vec![("slx2", vec![slx(37, &[1]), slx(5, &mv(4276))])],
        );
        assert_eq!(
            (out[0].percent, out[0].charging, out[0].key.as_str()),
            (Some(88), true, "finalmouse:0301")
        );
    }

    #[test]
    fn starlight_x_tries_usage_1_then_the_others() {
        let mut f = dongle(
            HashMap::from([("slx-c", vec![slx(38, &[40, 0, 0])])]),
            &["slx-a"],
        );
        let out = poll_with(
            &mut f,
            &[
                coll(0x0300, "slx-b", 2, 0xFF00),
                coll(0x0300, "slx-c", 3, 0xFF00),
                coll(0x0300, "slx-a", 1, 0xFF00),
            ],
        );
        let w = writes(&f);
        assert_eq!(w.len(), 1 + 2 + 2);
        assert_eq!((w[0].0.as_str(), w[1].0.as_str()), ("slx-a", "slx-b"));
        assert_eq!((out[0].percent, out[0].millivolts), (Some(40), None));
    }

    #[test]
    fn only_the_known_dongles_and_their_vendor_collection() {
        let mut no_output = coll(0x0100, "no-output", 1, 0xFF00);
        no_output.output = 0;
        let (out, f) = poll(
            &[
                coll(0x0102, "ulx-cable", 1, 0xFF00),
                coll(0x0310, "slx-cable", 1, 0xFF00),
                coll(0x0100, "ulx-u2", 2, 0xFF00),
                coll(0x0300, "slx-ff01", 1, 0xFF01),
                coll(0x0100, "ulx-mouse", 2, 0x0001),
                no_output,
            ],
            vec![],
        );
        assert!(out.is_empty());
        assert_eq!(f.opens.get(), 0);
    }
}

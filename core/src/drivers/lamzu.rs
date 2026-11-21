//! LAMZU wireless mice on their own vendor ids 373E and 37B0 (Maya X, Maya, Tachi, Inca, Paro, Thorn,
//! Atlantis Mini / OG Champion, Orcus ...). SET_FEATURE report 0 (64 bytes) [00][00][02][02][00][83],
//! ~100 ms later GET_FEATURE report 0: [A1][..][..][02][..][83][charging][percent] after the report
//! id byte (accepted with or without that byte, as LAMZU's web driver does). Charging 1 = on the
//! cable; percent 0 = the mouse is not linked to its dongle. Only this read is sent.
//! 373E is shared with Arbiter Studio, so only LAMZU's own 373E ids are declared.
//!
//! Ported from BatteryHub (src/drivers/lamzu.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0, which credits LAMZU's Aurora
//! web driver (lamzu.net).

use std::time::Duration;

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::report_io::{self, Coll, Hid, device, find, products};

pub const VID_ARBITER_SHARED: u16 = 0x373E;
/// LAMZU's ids on 373E (Maya X, Maya X V2); the rest of 373E is Arbiter Studio's.
pub const PIDS_373E: &[u16] = &[0x001C, 0x001D, 0x001E, 0x0016];
pub const VID: u16 = 0x37B0;

fn model_name(vid: u16, pid: u16) -> Option<&'static str> {
    const MODELS: &[(u16, &[u16], &str)] = &[
        (0x373E, &[0x001C, 0x001D, 0x001E], "LAMZU Maya X"),
        (0x373E, &[0x0016], "LAMZU Maya X V2"),
        (0x37B0, &[0x001F], "LAMZU Maya X V2"),
        (0x37B0, &[0x0005, 0x000B, 0x000C], "LAMZU Tachi"),
        (0x37B0, &[0x001C, 0x001E], "LAMZU Tachi Lite"),
        (0x37B0, &[0x0009, 0x000F, 0x0010], "LAMZU Inca"),
        (0x37B0, &[0x0011, 0x0013, 0x0015], "LAMZU Maya"),
        (0x37B0, &[0x0007, 0x000D, 0x000E], "LAMZU Paro"),
        (0x37B0, &[0x0017, 0x0019, 0x001B], "LAMZU Thorn"),
        (0x37B0, &[0x0021, 0x0003, 0x0023], "LAMZU Thorn V2"),
        (0x37B0, &[0x0030, 0x0032, 0x0040], "LAMZU Thorn V2"),
        (0x37B0, &[0x0025, 0x0027], "LAMZU Atlantis OG Champion"),
        (0x37B0, &[0x0028, 0x002B, 0x002A], "LAMZU Atlantis Mini"),
        (0x37B0, &[0x003E, 0x003F], "LAMZU Atlantis Mini"),
        (0x37B0, &[0x002C, 0x0052], "LAMZU Maya M"),
        (0x37B0, &[0x003A, 0x003B, 0x0044], "LAMZU Maya X"),
        (0x37B0, &[0x003C, 0x0050], "LAMZU Maya"),
        (0x37B0, &[0x0042, 0x0056], "LAMZU Orcus"),
        (0x37B0, &[0x0046, 0x0054], "LAMZU Mini"),
    ];
    MODELS
        .iter()
        .find(|(v, pids, _)| *v == vid && pids.contains(&pid))
        .map(|m| m.2)
}

/// The mouse itself on its USB cable.
fn is_cable(vid: u16, pid: u16) -> bool {
    if vid == 0x373E {
        return pid == 0x001C;
    }
    matches!(
        pid,
        0x0005
            | 0x0007
            | 0x0009
            | 0x0011
            | 0x0017
            | 0x001C
            | 0x001F
            | 0x0021
            | 0x0025
            | 0x0028
            | 0x002C
            | 0x0030
            | 0x0034
            | 0x0038
            | 0x003A
            | 0x003C
            | 0x003E
            | 0x0040
            | 0x0042
            | 0x0044
            | 0x0046
            | 0x0050
            | 0x0052
            | 0x0056
    )
}

/// The SET_FEATURE buffer: report id 0, then the request.
pub fn request() -> Vec<u8> {
    let mut req = vec![0u8; 65];
    req[3] = 0x02;
    req[4] = 0x02;
    req[6] = 0x83;
    req
}

/// (charging, percent) from a GET_FEATURE buffer, with or without the report id byte.
pub fn parse(r: &[u8]) -> Option<(bool, u8)> {
    if r.len() < 10 {
        return None;
    }
    let (chg, pct) = if r[1] == 0xA1 && r[4] == 0x02 && r[6] == 0x83 {
        (r[7], r[8])
    } else if r[0] == 0xA1 && r[3] == 0x02 && r[5] == 0x83 {
        (r[6], r[7])
    } else {
        return None;
    };
    (pct <= 100).then_some((chg == 1, pct))
}

fn poll_with(hid: &mut dyn Hid, colls: &[Coll]) -> Vec<Device> {
    let mut out = Vec::new();
    for (vid, pid) in products(colls) {
        for c in find(colls, vid, pid, |c| c.feature == 65) {
            let Some(mut d) = hid.open(c) else { continue };
            if !d.set_feature(&request()) {
                continue;
            }
            hid.sleep(Duration::from_millis(100));
            let Some((charging, pct)) = d.get_feature(0).and_then(|r| parse(&r)) else {
                continue;
            };
            let cable = is_cable(vid, pid);
            // 0 from a dongle = mouse off or out of range: not listed, the tracker keeps the level.
            if pct == 0 && !cable {
                break;
            }
            let mut s = device(
                format!("lamzu:{vid:04X}:{pid:04X}"),
                model_name(vid, pid).unwrap_or("LAMZU wireless mouse"),
                Kind::Mouse,
                if cable {
                    "USB cable"
                } else {
                    "2.4 GHz receiver"
                },
            );
            if !cable {
                s.grace = Duration::from_secs(30 * 60);
            }
            s.percent = (pct > 0).then_some(pct);
            s.charging = charging;
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

    fn reply(chg: u8, pct: u8) -> Vec<u8> {
        let mut r = vec![0u8; 65];
        r[1] = 0xA1;
        r[4] = 0x02;
        r[6] = 0x83;
        r[7] = chg;
        r[8] = pct;
        r
    }

    fn fake(r: Vec<u8>) -> Fake {
        Fake::new(move |_, op| match op {
            Op::SetFeature(w) => {
                assert_eq!(w, request());
                Some(vec![])
            }
            Op::GetFeature(0) => Some(r.clone()),
            _ => None,
        })
    }

    fn coll(vid: u16, pid: u16) -> Coll {
        Coll {
            vid,
            pid,
            feature: 65,
            ..Default::default()
        }
    }

    #[test]
    fn both_reply_layouts() {
        assert_eq!(parse(&reply(0, 73)), Some((false, 73)));
        assert_eq!(parse(&reply(1, 73)[1..]), Some((true, 73)));
        assert_eq!(parse(&reply(0, 101)), None);
        assert_eq!(parse(&reply(0, 50)[..9]), None);
    }

    #[test]
    fn thorn_on_its_dongle() {
        let out = poll_with(&mut fake(reply(0, 73)), &[coll(0x37B0, 0x0019)]);
        let s = &out[0];
        assert_eq!(
            (s.name.as_str(), s.percent, s.charging),
            ("LAMZU Thorn", Some(73), false)
        );
        assert_eq!(
            (s.key.as_str(), s.via.as_str()),
            ("lamzu:37B0:0019", "2.4 GHz receiver")
        );
    }

    #[test]
    fn zero_percent_dongle_is_off_but_a_cable_is_listed() {
        assert!(poll_with(&mut fake(reply(0, 0)), &[coll(0x37B0, 0x0019)]).is_empty());
        let out = poll_with(&mut fake(reply(1, 0)), &[coll(0x37B0, 0x0017)]);
        assert_eq!(
            (out[0].via.as_str(), out[0].percent, out[0].charging),
            ("USB cable", None, true)
        );
        let out = poll_with(&mut fake(reply(0, 90)), &[coll(0x37B0, 0x0099)]);
        assert_eq!(out[0].name, "LAMZU wireless mouse");
    }
}

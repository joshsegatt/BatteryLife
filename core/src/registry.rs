//! Declarative driver registry and the parallel poller.
//!
//! Each HID driver declares which devices it handles. A device goes to the most specific match: an
//! exact vendor + product id beats a whole-vendor match, so drivers that share a vendor id need no
//! hand-kept ordering. A product no driver claims exactly can also have a chain of drivers to try
//! after its whole-vendor driver (`CHAINS`): vendors whose mice speak one of several protocols.
//! Drivers then run in parallel, each with a deadline, so one slow or stuck device cannot hold up
//! the others; a chain moves on round by round.

use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use hidapi::{DeviceInfo, HidApi};

use crate::device::Device;
use crate::drivers::{
    ajazz_ak992, akg, akko, alienware, arbiter, areson, astro, atk_bitmouse, atk_keyboard, audeze,
    aula, aula_mouse, barracuda_pro, cherry, chimera, coolermaster, dareu, darkrim, darmoshark,
    endorfy, eweadn, fantech, fractal, hyperx, jabra, jbl, keychron, kysona, logitech_g435_0adf,
    machenike, maono, mchose, mchose_a5, mchose_g3, mchose_headset, mchose_keyboard, mchose_modern,
    misc_mice, msi, niceboy, nuphy, nzxt, plantronics, pressplay_icarus, raigor, rapoo, rawm,
    razer_headset, redragon_evision, rog, rongyuan, royal_kludge, semico, sinowealth_receiver,
    sonix, sony_inzone, sprime, steelseries_elite, turtlebeach, via_dongle, zenith,
};
use crate::drivers::{bluetooth, generic_hid, logitech, phonelink, xbox};
use crate::drivers::{
    corsair, endgame, endgame_we, finalmouse, glorious, lamzu, ninjutso, pulsar, razer, scyrox,
    steelseries, vaxee, vgn, wlmouse, zowie,
};

/// Devices a driver handles: one vendor, and either listed product ids or (empty) the whole vendor.
pub struct Match {
    pub vid: u16,
    pub pids: &'static [u16],
}

pub struct HidDriver {
    pub name: &'static str,
    pub matches: &'static [Match],
    /// Polls only the HID collections assigned to this driver. Blocking, read-only.
    pub poll: fn(&HidApi, &[DeviceInfo]) -> Vec<Device>,
}

/// Drivers tried, in order, on the products of `vid` that no driver claims exactly, after the
/// whole-vendor driver of `vid` (if any), as BatteryHub tries its protocols in turn.
pub struct Chain {
    pub vid: u16,
    /// Driver names, in the order they are asked.
    pub drivers: &'static [&'static str],
}

/// G-Wolves and Hitscan mice speak either Glorious' protocol or Kysona's (glorious.cpp then
/// kysona.cpp); Kysona's protocol also runs on vendor ids shared with AULA, Chimera, ATK, Pulsar and
/// HECATE headsets, whose own products are claimed exactly (kysona.rs skips the ones it must never
/// send mouse frames to).
pub static CHAINS: &[Chain] = &[
    Chain {
        vid: 0x3770,
        drivers: &["glorious", "kysona"],
    },
    Chain {
        vid: 0x33E4,
        drivers: &["glorious", "kysona"],
    },
    Chain {
        vid: 0x3554,
        drivers: &["kysona"],
    },
    Chain {
        vid: 0x373B,
        drivers: &["kysona"],
    },
    Chain {
        vid: 0x3710,
        drivers: &["kysona"],
    },
    Chain {
        vid: 0x35BB,
        drivers: &["kysona"],
    },
];

/// A source that needs no HID access (files, OS APIs).
pub struct Source {
    pub name: &'static str,
    pub poll: fn() -> Vec<Device>,
}

pub static HID_DRIVERS: &[HidDriver] = &[
    HidDriver {
        name: "logitech",
        matches: &[Match {
            vid: logitech::VID,
            pids: &[],
        }],
        poll: logitech::poll,
    },
    // Mice, part A. Razer headsets share 1532, so Razer declares OpenRazer's keyboard and mouse ids.
    HidDriver {
        name: "razer",
        matches: &[
            Match {
                vid: razer::VID,
                pids: razer::KEYBOARD_PIDS,
            },
            Match {
                vid: razer::VID,
                pids: razer::MOUSE_PIDS,
            },
        ],
        poll: razer::poll,
    },
    HidDriver {
        name: "steelseries",
        matches: &[Match {
            vid: steelseries::VID,
            pids: steelseries::PIDS,
        }],
        poll: steelseries::poll,
    },
    HidDriver {
        name: "corsair",
        matches: &[Match {
            vid: corsair::VID,
            pids: &[],
        }],
        poll: corsair::poll,
    },
    HidDriver {
        name: "glorious",
        matches: &[
            Match {
                vid: glorious::VID_SINOWEALTH,
                pids: &[],
            },
            Match {
                vid: glorious::VID_HITSCAN,
                pids: glorious::HITSCAN_PIDS,
            },
            Match {
                vid: glorious::VID_WLMOUSE,
                pids: glorious::WLMOUSE_YING_PIDS,
            },
        ],
        poll: glorious::poll,
    },
    HidDriver {
        name: "pulsar",
        matches: &[Match {
            vid: pulsar::VID,
            pids: pulsar::PIDS,
        }],
        poll: pulsar::poll,
    },
    HidDriver {
        name: "lamzu",
        matches: &[
            Match {
                vid: lamzu::VID_ARBITER_SHARED,
                pids: lamzu::PIDS_373E,
            },
            Match {
                vid: lamzu::VID,
                pids: &[],
            },
        ],
        poll: lamzu::poll,
    },
    HidDriver {
        name: "vgn",
        matches: &[Match {
            vid: vgn::VID,
            pids: &[vgn::PID_ZHOU_WIRELESS],
        }],
        poll: vgn::poll,
    },
    HidDriver {
        name: "finalmouse",
        matches: &[Match {
            vid: finalmouse::VID,
            pids: finalmouse::PIDS,
        }],
        poll: finalmouse::poll,
    },
    HidDriver {
        name: "wlmouse",
        matches: &[Match {
            vid: wlmouse::VID,
            pids: wlmouse::PIDS,
        }],
        poll: wlmouse::poll,
    },
    HidDriver {
        name: "vaxee",
        matches: &[Match {
            vid: vaxee::VID,
            pids: vaxee::PIDS,
        }],
        poll: vaxee::poll,
    },
    HidDriver {
        name: "ninjutso",
        matches: &[
            Match {
                vid: ninjutso::VID_NORDIC,
                pids: ninjutso::SORA_PIDS,
            },
            Match {
                vid: ninjutso::VID_PIXART,
                pids: ninjutso::TEN_PIDS,
            },
        ],
        poll: ninjutso::poll,
    },
    HidDriver {
        name: "scyrox",
        matches: &[Match {
            vid: scyrox::VID,
            pids: &[scyrox::PID_V12M_RECEIVER],
        }],
        poll: scyrox::poll,
    },
    HidDriver {
        name: "endgame",
        matches: &[Match {
            vid: endgame::VID,
            pids: &[endgame::PID_DONGLE_4K],
        }],
        poll: endgame::poll,
    },
    HidDriver {
        name: "endgame-we",
        matches: &[Match {
            vid: endgame_we::VID,
            pids: &[endgame_we::PID],
        }],
        poll: endgame_we::poll,
    },
    HidDriver {
        name: "zowie",
        matches: &[Match {
            vid: zowie::VID,
            pids: &[],
        }],
        poll: zowie::poll,
    },
    // Keyboard batch ported from BatteryHub (and the mice those files also cover).
    HidDriver {
        name: "aula",
        matches: aula::MATCHES,
        poll: aula::poll,
    },
    HidDriver {
        name: "dareu",
        matches: &[Match {
            vid: dareu::VID,
            pids: dareu::PIDS,
        }],
        poll: dareu::poll,
    },
    HidDriver {
        name: "atk_keyboard",
        matches: atk_keyboard::MATCHES,
        poll: atk_keyboard::poll,
    },
    HidDriver {
        name: "redragon_evision",
        matches: &[Match {
            vid: redragon_evision::VID,
            pids: redragon_evision::PIDS,
        }],
        poll: redragon_evision::poll,
    },
    HidDriver {
        name: "keychron",
        matches: &[Match {
            vid: keychron::VID,
            pids: &[],
        }],
        poll: keychron::poll,
    },
    HidDriver {
        name: "cherry",
        matches: &[Match {
            vid: cherry::VID,
            pids: cherry::PIDS,
        }],
        poll: cherry::poll,
    },
    HidDriver {
        name: "nuphy",
        matches: &[Match {
            vid: nuphy::VID,
            pids: nuphy::PIDS,
        }],
        poll: nuphy::poll,
    },
    HidDriver {
        name: "akko",
        matches: &[Match {
            vid: akko::VID,
            pids: akko::PIDS,
        }],
        poll: akko::poll,
    },
    HidDriver {
        name: "rongyuan",
        matches: rongyuan::MATCHES,
        poll: rongyuan::poll,
    },
    HidDriver {
        name: "sonix",
        matches: sonix::MATCHES,
        poll: sonix::poll,
    },
    HidDriver {
        name: "via_dongle",
        matches: &[Match {
            vid: via_dongle::VID,
            pids: via_dongle::PIDS,
        }],
        poll: via_dongle::poll,
    },
    HidDriver {
        name: "semico",
        matches: &[Match {
            vid: semico::VID,
            pids: semico::PIDS,
        }],
        poll: semico::poll,
    },
    HidDriver {
        name: "ajazz_ak992",
        matches: &[Match {
            vid: ajazz_ak992::VID,
            pids: ajazz_ak992::PIDS,
        }],
        poll: ajazz_ak992::poll,
    },
    HidDriver {
        name: "royal_kludge",
        matches: &[Match {
            vid: royal_kludge::VID,
            pids: royal_kludge::PIDS,
        }],
        poll: royal_kludge::poll,
    },
    HidDriver {
        name: "machenike",
        matches: &[Match {
            vid: machenike::VID,
            pids: machenike::PIDS,
        }],
        poll: machenike::poll,
    },
    HidDriver {
        name: "niceboy",
        matches: &[Match {
            vid: niceboy::VID,
            pids: niceboy::PIDS,
        }],
        poll: niceboy::poll,
    },
    HidDriver {
        name: "mchose_keyboard",
        matches: &[Match {
            vid: mchose_keyboard::VID,
            pids: mchose_keyboard::PIDS,
        }],
        poll: mchose_keyboard::poll,
    },
    HidDriver {
        name: "darkrim",
        matches: &[Match {
            vid: darkrim::VID,
            pids: darkrim::PIDS,
        }],
        poll: darkrim::poll,
    },
    // Headsets (and a few mice that live in the same BatteryHub files), exact product ids except
    // Jabra, whose driver asks every Jabra product.
    HidDriver {
        name: "hyperx",
        matches: &[
            Match {
                vid: hyperx::VID_HP,
                pids: hyperx::HP_PIDS,
            },
            Match {
                vid: hyperx::VID_KINGSTON,
                pids: hyperx::KINGSTON_PIDS,
            },
        ],
        poll: hyperx::poll,
    },
    HidDriver {
        name: "steelseries-elite",
        matches: &[Match {
            vid: steelseries_elite::VID,
            pids: &[steelseries_elite::PID],
        }],
        poll: steelseries_elite::poll,
    },
    HidDriver {
        name: "razer-headset",
        matches: &[Match {
            vid: razer_headset::VID,
            pids: razer_headset::PIDS,
        }],
        poll: razer_headset::poll,
    },
    HidDriver {
        name: "barracuda-pro",
        matches: &[Match {
            vid: barracuda_pro::VID,
            pids: &[barracuda_pro::PID],
        }],
        poll: barracuda_pro::poll,
    },
    HidDriver {
        name: "astro",
        matches: &[
            Match {
                vid: astro::VID_LOGITECH,
                pids: astro::LOGITECH_PIDS,
            },
            Match {
                vid: astro::VID_ASTRO,
                pids: astro::ASTRO_PIDS,
            },
        ],
        poll: astro::poll,
    },
    HidDriver {
        name: "audeze",
        matches: &[Match {
            vid: audeze::VID,
            pids: audeze::PIDS,
        }],
        poll: audeze::poll,
    },
    HidDriver {
        name: "jabra",
        matches: &[Match {
            vid: jabra::VID,
            pids: &[],
        }],
        poll: jabra::poll,
    },
    HidDriver {
        name: "jbl",
        matches: &[Match {
            vid: jbl::VID,
            pids: jbl::PIDS,
        }],
        poll: jbl::poll,
    },
    HidDriver {
        name: "plantronics",
        matches: &[Match {
            vid: plantronics::VID,
            pids: plantronics::PIDS,
        }],
        poll: plantronics::poll,
    },
    HidDriver {
        name: "turtlebeach",
        matches: &[Match {
            vid: turtlebeach::VID,
            pids: turtlebeach::PIDS,
        }],
        poll: turtlebeach::poll,
    },
    HidDriver {
        name: "akg",
        matches: &[Match {
            vid: akg::VID,
            pids: &[akg::PID],
        }],
        poll: akg::poll,
    },
    HidDriver {
        name: "nzxt",
        matches: &[Match {
            vid: nzxt::VID,
            pids: &[nzxt::RECEIVER, nzxt::CABLE],
        }],
        poll: nzxt::poll,
    },
    HidDriver {
        name: "sony-inzone",
        matches: &[Match {
            vid: sony_inzone::VID,
            pids: sony_inzone::PIDS,
        }],
        poll: sony_inzone::poll,
    },
    HidDriver {
        name: "fractal",
        matches: &[Match {
            vid: fractal::VID,
            pids: fractal::PIDS,
        }],
        poll: fractal::poll,
    },
    HidDriver {
        name: "mchose-headset",
        matches: &[Match {
            vid: mchose_headset::VID,
            pids: &[mchose_headset::PID],
        }],
        poll: mchose_headset::poll,
    },
    HidDriver {
        name: "logitech-g435-0adf",
        matches: &[Match {
            vid: logitech_g435_0adf::VID,
            pids: &[logitech_g435_0adf::PID],
        }],
        poll: logitech_g435_0adf::poll,
    },
    HidDriver {
        name: "endorfy",
        matches: &[Match {
            vid: endorfy::VID,
            pids: &[endorfy::PID],
        }],
        poll: endorfy::poll,
    },
    HidDriver {
        name: "maono",
        matches: &[Match {
            vid: maono::VID,
            pids: &[maono::PID],
        }],
        poll: maono::poll,
    },
    HidDriver {
        name: "alienware",
        matches: &[Match {
            vid: alienware::VID,
            pids: &[alienware::PID],
        }],
        poll: alienware::poll,
    },
    HidDriver {
        name: "pressplay-icarus",
        matches: &[Match {
            vid: pressplay_icarus::VID,
            pids: &[pressplay_icarus::PID],
        }],
        poll: pressplay_icarus::poll,
    },
    // Wireless mice, part B (BatteryHub ports).
    HidDriver {
        name: "rawm",
        matches: rawm::MATCHES,
        poll: rawm::poll,
    },
    HidDriver {
        name: "sprime",
        matches: sprime::MATCHES,
        poll: sprime::poll,
    },
    HidDriver {
        name: "darmoshark",
        matches: darmoshark::MATCHES,
        poll: darmoshark::poll,
    },
    HidDriver {
        name: "kysona",
        matches: kysona::MATCHES,
        poll: kysona::poll,
    },
    HidDriver {
        name: "chimera",
        matches: chimera::MATCHES,
        poll: chimera::poll,
    },
    HidDriver {
        name: "arbiter",
        matches: arbiter::MATCHES,
        poll: arbiter::poll,
    },
    HidDriver {
        name: "atk-bitmouse",
        matches: atk_bitmouse::MATCHES,
        poll: atk_bitmouse::poll,
    },
    HidDriver {
        name: "eweadn",
        matches: eweadn::MATCHES,
        poll: eweadn::poll,
    },
    HidDriver {
        name: "zenith",
        matches: zenith::MATCHES,
        poll: zenith::poll,
    },
    HidDriver {
        name: "misc-motospeed",
        matches: misc_mice::MOTOSPEED,
        poll: misc_mice::poll_motospeed,
    },
    HidDriver {
        name: "misc-xenta",
        matches: misc_mice::XENTA,
        poll: misc_mice::poll_xenta,
    },
    HidDriver {
        name: "misc-ajazz",
        matches: misc_mice::AJAZZ,
        poll: misc_mice::poll_ajazz,
    },
    HidDriver {
        name: "misc-pixart",
        matches: misc_mice::PIXART,
        poll: misc_mice::poll_pixart,
    },
    HidDriver {
        name: "fantech",
        matches: fantech::MATCHES,
        poll: fantech::poll,
    },
    HidDriver {
        name: "raigor",
        matches: raigor::MATCHES,
        poll: raigor::poll,
    },
    HidDriver {
        name: "rapoo",
        matches: rapoo::MATCHES,
        poll: rapoo::poll,
    },
    HidDriver {
        name: "areson",
        matches: areson::MATCHES,
        poll: areson::poll,
    },
    HidDriver {
        name: "coolermaster",
        matches: coolermaster::MATCHES,
        poll: coolermaster::poll,
    },
    HidDriver {
        name: "msi",
        matches: msi::MATCHES,
        poll: msi::poll,
    },
    HidDriver {
        name: "rog",
        matches: rog::MATCHES,
        poll: rog::poll,
    },
    HidDriver {
        name: "mchose",
        matches: mchose::MATCHES,
        poll: mchose::poll,
    },
    HidDriver {
        name: "mchose-a5",
        matches: mchose_a5::MATCHES,
        poll: mchose_a5::poll,
    },
    HidDriver {
        name: "mchose-g3",
        matches: mchose_g3::MATCHES,
        poll: mchose_g3::poll,
    },
    HidDriver {
        name: "mchose-modern",
        matches: mchose_modern::MATCHES,
        poll: mchose_modern::poll,
    },
    HidDriver {
        name: "sinowealth-receiver",
        matches: sinowealth_receiver::MATCHES,
        poll: sinowealth_receiver::poll,
    },
    HidDriver {
        name: "aula-mouse",
        matches: aula_mouse::MATCHES,
        poll: aula_mouse::poll,
    },
];

/// Gets every HID collection that no vendor driver matched.
pub static FALLBACK: &HidDriver = &HidDriver {
    name: "generic-hid",
    matches: &[],
    poll: generic_hid::poll,
};

pub static SOURCES: &[Source] = &[
    Source {
        name: "phonelink",
        poll: || phonelink::poll().into_iter().collect(),
    },
    Source {
        name: "bluetooth",
        poll: bluetooth::poll,
    },
    Source {
        name: "xbox",
        poll: xbox::poll,
    },
];

/// A driver gets this long; whatever it has not returned by then is dropped for this poll.
pub const DRIVER_TIMEOUT: Duration = Duration::from_secs(5);

/// Index of the driver that handles (vid, pid): exact product match first, then a whole-vendor match,
/// then the first listed driver on a tie.
pub fn pick(drivers: &[HidDriver], vid: u16, pid: u16) -> Option<usize> {
    let score = |d: &HidDriver| {
        d.matches
            .iter()
            .filter(|m| m.vid == vid)
            .map(|m| {
                if m.pids.contains(&pid) {
                    2
                } else if m.pids.is_empty() {
                    1
                } else {
                    0
                }
            })
            .max()
            .unwrap_or(0)
    };
    let (best, s) = drivers
        .iter()
        .enumerate()
        .map(|(i, d)| (i, score(d)))
        .fold((0, 0), |acc, x| if x.1 > acc.1 { x } else { acc });
    (s > 0).then_some(best)
}

/// Drivers to try on (vid, pid), in order. A product a driver claims exactly is a known product of
/// that driver, so it goes to that driver alone; any other product goes to its whole-vendor driver
/// (if any), then down the vendor's chain.
pub fn candidates(drivers: &[HidDriver], chains: &[Chain], vid: u16, pid: u16) -> Vec<usize> {
    let exact = |d: &HidDriver| {
        d.matches
            .iter()
            .any(|m| m.vid == vid && m.pids.contains(&pid))
    };
    if let Some(i) = drivers.iter().position(exact) {
        return vec![i];
    }
    let mut out: Vec<usize> = pick(drivers, vid, pid).into_iter().collect();
    let chained = chains
        .iter()
        .filter(|c| c.vid == vid)
        .flat_map(|c| c.drivers);
    for i in chained.filter_map(|n| drivers.iter().position(|d| d.name == *n)) {
        if !out.contains(&i) {
            out.push(i);
        }
    }
    out
}

/// What one round of driver jobs returned, per driver index: its devices, or None when it did not
/// return in time (or could not start).
pub type Round = Vec<(usize, Option<Vec<Device>>)>;

/// Hands each HID collection to the first of its `candidates`, then, round by round, the collections
/// of a driver that reported no device at all to their next candidate. A driver's collections are
/// settled once it reports anything (as the fallback rule always was: a product counts as claimed
/// once its driver reports something), so no product is read by two drivers. A driver that missed
/// the deadline is still running on its handles and its collections are left alone.
///
/// Returns the devices per driver index and the collections nobody reported (for the fallback).
pub fn route<T: Clone>(
    items: &[T],
    candidates: impl Fn(&T) -> Vec<usize>,
    mut run: impl FnMut(Vec<(usize, Vec<T>)>) -> Round,
) -> (Vec<(usize, Vec<Device>)>, Vec<T>) {
    let mut pending: Vec<(T, Vec<usize>, usize)> = items
        .iter()
        .map(|t| (t.clone(), candidates(t), 0))
        .collect();
    let mut results = Vec::new();
    let mut unclaimed = Vec::new();
    loop {
        let mut jobs: Vec<(usize, Vec<T>)> = Vec::new();
        let mut asked = Vec::new();
        for (t, cands, n) in pending.drain(..) {
            let Some(&i) = cands.get(n) else {
                unclaimed.push(t);
                continue;
            };
            match jobs.iter_mut().find(|(j, _)| *j == i) {
                Some((_, ts)) => ts.push(t.clone()),
                None => jobs.push((i, vec![t.clone()])),
            }
            asked.push((t, cands, n));
        }
        if jobs.is_empty() {
            return (results, unclaimed);
        }
        let got = run(jobs);
        for (t, cands, n) in asked {
            let reported = got.iter().find(|(i, _)| *i == cands[n]).map(|(_, d)| d);
            if let Some(Some(d)) = reported
                && d.is_empty()
            {
                pending.push((t, cands, n + 1));
            }
        }
        results.extend(got.into_iter().filter_map(|(i, d)| Some((i, d?))));
    }
}

/// Runs every HID driver and source in parallel and returns their devices in registry order.
pub fn poll(
    api: Arc<HidApi>,
    drivers: &'static [HidDriver],
    chains: &[Chain],
    fallback: &'static HidDriver,
    sources: &'static [Source],
    timeout: Duration,
) -> Vec<Device> {
    let infos: Vec<DeviceInfo> = api.device_list().cloned().collect();
    let cands =
        |info: &DeviceInfo| candidates(drivers, chains, info.vendor_id(), info.product_id());

    // Sources need no HID access: they run alongside every driver round.
    let (stx, srx) = mpsc::channel::<(usize, Vec<Device>)>();
    let first_source = drivers.len() + 1;
    let mut source_jobs = Vec::new();
    for (j, source) in sources.iter().enumerate() {
        let tx = stx.clone();
        if spawn(source.name, move || {
            let _ = tx.send((first_source + j, (source.poll)()));
        }) {
            source_jobs.push((first_source + j, source.name));
        }
    }
    drop(stx);
    let sources_deadline = Instant::now() + timeout;

    // Vendor drivers run in parallel, each round with its own deadline. A late driver keeps running
    // detached (and is skipped by later polls until it returns); its result is discarded when it
    // finally arrives. The generic fallback runs last, on what no vendor driver reported, so an
    // unknown model of a whole-vendor match can still be read through standard HID.
    let (mut results, fallback_infos) = route(&infos, cands, |jobs| {
        let (tx, rx) = mpsc::channel::<(usize, Vec<Device>)>();
        let mut started = Vec::new();
        for (i, infos) in &jobs {
            let (i, driver) = (*i, &drivers[*i]);
            let (tx, api, infos) = (tx.clone(), api.clone(), infos.clone());
            if spawn(driver.name, move || {
                let _ = tx.send((i, (driver.poll)(&api, &infos)));
            }) {
                started.push(i);
            }
        }
        drop(tx);
        let got = collect(&rx, started.len(), Instant::now() + timeout);
        jobs.iter()
            .map(|(i, _)| {
                let d = got.iter().find(|(r, _)| r == i).map(|(_, d)| d.clone());
                if d.is_none() && started.contains(i) {
                    eprintln!(
                        "driver {} missed the {} s deadline",
                        drivers[*i].name,
                        timeout.as_secs()
                    );
                }
                (*i, d)
            })
            .collect()
    });
    if !fallback_infos.is_empty() {
        results.push((drivers.len(), (fallback.poll)(&api, &fallback_infos)));
    }
    let sources_got = collect(&srx, source_jobs.len(), sources_deadline);
    for (i, name) in &source_jobs {
        if !sources_got.iter().any(|(r, _)| r == i) {
            eprintln!("source {name} missed the {} s deadline", timeout.as_secs());
        }
    }
    results.extend(sources_got);
    results.sort_by_key(|(i, _)| *i);

    // Physical devices (Windows container ids) a HID driver reported, so the same device seen
    // again over Bluetooth is listed once. Only worked out when there is a Bluetooth entry.
    let has_bt = results
        .iter()
        .any(|(_, ds)| ds.iter().any(|d| d.key.starts_with("bt:")));
    // Every collection a vendor driver handles counts, even when it returned nothing this poll
    // (asleep): the tracker still shows that device, so its Bluetooth copy must stay hidden.
    let vendor: Vec<String> = if has_bt {
        infos
            .iter()
            .filter(|info| !cands(info).is_empty())
            .filter_map(|info| bluetooth::container_of(info.path()))
            .map(|c| format!("bt:{c}"))
            .collect()
    } else {
        Vec::new()
    };
    // The generic HID reading of a Bluetooth device loses to Windows' own level for it.
    let generic: Vec<(String, String)> = if has_bt {
        fallback_infos
            .iter()
            .filter_map(|info| {
                let c = bluetooth::container_of(info.path())?;
                Some((
                    format!("hid:{:04X}:{:04X}", info.vendor_id(), info.product_id()),
                    format!("bt:{c}"),
                ))
            })
            .collect()
    } else {
        Vec::new()
    };
    dedup(
        results.into_iter().flat_map(|(_, d)| d).collect(),
        &vendor,
        &generic,
    )
}

/// Up to `n` results from `rx`, until `deadline`.
fn collect(
    rx: &mpsc::Receiver<(usize, Vec<Device>)>,
    n: usize,
    deadline: Instant,
) -> Vec<(usize, Vec<Device>)> {
    let mut got = Vec::with_capacity(n);
    while got.len() < n {
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(r) => got.push(r),
            Err(_) => break,
        }
    }
    got
}

/// Drops second readings of one physical device:
/// - a Bluetooth entry whose container a vendor driver already reported (`vendor_bt_keys`);
/// - a generic HID entry when Windows lists the same container over Bluetooth (`generic_to_bt`
///   maps "hid:VID:PID" keys to the "bt:{container}" key of their device);
/// - Xbox / XInput entries when the controller is already listed over Bluetooth.
pub fn dedup(
    devices: Vec<Device>,
    vendor_bt_keys: &[String],
    generic_to_bt: &[(String, String)],
) -> Vec<Device> {
    let bt_keys: Vec<String> = devices
        .iter()
        .filter(|d| d.key.starts_with("bt:"))
        .map(|d| d.key.clone())
        .collect();
    let bt_xbox = devices
        .iter()
        .any(|d| d.key.starts_with("bt:") && d.name.contains("Xbox"));
    // Microsoft (045E) pads read by the Xbox source are not repeated by the generic HID fallback.
    let xbox_listed = devices
        .iter()
        .any(|d| d.key.starts_with("xbox:") || d.key.starts_with("xinput:"));
    devices
        .into_iter()
        .filter(|d| {
            if d.key.starts_with("bt:") {
                return !vendor_bt_keys.contains(&d.key);
            }
            if d.key.starts_with("hid:") {
                if xbox_listed && d.key.starts_with("hid:045E:") {
                    return false;
                }
                return !generic_to_bt.iter().any(|(h, b)| {
                    *h == d.key && bt_keys.contains(b) && !vendor_bt_keys.contains(b)
                });
            }
            !(bt_xbox && (d.key.starts_with("xbox:") || d.key.starts_with("xinput:")))
        })
        .collect()
}

/// Drivers whose previous run has not returned yet. A driver is never run twice at once, so a slow
/// device cannot pile up threads or have two runs writing to the same HID handles.
static RUNNING: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());

fn running() -> std::sync::MutexGuard<'static, Vec<&'static str>> {
    RUNNING.lock().unwrap_or_else(|e| e.into_inner())
}

/// Removes the driver from RUNNING when its thread ends, even by panic.
struct Done(&'static str);

impl Drop for Done {
    fn drop(&mut self) {
        running().retain(|n| *n != self.0);
    }
}

/// Starts a driver thread unless that driver is still running. Returns whether it started.
fn spawn(name: &'static str, f: impl FnOnce() + Send + 'static) -> bool {
    {
        let mut r = running();
        if r.contains(&name) {
            eprintln!("driver {name} still busy from the previous poll; skipped");
            return false;
        }
        r.push(name);
    }
    let started = std::thread::Builder::new()
        .name(format!("driver-{name}"))
        .spawn(move || {
            let _done = Done(name);
            f();
        });
    if let Err(e) = started {
        running().retain(|n| *n != name);
        eprintln!("could not start driver {name}: {e}");
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none(_: &HidApi, _: &[DeviceInfo]) -> Vec<Device> {
        Vec::new()
    }

    fn dev(key: &str, name: &str) -> Device {
        Device::new(key, name, crate::device::Kind::Other, "test")
    }

    #[test]
    fn bluetooth_copy_of_a_vendor_device_is_dropped() {
        let out = dedup(
            vec![
                dev("logi:unit:1", "MX Master 3S"),
                dev("bt:{A}", "MX Master 3S"),
                dev("bt:{B}", "Buds"),
            ],
            &["bt:{A}".into()],
            &[],
        );
        let keys: Vec<&str> = out.iter().map(|d| d.key.as_str()).collect();
        assert_eq!(keys, ["logi:unit:1", "bt:{B}"]);
    }

    #[test]
    fn windows_bluetooth_level_beats_generic_hid() {
        let out = dedup(
            vec![dev("hid:046D:B023", "Keyboard"), dev("bt:{K}", "Keyboard")],
            &[],
            &[("hid:046D:B023".into(), "bt:{K}".into())],
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].key, "bt:{K}");
    }

    #[test]
    fn xbox_listed_over_bluetooth_is_not_repeated() {
        let out = dedup(
            vec![
                dev("bt:{X}", "Xbox Wireless Controller"),
                dev("xbox:0B13:0", "Xbox Wireless Controller"),
            ],
            &[],
            &[],
        );
        assert_eq!(out.len(), 1);
        let out = dedup(vec![dev("xinput:0", "Controller 1")], &[], &[]);
        assert_eq!(out.len(), 1, "no Bluetooth Xbox: keep XInput");
    }

    #[test]
    fn generic_hid_does_not_repeat_an_xbox_pad() {
        let out = dedup(
            vec![
                dev("hid:045E:0B13", "Controller"),
                dev("xbox:0B13:0", "Xbox Wireless Controller"),
                dev("hid:1234:0001", "Keyboard"),
            ],
            &[],
            &[],
        );
        let keys: Vec<&str> = out.iter().map(|d| d.key.as_str()).collect();
        assert_eq!(keys, ["xbox:0B13:0", "hid:1234:0001"]);
    }

    static TWO: &[HidDriver] = &[
        HidDriver {
            name: "vendor-wide",
            matches: &[Match {
                vid: 0x3554,
                pids: &[],
            }],
            poll: none,
        },
        HidDriver {
            name: "exact",
            matches: &[Match {
                vid: 0x3554,
                pids: &[0xF512],
            }],
            poll: none,
        },
    ];

    #[test]
    fn busy_driver_is_not_started_twice() {
        let (release_tx, release_rx) = mpsc::channel::<()>();
        assert!(spawn("test-busy", move || {
            let _ = release_rx.recv();
        }));
        assert!(!spawn("test-busy", || {}));
        release_tx.send(()).unwrap();
        let start = Instant::now();
        while running().contains(&"test-busy") && start.elapsed() < Duration::from_secs(2) {
            std::thread::yield_now();
        }
        assert!(spawn("test-busy", || {}));
    }

    #[test]
    fn exact_product_beats_whole_vendor_regardless_of_order() {
        assert_eq!(pick(TWO, 0x3554, 0xF512), Some(1));
        assert_eq!(pick(TWO, 0x3554, 0x0001), Some(0));
        assert_eq!(pick(TWO, 0x1234, 0xF512), None);
    }

    #[test]
    fn headset_dongles_beat_the_logitech_vendor_match() {
        let name = |vid, pid| pick(HID_DRIVERS, vid, pid).map(|i| HID_DRIVERS[i].name);
        assert_eq!(name(0x046D, 0x0ADF), Some("logitech-g435-0adf"));
        assert_eq!(name(0x046D, 0x0B0B), Some("astro"));
        assert_eq!(name(0x046D, 0xC54D), Some("logitech"));
        assert_eq!(name(0x0B0E, 0x2E51), Some("jabra"));
    }

    // ---- Chains ----

    static CHAINED: &[HidDriver] = &[
        HidDriver {
            name: "exact",
            matches: &[Match {
                vid: 0x33E4,
                pids: &[0x3519],
            }],
            poll: none,
        },
        HidDriver {
            name: "second",
            matches: &[],
            poll: none,
        },
        HidDriver {
            name: "first",
            matches: &[],
            poll: none,
        },
        HidDriver {
            name: "vendor-wide",
            matches: &[Match {
                vid: 0x3554,
                pids: &[],
            }],
            poll: none,
        },
    ];
    static TEST_CHAINS: &[Chain] = &[
        Chain {
            vid: 0x33E4,
            drivers: &["first", "second"],
        },
        Chain {
            vid: 0x3554,
            drivers: &["second", "vendor-wide"],
        },
    ];

    #[test]
    fn chain_order_comes_from_the_chain_not_the_registry() {
        let c = |vid, pid| candidates(CHAINED, TEST_CHAINS, vid, pid);
        assert_eq!(c(0x33E4, 0x5408), [2, 1]);
        // An exact claim is a known product of that driver: no other protocol is tried on it.
        assert_eq!(c(0x33E4, 0x3519), [0]);
        // The whole-vendor driver comes first, and is not asked twice.
        assert_eq!(c(0x3554, 0x0001), [3, 1]);
        assert!(c(0x1234, 0x0001).is_empty());
    }

    fn mouse(key: &str) -> Device {
        dev(key, "Mouse")
    }

    /// Runs `route` over products with fake drivers: `answer(driver, products)` is what a driver
    /// returns for the products handed to it. Returns (devices, unclaimed, every job run).
    #[allow(clippy::type_complexity)]
    fn route_with(
        products: &[(u16, u16)],
        cands: impl Fn(&(u16, u16)) -> Vec<usize>,
        answer: impl Fn(usize, &[(u16, u16)]) -> Option<Vec<Device>>,
    ) -> (
        Vec<String>,
        Vec<(u16, u16)>,
        Vec<Vec<(usize, Vec<(u16, u16)>)>>,
    ) {
        let mut rounds = Vec::new();
        let (got, left) = route(products, cands, |jobs| {
            rounds.push(jobs.clone());
            jobs.iter().map(|(i, ps)| (*i, answer(*i, ps))).collect()
        });
        let keys = got
            .into_iter()
            .flat_map(|(_, ds)| ds)
            .map(|d| d.key)
            .collect();
        (keys, left, rounds)
    }

    const G: (u16, u16) = (0x33E4, 0x5408);
    const K: (u16, u16) = (0x33E4, 0x7777);

    #[test]
    fn first_driver_that_reports_wins() {
        let cands = |p: &(u16, u16)| candidates(CHAINED, TEST_CHAINS, p.0, p.1);
        // "first" (2) reads G: "second" (1) is never asked.
        let (keys, left, rounds) = route_with(&[G], cands, |i, _| match i {
            2 => Some(vec![mouse("first:G")]),
            _ => Some(vec![mouse("second:G")]),
        });
        assert_eq!(
            (keys, left.len(), rounds.len()),
            (vec!["first:G".into()], 0, 1)
        );
        // "first" does not speak it: the product goes down the chain, alone.
        let (keys, left, rounds) = route_with(&[K], cands, |i, _| match i {
            2 => Some(vec![]),
            _ => Some(vec![mouse("second:K")]),
        });
        assert_eq!((keys, left.len()), (vec!["second:K".to_string()], 0));
        assert_eq!(rounds, [vec![(2, vec![K])], vec![(1, vec![K])]]);
    }

    #[test]
    fn product_nobody_reports_goes_to_the_fallback() {
        let cands = |p: &(u16, u16)| candidates(CHAINED, TEST_CHAINS, p.0, p.1);
        let unknown = (0x1234, 0x0001);
        let (keys, left, rounds) = route_with(&[K, unknown], cands, |_, _| Some(vec![]));
        assert!(keys.is_empty());
        assert_eq!(left, [unknown, K]);
        assert_eq!(
            rounds.len(),
            2,
            "first, then second; never the fallback twice"
        );
    }

    #[test]
    fn a_late_driver_keeps_its_products() {
        let cands = |p: &(u16, u16)| candidates(CHAINED, TEST_CHAINS, p.0, p.1);
        // "first" missed the deadline (or was still busy): still running on its handles, so its
        // products go neither down the chain nor to the fallback.
        let (keys, left, rounds) = route_with(&[K], cands, |i, _| match i {
            2 => None,
            _ => Some(vec![mouse("second:K")]),
        });
        assert!(keys.is_empty() && left.is_empty());
        assert_eq!(rounds.len(), 1);
    }

    #[test]
    fn a_product_is_read_by_one_driver_only() {
        let cands = |p: &(u16, u16)| candidates(CHAINED, TEST_CHAINS, p.0, p.1);
        let exact = (0x33E4, 0x3519);
        // Everyone answers everything: each product still ends up with exactly one reading, from
        // its first candidate, and a driver's products move on only when it reported nothing.
        let (keys, left, rounds) = route_with(&[G, exact, K], cands, |i, ps| {
            Some(
                ps.iter()
                    .map(|p| mouse(&format!("{i}:{:04X}", p.1)))
                    .collect(),
            )
        });
        assert_eq!(keys, ["2:5408", "2:7777", "0:3519"]);
        assert!(left.is_empty());
        assert_eq!(rounds.len(), 1);
    }

    #[test]
    fn real_chains_name_registered_drivers_once() {
        for c in CHAINS {
            for (n, name) in c.drivers.iter().enumerate() {
                assert!(
                    HID_DRIVERS.iter().any(|d| d.name == *name),
                    "chain {:04X}: no driver {name}",
                    c.vid
                );
                assert!(!c.drivers[..n].contains(name), "{name} twice in a chain");
            }
        }
        let names = |vid, pid| -> Vec<&str> {
            candidates(HID_DRIVERS, CHAINS, vid, pid)
                .into_iter()
                .map(|i| HID_DRIVERS[i].name)
                .collect()
        };
        // G-Wolves and Hitscan: Glorious' protocol first, then Kysona's (BatteryHub's order).
        assert_eq!(names(0x33E4, 0x5408), ["glorious", "kysona"]);
        assert_eq!(names(0x3770, 0x0300), ["glorious", "kysona"]);
        // Kysona-protocol mice outside the listed ids (Kysona M600, VXE X3 Pro and the like).
        assert_eq!(names(0x3554, 0xF5F0), ["kysona"]);
        // Products claimed exactly stay with their driver.
        assert_eq!(names(0x33E4, 0x3519), ["kysona"]);
        assert_eq!(names(0x3770, 0x0136), ["glorious"]);
        assert_eq!(names(0x3554, 0xF512), ["chimera"]);
        assert_eq!(names(0x3554, 0xFA09), ["aula"]);
    }

    /// The first candidate of a product is unambiguous: no product is claimed exactly by two
    /// drivers. Chains add later candidates on purpose, in their own order, so they are not claims.
    #[test]
    fn registry_has_no_duplicate_exact_claims() {
        let mut seen = Vec::new();
        for d in HID_DRIVERS {
            for m in d.matches {
                for p in m.pids {
                    assert!(
                        !seen.contains(&(m.vid, *p)),
                        "{:04X}:{p:04X} claimed twice",
                        m.vid
                    );
                    seen.push((m.vid, *p));
                }
            }
        }
    }

    /// Two whole-vendor drivers on one vid would leave the winner to registry order, the fragile
    /// ordering this design exists to avoid; a vendor whose products need several drivers tried
    /// declares that order in `CHAINS` instead.
    #[test]
    fn registry_has_no_duplicate_whole_vendor_claims() {
        let mut seen = Vec::new();
        for d in HID_DRIVERS {
            for m in d.matches.iter().filter(|m| m.pids.is_empty()) {
                assert!(
                    !seen.contains(&m.vid),
                    "{:04X} claimed whole by two drivers ({})",
                    m.vid,
                    d.name
                );
                seen.push(m.vid);
            }
        }
    }
}

//! Declarative driver registry and the parallel poller.
//!
//! Each HID driver declares which devices it handles. A device goes to the most specific match: an
//! exact vendor + product id beats a whole-vendor match, so drivers that share a vendor id need no
//! hand-kept ordering. Drivers then run in parallel, each with a deadline, so one slow or stuck
//! device cannot hold up the others.

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

/// Runs every HID driver and source in parallel and returns their devices in registry order.
pub fn poll(
    api: Arc<HidApi>,
    drivers: &'static [HidDriver],
    fallback: &'static HidDriver,
    sources: &'static [Source],
    timeout: Duration,
) -> Vec<Device> {
    // Index drivers.len() is the fallback, which gets everything no vendor driver matched (and,
    // after the vendor drivers ran, the products they did not recognise).
    let mut assigned: Vec<Vec<DeviceInfo>> = vec![Vec::new(); drivers.len() + 1];
    for info in api.device_list() {
        let i = pick(drivers, info.vendor_id(), info.product_id()).unwrap_or(drivers.len());
        assigned[i].push(info.clone());
    }

    let (tx, rx) = mpsc::channel::<(usize, Vec<Device>)>();
    let mut jobs: Vec<(usize, &'static str)> = Vec::new();
    // Vendor drivers run in parallel now; the fallback runs after them (below), because it also
    // gets the products of vendor drivers that did not recognise them.
    for (i, (driver, infos)) in drivers.iter().zip(&assigned).enumerate() {
        if infos.is_empty() {
            continue;
        }
        let (tx, api, infos) = (tx.clone(), api.clone(), infos.clone());
        if spawn(driver.name, move || {
            let _ = tx.send((i, (driver.poll)(&api, &infos)));
        }) {
            jobs.push((i, driver.name));
        }
    }
    let first_source = drivers.len() + 1;
    for (j, source) in sources.iter().enumerate() {
        let tx = tx.clone();
        if spawn(source.name, move || {
            let _ = tx.send((first_source + j, (source.poll)()));
        }) {
            jobs.push((first_source + j, source.name));
        }
    }
    drop(tx);

    // A late driver keeps running detached (and is skipped by later polls until it returns); its
    // result is discarded when it finally arrives.
    let deadline = Instant::now() + timeout;
    let mut results = Vec::with_capacity(jobs.len());
    while results.len() < jobs.len() {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(r) => results.push(r),
            Err(_) => break,
        }
    }
    for (i, name) in &jobs {
        if !results.iter().any(|(r, _)| r == i) {
            eprintln!("driver {name} missed the {} s deadline", timeout.as_secs());
        }
    }
    // A product counts as claimed only once its driver reports something (BatteryHub's rule): the
    // products of a vendor driver that answered with no device also go to the generic fallback,
    // so an unknown model of a whole-vendor match can still be read through standard HID.
    // A driver that missed the deadline is still running on its handles and is left alone.
    let mut fallback_infos = assigned[drivers.len()].clone();
    for (i, devices) in &results {
        if *i < drivers.len() && devices.is_empty() {
            fallback_infos.extend(assigned[*i].iter().cloned());
        }
    }
    if !fallback_infos.is_empty() {
        results.push((drivers.len(), (fallback.poll)(&api, &fallback_infos)));
    }
    results.sort_by_key(|(i, _)| *i);

    // Physical devices (Windows container ids) a HID driver reported, so the same device seen
    // again over Bluetooth is listed once. Only worked out when there is a Bluetooth entry.
    let has_bt = results
        .iter()
        .any(|(_, ds)| ds.iter().any(|d| d.key.starts_with("bt:")));
    let containers = |pred: &dyn Fn(usize) -> bool| -> Vec<String> {
        if !has_bt {
            return Vec::new();
        }
        // Every collection a vendor driver handles counts, even when it returned nothing this poll
        // (asleep): the tracker still shows that device, so its Bluetooth copy must stay hidden.
        (0..assigned.len())
            .filter(|i| pred(*i))
            .flat_map(|i| &assigned[i])
            .filter_map(|info| bluetooth::container_of(info.path()))
            .map(|c| format!("bt:{c}"))
            .collect()
    };
    let vendor = containers(&|i| i < drivers.len());
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
    /// ordering this design exists to avoid.
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

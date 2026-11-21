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
use crate::drivers::{bluetooth, generic_hid, logitech, phonelink, xbox};

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

pub static HID_DRIVERS: &[HidDriver] = &[HidDriver {
    name: "logitech",
    matches: &[Match {
        vid: logitech::VID,
        pids: &[],
    }],
    poll: logitech::poll,
}];

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
    // Index drivers.len() is the fallback, which gets everything no vendor driver matched.
    let mut assigned: Vec<Vec<DeviceInfo>> = vec![Vec::new(); drivers.len() + 1];
    for info in api.device_list() {
        let i = pick(drivers, info.vendor_id(), info.product_id()).unwrap_or(drivers.len());
        assigned[i].push(info.clone());
    }

    let (tx, rx) = mpsc::channel::<(usize, Vec<Device>)>();
    let mut jobs: Vec<(usize, &'static str)> = Vec::new();
    for (i, infos) in assigned.iter().enumerate() {
        if infos.is_empty() {
            continue;
        }
        let driver = drivers.get(i).unwrap_or(fallback);
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
        assigned[drivers.len()]
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
}

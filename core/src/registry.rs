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
use crate::{logitech, phonelink};

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

pub static SOURCES: &[Source] = &[Source {
    name: "phonelink",
    poll: || phonelink::poll().into_iter().collect(),
}];

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
    sources: &'static [Source],
    timeout: Duration,
) -> Vec<Device> {
    let mut assigned: Vec<Vec<DeviceInfo>> = vec![Vec::new(); drivers.len()];
    for info in api.device_list() {
        if let Some(i) = pick(drivers, info.vendor_id(), info.product_id()) {
            assigned[i].push(info.clone());
        }
    }

    let (tx, rx) = mpsc::channel::<(usize, Vec<Device>)>();
    let mut jobs: Vec<(usize, &'static str)> = Vec::new();
    for (i, (driver, infos)) in drivers.iter().zip(assigned).enumerate() {
        if infos.is_empty() {
            continue;
        }
        let (tx, api) = (tx.clone(), api.clone());
        if spawn(driver.name, move || {
            let _ = tx.send((i, (driver.poll)(&api, &infos)));
        }) {
            jobs.push((i, driver.name));
        }
    }
    for (j, source) in sources.iter().enumerate() {
        let tx = tx.clone();
        if spawn(source.name, move || {
            let _ = tx.send((drivers.len() + j, (source.poll)()));
        }) {
            jobs.push((drivers.len() + j, source.name));
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
    results.into_iter().flat_map(|(_, d)| d).collect()
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

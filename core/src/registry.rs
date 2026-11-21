//! Declarative driver registry and the parallel poller.
//!
//! Each HID driver declares which devices it handles. A device goes to the most specific match: an
//! exact vendor + product id beats a whole-vendor match, so drivers that share a vendor id need no
//! hand-kept ordering. Drivers then run in parallel, each with a deadline, so one slow or stuck
//! device cannot hold up the others.

use std::sync::{Arc, mpsc};
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
    let mut jobs = 0;
    for (i, (driver, infos)) in drivers.iter().zip(assigned).enumerate() {
        if infos.is_empty() {
            continue;
        }
        let (tx, api) = (tx.clone(), api.clone());
        spawn(driver.name, move || {
            let _ = tx.send((i, (driver.poll)(&api, &infos)));
        });
        jobs += 1;
    }
    for (j, source) in sources.iter().enumerate() {
        let tx = tx.clone();
        spawn(source.name, move || {
            let _ = tx.send((drivers.len() + j, (source.poll)()));
        });
        jobs += 1;
    }
    drop(tx);

    // A late driver keeps running detached; its result is discarded when it finally arrives.
    let deadline = Instant::now() + timeout;
    let mut results = Vec::with_capacity(jobs);
    while results.len() < jobs {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(r) => results.push(r),
            Err(_) => break,
        }
    }
    results.sort_by_key(|(i, _)| *i);
    results.into_iter().flat_map(|(_, d)| d).collect()
}

fn spawn(name: &str, f: impl FnOnce() + Send + 'static) {
    if let Err(e) = std::thread::Builder::new()
        .name(format!("driver-{name}"))
        .spawn(f)
    {
        eprintln!("could not start driver {name}: {e}");
    }
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

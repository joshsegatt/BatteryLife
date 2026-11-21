//! Keeps a device's last known level visible while it sleeps.
//!
//! Wireless devices often stop answering while asleep (a Logitech receiver drops them from the link).
//! Instead of making them vanish, the last reading stays for the device's grace period, marked asleep.

use std::collections::HashMap;
use std::time::Instant;

use crate::device::Device;

#[derive(Default)]
pub struct Tracker {
    /// Last reading with a level, and when it was taken.
    last: HashMap<String, (Device, Instant)>,
    /// Keys in the order they were first seen, so a device keeps its row when it sleeps or wakes.
    order: Vec<String>,
}

impl Tracker {
    /// Merges a fresh poll with remembered readings, in the order devices were first seen.
    pub fn update(&mut self, fresh: Vec<Device>, now: Instant) -> Vec<Device> {
        let mut out = Vec::with_capacity(fresh.len());
        for mut d in fresh {
            match (d.percent, self.last.get(&d.key)) {
                (Some(_), _) if !d.asleep => {
                    self.last.insert(d.key.clone(), (d.clone(), now));
                }
                // Present but without a level (or explicitly asleep): show the last level if still fresh.
                (None, Some((prev, at))) if now.duration_since(*at) <= prev.grace => {
                    d.percent = prev.percent;
                    d.asleep = true;
                }
                _ => {}
            }
            out.push(d);
        }
        self.last.retain(|key, (prev, at)| {
            if out.iter().any(|d| d.key == *key) {
                return true;
            }
            if now.duration_since(*at) > prev.grace {
                return false;
            }
            let mut d = prev.clone();
            d.asleep = true;
            d.charging = false;
            d.full = false;
            out.push(d);
            true
        });
        for d in &out {
            if !self.order.contains(&d.key) {
                self.order.push(d.key.clone());
            }
        }
        // Forget keys that are neither shown nor remembered, so the list cannot grow without bound.
        self.order
            .retain(|k| out.iter().any(|d| d.key == *k) || self.last.contains_key(k));
        let pos = |k: &str| self.order.iter().position(|o| o == k).unwrap_or(usize::MAX);
        out.sort_by_key(|d| pos(&d.key));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::Kind;
    use std::time::Duration;

    fn mouse(pct: Option<u8>) -> Device {
        let mut d = Device::new("m", "Mouse", Kind::Mouse, "receiver");
        d.percent = pct;
        d.grace = Duration::from_secs(1800);
        d
    }

    #[test]
    fn missing_device_stays_asleep_within_grace() {
        let mut t = Tracker::default();
        let t0 = Instant::now();
        t.update(vec![mouse(Some(80))], t0);
        let out = t.update(vec![], t0 + Duration::from_secs(600));
        assert_eq!(out.len(), 1);
        assert_eq!((out[0].percent, out[0].asleep), (Some(80), true));
    }

    #[test]
    fn missing_device_drops_after_grace() {
        let mut t = Tracker::default();
        let t0 = Instant::now();
        t.update(vec![mouse(Some(80))], t0);
        assert!(t.update(vec![], t0 + Duration::from_secs(1801)).is_empty());
        // and stays gone
        assert!(t.update(vec![], t0 + Duration::from_secs(1802)).is_empty());
    }

    #[test]
    fn device_without_level_keeps_last_level() {
        let mut t = Tracker::default();
        let t0 = Instant::now();
        t.update(vec![mouse(Some(55))], t0);
        let out = t.update(vec![mouse(None)], t0 + Duration::from_secs(60));
        assert_eq!((out[0].percent, out[0].asleep), (Some(55), true));
    }

    #[test]
    fn fresh_reading_replaces_old_one() {
        let mut t = Tracker::default();
        let t0 = Instant::now();
        t.update(vec![mouse(Some(55))], t0);
        let out = t.update(vec![mouse(Some(54))], t0 + Duration::from_secs(60));
        assert_eq!((out[0].percent, out[0].asleep), (Some(54), false));
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn sleeping_device_keeps_its_row() {
        let mut t = Tracker::default();
        let t0 = Instant::now();
        let mut phone = Device::new("p", "Phone", Kind::Phone, "Phone Link");
        phone.percent = Some(40);
        t.update(vec![mouse(Some(80)), phone.clone()], t0);
        let out = t.update(vec![phone], t0 + Duration::from_secs(60));
        let keys: Vec<&str> = out.iter().map(|d| d.key.as_str()).collect();
        assert_eq!(keys, ["m", "p"]);
        assert!(out[0].asleep);
    }

    #[test]
    fn zero_grace_is_not_remembered() {
        let mut t = Tracker::default();
        let t0 = Instant::now();
        let mut phone = Device::new("p", "Phone", Kind::Phone, "Phone Link");
        phone.percent = Some(40);
        t.update(vec![phone], t0);
        assert!(t.update(vec![], t0 + Duration::from_secs(1)).is_empty());
    }
}

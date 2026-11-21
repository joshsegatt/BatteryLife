//! Keeps a device's last known level visible while it sleeps.
//!
//! Wireless devices often stop answering while asleep (a Logitech receiver drops them from the link).
//! Instead of making them vanish, the last reading stays for the device's grace period, marked asleep.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::device::Device;

#[derive(Default)]
pub struct Tracker {
    /// Last reading with a level, and when it was taken.
    last: HashMap<String, (Device, Instant)>,
    /// Keys in the order they were first seen, so a device keeps its row when it sleeps or wakes.
    order: Vec<String>,
    /// Discharging readings per device, for the time-left estimate.
    samples: HashMap<String, Vec<(Instant, u8)>>,
}

/// Drain is measured over at most this much recent history.
const WINDOW: Duration = Duration::from_secs(6 * 60 * 60);
/// No estimate until the history spans this long and the level dropped this much: a single
/// one-point step on a fresh start would otherwise predict nonsense.
const MIN_SPAN_MIN: f64 = 20.0;
const MIN_DROP: f64 = 2.0;

/// Records a fresh reading and returns the estimated minutes until empty.
fn estimate(samples: &mut Vec<(Instant, u8)>, d: &Device, now: Instant) -> Option<u32> {
    let pct = d.percent?;
    // Charging, or a level that went up (charged in between): start a new discharge run.
    if d.charging || samples.last().is_some_and(|(_, p)| pct > *p) {
        samples.clear();
    }
    if d.charging {
        return None;
    }
    samples.push((now, pct));
    samples.retain(|(t, _)| now.duration_since(*t) <= WINDOW);
    let (t0, p0) = *samples.first()?;
    let span = now.duration_since(t0).as_secs_f64() / 60.0;
    let drop = f64::from(p0.saturating_sub(pct));
    if span < MIN_SPAN_MIN || drop < MIN_DROP {
        return None;
    }
    Some((f64::from(pct) * span / drop).round() as u32)
}

impl Tracker {
    /// Merges a fresh poll with remembered readings, in the order devices were first seen.
    pub fn update(&mut self, fresh: Vec<Device>, now: Instant) -> Vec<Device> {
        let mut out = Vec::with_capacity(fresh.len());
        for mut d in fresh {
            match (d.percent, self.last.get(&d.key)) {
                (Some(_), _) if !d.asleep => {
                    let samples = self.samples.entry(d.key.clone()).or_default();
                    d.minutes_left = estimate(samples, &d, now);
                    self.last.insert(d.key.clone(), (d.clone(), now));
                }
                // Present but without a level (or explicitly asleep): show the last level if still fresh.
                (None, Some((prev, at))) if now.duration_since(*at) <= prev.grace => {
                    d.percent = prev.percent;
                    d.asleep = true;
                    d.minutes_left = None;
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
            d.minutes_left = None;
            out.push(d);
            true
        });
        self.samples.retain(|k, _| self.last.contains_key(k));
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

    fn at(t0: Instant, min: u64) -> Instant {
        t0 + Duration::from_secs(min * 60)
    }

    #[test]
    fn estimates_time_left_from_steady_drain() {
        let mut t = Tracker::default();
        let t0 = Instant::now();
        // 1 point every 6 minutes: 10 points per hour.
        let mut last = Vec::new();
        for (i, pct) in (70..=80).rev().enumerate() {
            last = t.update(vec![mouse(Some(pct))], at(t0, i as u64 * 6));
        }
        // 70% left at 10 points/hour = 7 h.
        assert_eq!(last[0].minutes_left, Some(420));
    }

    #[test]
    fn no_estimate_from_short_or_flat_history() {
        let mut t = Tracker::default();
        let t0 = Instant::now();
        t.update(vec![mouse(Some(80))], t0);
        let out = t.update(vec![mouse(Some(79))], at(t0, 10));
        assert_eq!(out[0].minutes_left, None, "10 minutes is too short");
        let out = t.update(vec![mouse(Some(79))], at(t0, 30));
        assert_eq!(out[0].minutes_left, None, "a 1-point drop is too small");
    }

    #[test]
    fn charging_resets_the_estimate() {
        let mut t = Tracker::default();
        let t0 = Instant::now();
        t.update(vec![mouse(Some(80))], t0);
        let mut c = mouse(Some(70));
        c.charging = true;
        assert_eq!(t.update(vec![c], at(t0, 60))[0].minutes_left, None);
        // After charging, a new run needs its own 20 minutes.
        assert_eq!(
            t.update(vec![mouse(Some(90))], at(t0, 70))[0].minutes_left,
            None
        );
        assert_eq!(
            t.update(vec![mouse(Some(88))], at(t0, 80))[0].minutes_left,
            None
        );
        assert_eq!(
            t.update(vec![mouse(Some(86))], at(t0, 100))[0].minutes_left,
            Some(645)
        );
    }

    #[test]
    fn sleeping_device_has_no_estimate() {
        let mut t = Tracker::default();
        let t0 = Instant::now();
        t.update(vec![mouse(Some(80))], t0);
        t.update(vec![mouse(Some(76))], at(t0, 30));
        let out = t.update(vec![], at(t0, 40));
        assert!(out[0].asleep);
        assert_eq!(out[0].minutes_left, None);
    }

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

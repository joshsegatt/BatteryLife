//! Logitech HID++ (VID 046D): Unifying / Bolt / LIGHTSPEED receivers, wired or Bluetooth devices.
//!
//! Message: [report][device index][feature index][function << 4 | sw id][params...]
//!   report 0x10 = short (7 bytes), 0x11 = long (20 bytes)
//!   HID++ 2.0 error: [.., 0xFF, feature, fn|sw, code]   HID++ 1.0 error: [.., 0x8F, sub id, address, code]
//! Battery features: 0x1004 unified battery, 0x1000 battery level, 0x1001 battery voltage.
//! HID++ 1.0 devices: registers 0x0D and 0x07. Only read-only queries are ever sent.
//!
//! Ported from BatteryHub (src/drivers/logitech.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0, which credits the Logitech
//! HID++ 2.0 drafts, Solaar and HeadsetControl. Headset voltage tables are not ported yet.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use hidapi::{DeviceInfo, HidApi, HidDevice};

use crate::device::{Device, Kind, guess_kind, interpolate};
use crate::drivers::hid_util::physical_key;

pub const VID: u16 = 0x046D;
/// Our software id; replies to other apps (G HUB, Options+) carry theirs and are ignored.
const SW_ID: u8 = 0x0D;
const PING_MS: u64 = 400;
const REPLY_MS: u64 = 500;

#[derive(Debug, PartialEq)]
pub enum Reply {
    Ok(Vec<u8>),
    Err(u8),
    NoAnswer,
}

/// One HID++ connection (a receiver, cable or Bluetooth link). Abstract so the protocol is testable.
pub trait Link {
    fn request(
        &mut self,
        idx: u8,
        b2: u8,
        b3: u8,
        params: &[u8],
        force_short: bool,
        timeout: Duration,
    ) -> Reply;

    /// Pings several device indices. Links that can pipeline (send all, then collect) override this so
    /// a receiver with five empty slots costs one timeout instead of five.
    fn ping_many(&mut self, indices: &[u8], timeout: Duration) -> Vec<(u8, Reply)> {
        indices
            .iter()
            .map(|&i| {
                (
                    i,
                    self.request(i, 0x00, 0x10 | SW_ID, &PING, false, timeout),
                )
            })
            .collect()
    }
}

/// HID++ 2.0 ping (root feature, getProtocolVersion) with an arbitrary marker byte.
const PING: [u8; 3] = [0, 0, 0x5A];

/// Classifies a reply to one HID++ message: Some(reply) when it answers (idx, b2, b3).
fn match_reply(r: &[u8], idx: u8, b2: u8, b3: u8) -> Option<Reply> {
    if r.len() < 7 || !(r[0] == 0x10 || r[0] == 0x11) || r[1] != idx {
        return None;
    }
    if r[2] == b2 && r[3] == b3 {
        return Some(Reply::Ok(r[4..].to_vec()));
    }
    if (r[2] == 0xFF || r[2] == 0x8F) && r[3] == b2 && r[4] == b3 {
        return Some(Reply::Err(r[5]));
    }
    None
}

fn call(l: &mut dyn Link, idx: u8, feature: u8, func: u8, params: &[u8]) -> Option<Vec<u8>> {
    match l.request(
        idx,
        feature,
        (func << 4) | SW_ID,
        params,
        false,
        Duration::from_millis(REPLY_MS),
    ) {
        Reply::Ok(p) => Some(p),
        _ => None,
    }
}

fn feature_index(l: &mut dyn Link, idx: u8, id: u16) -> Option<u8> {
    let p = call(l, idx, 0x00, 0, &[(id >> 8) as u8, id as u8])?;
    p.first().copied().filter(|f| *f != 0)
}

fn li_ion_percent(mv: i32) -> u8 {
    const V: [i32; 13] = [
        4186, 4067, 3989, 3922, 3859, 3811, 3778, 3751, 3717, 3671, 3646, 3579, 3500,
    ];
    const P: [i32; 13] = [100, 90, 80, 70, 60, 50, 40, 30, 20, 10, 5, 2, 0];
    interpolate(mv, &V, &P)
}

fn kind_from_type(t: u8) -> Kind {
    match t {
        0x00 | 0x02 => Kind::Keyboard,
        0x03..=0x05 => Kind::Mouse,
        0x08 => Kind::Headset,
        0x0B | 0x0C => Kind::Gamepad,
        _ => Kind::Other,
    }
}

/// Fills the battery fields from whichever battery feature the device has. False if none.
fn read_battery20(l: &mut dyn Link, idx: u8, d: &mut Device) -> bool {
    if let Some(f) = feature_index(l, idx, 0x1004) {
        // UNIFIED_BATTERY get_status: [state of charge %][level flags][charging status]
        if let Some(p) = call(l, idx, f, 1, &[]).filter(|p| p.len() >= 3) {
            let (soc, level, st) = (p[0], p[1], p[2]);
            if (1..=100).contains(&soc) {
                d.percent = Some(soc);
            } else if level != 0 {
                d.coarse = true;
                d.percent = Some(if level & 8 != 0 {
                    100
                } else if level & 4 != 0 {
                    60
                } else if level & 2 != 0 {
                    20
                } else {
                    5
                });
            }
            d.charging = matches!(st, 1..=3);
            d.full = st == 3;
            return true;
        }
    }
    if let Some(f) = feature_index(l, idx, 0x1000) {
        // BATTERY_STATUS GetBatteryLevelStatus: [level %][next level][status]
        if let Some(p) = call(l, idx, f, 0, &[]).filter(|p| p.len() >= 3) {
            let st = p[2];
            d.charging = (1..=4).contains(&st);
            d.full = st == 3;
            d.percent = if p[0] == 0 && st == 3 {
                Some(100)
            } else {
                Some(p[0]).filter(|v| *v <= 100)
            };
            return true;
        }
    }
    if let Some(f) = feature_index(l, idx, 0x1001) {
        // BATTERY_VOLTAGE GetBatteryInfo: [mV hi][mV lo][flags]
        if let Some(p) = call(l, idx, f, 0, &[]).filter(|p| p.len() >= 3) {
            let mv = u16::from_be_bytes([p[0], p[1]]);
            d.charging = p[2] & 0x80 != 0;
            if mv != 0 {
                d.millivolts = Some(mv);
                d.percent = Some(li_ion_percent(mv as i32));
            }
            return true;
        }
    }
    false
}

fn read_name_and_type20(l: &mut dyn Link, idx: u8, d: &mut Device) {
    let Some(f) = feature_index(l, idx, 0x0005) else {
        return;
    }; // DEVICE_NAME
    if let Some(len) = call(l, idx, f, 0, &[]).and_then(|p| p.first().copied()) {
        let mut name = Vec::new();
        while name.len() < len as usize {
            let Some(chunk) = call(l, idx, f, 1, &[name.len() as u8]) else {
                break;
            };
            let before = name.len();
            name.extend(
                chunk
                    .iter()
                    .take_while(|c| **c != 0)
                    .take(len as usize - before),
            );
            if name.len() == before {
                break;
            }
        }
        let name = String::from_utf8_lossy(&name).trim().to_string();
        if !name.is_empty() {
            let branded = ["Logitech", "G ", "MX", "PRO"]
                .iter()
                .any(|p| name.starts_with(p));
            d.name = if branded {
                name
            } else {
                format!("Logitech {name}")
            };
        }
    }
    if let Some(t) = call(l, idx, f, 2, &[]).and_then(|p| p.first().copied()) {
        d.kind = kind_from_type(t);
    }
}

/// DEVICE_INFORMATION unit id: identifies the physical device across receiver and cable.
fn read_unit_id20(l: &mut dyn Link, idx: u8) -> Option<String> {
    let f = feature_index(l, idx, 0x0003)?;
    let p = call(l, idx, f, 0, &[]).filter(|p| p.len() >= 5)?;
    if p[1..5].iter().all(|b| *b == 0) {
        return None;
    }
    Some(p[1..5].iter().map(|b| format!("{b:02X}")).collect())
}

fn read_battery10(l: &mut dyn Link, idx: u8, d: &mut Device) -> bool {
    let t = Duration::from_millis(REPLY_MS);
    if let Reply::Ok(p) = l.request(idx, 0x81, 0x0D, &[], true, t)
        && p.len() >= 3
        && (1..=100).contains(&p[0])
    {
        d.percent = Some(p[0]);
        d.charging = (0x21..=0x26).contains(&p[2]);
        return true;
    }
    if let Reply::Ok(p) = l.request(idx, 0x81, 0x07, &[], true, t)
        && p.len() >= 2
        && p[0] != 0
    {
        const PCT: [u8; 8] = [0, 5, 10, 20, 35, 50, 75, 100];
        d.percent = Some(PCT[p[0].min(7) as usize]);
        d.coarse = true;
        d.charging = (0x21..=0x26).contains(&p[1]);
        return true;
    }
    false
}

/// Polls every device behind one HID++ link. `direct` = cable / Bluetooth (index 0xFF only).
pub fn poll_link(
    l: &mut dyn Link,
    pid: u16,
    product: &str,
    direct: bool,
    key_suffix: &str,
) -> Vec<Device> {
    let indices: &[u8] = if direct { &[0xFF] } else { &[1, 2, 3, 4, 5, 6] };
    let pings = l.ping_many(indices, Duration::from_millis(PING_MS));
    let mut out = Vec::new();
    for (idx, ping) in pings {
        let name = if direct && !product.is_empty() {
            product
        } else {
            "Logitech wireless device"
        };
        let via = if direct {
            "USB / Bluetooth"
        } else {
            "Logitech receiver"
        };
        let mut d = Device::new(
            format!("logi:{pid:04X}{key_suffix}:{idx}"),
            name,
            Kind::Other,
            via,
        );
        // Receiver devices drop off the link while they sleep; a cable hands them back within a minute.
        d.grace = Duration::from_secs(if direct { 60 } else { 30 * 60 });

        let found = match ping {
            Reply::Ok(p) if p.len() >= 3 && p[0] >= 2 => {
                read_name_and_type20(l, idx, &mut d);
                let ok = read_battery20(l, idx, &mut d);
                if ok {
                    let unit = read_unit_id20(l, idx);
                    d.key = stable_key(&d.key, unit);
                }
                ok
            }
            Reply::Err(0x01) => read_battery10(l, idx, &mut d), // "invalid sub id": HID++ 1.0
            _ => false,
        };
        if found {
            if d.kind == Kind::Other {
                d.kind = guess_kind(&d.name);
            }
            out.push(d);
        }
    }
    out
}

/// Unit-id key for a receiver slot, remembered so one failed unit-id read does not change the key
/// (the tracker would then show the old key as a second, sleeping copy of the same device).
static UNIT_KEYS: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

fn stable_key(slot_key: &str, unit: Option<String>) -> String {
    let mut guard = UNIT_KEYS.lock().unwrap_or_else(|e| e.into_inner());
    let map = guard.get_or_insert_with(HashMap::new);
    match unit {
        Some(u) => {
            let key = format!("logi:unit:{u}");
            map.insert(slot_key.to_string(), key.clone());
            key
        }
        None => map
            .get(slot_key)
            .cloned()
            .unwrap_or_else(|| slot_key.to_string()),
    }
}

/// hidapi-backed link: short (FF00:0001) and long (FF00:0002 / FF43:0202) collections of one device.
struct HidLink {
    short: Option<HidDevice>,
    long: Option<HidDevice>,
    /// After this, requests return NoAnswer at once, so a stalled device cannot push the whole driver
    /// past the registry's deadline and take the devices already read down with it.
    deadline: Instant,
}

/// Total time this driver spends per poll; stays under registry::DRIVER_TIMEOUT (5 s).
const POLL_BUDGET: Duration = Duration::from_secs(4);

impl Link for HidLink {
    fn request(
        &mut self,
        idx: u8,
        b2: u8,
        b3: u8,
        params: &[u8],
        force_short: bool,
        timeout: Duration,
    ) -> Reply {
        if Instant::now() >= self.deadline {
            return Reply::NoAnswer;
        }
        let use_long = self.long.is_some() && !force_short;
        let (len, report) = if use_long { (20, 0x11) } else { (7, 0x10) };
        let Some(dev) = (if use_long { &self.long } else { &self.short }) else {
            return Reply::NoAnswer;
        };
        let mut msg = vec![0u8; len];
        msg[..4].copy_from_slice(&[report, idx, b2, b3]);
        for (slot, v) in msg[4..].iter_mut().zip(params) {
            *slot = *v;
        }
        if dev.write(&msg).is_err() {
            return Reply::NoAnswer;
        }
        let end = (Instant::now() + timeout).min(self.deadline);
        while Instant::now() < end {
            if let Some(r) = self.read_any().and_then(|r| match_reply(&r, idx, b2, b3)) {
                return r;
            }
        }
        Reply::NoAnswer
    }

    fn ping_many(&mut self, indices: &[u8], timeout: Duration) -> Vec<(u8, Reply)> {
        let mut out: Vec<(u8, Reply)> = indices.iter().map(|&i| (i, Reply::NoAnswer)).collect();
        let Some(dev) = self.long.as_ref().or(self.short.as_ref()) else {
            return out;
        };
        let (len, report) = if self.long.is_some() {
            (20, 0x11)
        } else {
            (7, 0x10)
        };
        let mut pending = Vec::new();
        for &idx in indices {
            let mut msg = vec![0u8; len];
            msg[..7].copy_from_slice(&[report, idx, 0x00, 0x10 | SW_ID, PING[0], PING[1], PING[2]]);
            if dev.write(&msg).is_ok() {
                pending.push(idx);
            }
        }
        let end = (Instant::now() + timeout).min(self.deadline);
        while !pending.is_empty() && Instant::now() < end {
            let Some(r) = self.read_any() else { continue };
            let Some(pos) = pending
                .iter()
                .position(|&i| match_reply(&r, i, 0x00, 0x10 | SW_ID).is_some())
            else {
                continue;
            };
            let idx = pending.remove(pos);
            if let Some(reply) = match_reply(&r, idx, 0x00, 0x10 | SW_ID)
                && let Some(slot) = out.iter_mut().find(|(i, _)| *i == idx)
            {
                slot.1 = reply;
            }
        }
        out
    }
}

impl HidLink {
    /// One input report from either collection, waiting at most 15 ms on each.
    fn read_any(&self) -> Option<Vec<u8>> {
        let mut buf = [0u8; 64];
        for d in [&self.long, &self.short].into_iter().flatten() {
            if let Ok(n) = d.read_timeout(&mut buf, 15)
                && n >= 7
            {
                return Some(buf[..n].to_vec());
            }
        }
        None
    }
}

/// Polls the Logitech HID collections the dispatcher assigned to this driver.
pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    struct Group {
        pid: u16,
        product: String,
        bluetooth: bool,
        short: Option<std::ffi::CString>,
        long: Option<std::ffi::CString>,
    }
    let mut groups: Vec<(String, Group)> = Vec::new();
    for info in infos {
        let (page, usage) = (info.usage_page(), info.usage());
        let is_short = page == 0xFF00 && usage == 0x0001;
        let is_long = (page == 0xFF00 && usage == 0x0002) || (page == 0xFF43 && usage == 0x0202);
        if !is_short && !is_long {
            continue;
        }
        let path = info.path().to_string_lossy().to_string();
        let key = physical_key(&path);
        let idx = match groups.iter().position(|(k, _)| *k == key) {
            Some(i) => i,
            None => {
                let lower = path.to_lowercase();
                groups.push((
                    key,
                    Group {
                        pid: info.product_id(),
                        product: info.product_string().unwrap_or_default().to_string(),
                        bluetooth: lower.contains("{00001812-") || lower.contains("{00001124-"),
                        short: None,
                        long: None,
                    },
                ));
                groups.len() - 1
            }
        };
        let g = &mut groups[idx].1;
        if is_short {
            g.short = Some(info.path().to_owned())
        } else {
            g.long = Some(info.path().to_owned())
        }
    }

    let deadline = Instant::now() + POLL_BUDGET;
    let mut out = Vec::new();
    let mut seen_pid: Vec<u16> = Vec::new();
    for (_, g) in groups {
        let mut link = HidLink {
            short: g.short.as_ref().and_then(|p| api.open_path(p).ok()),
            long: g.long.as_ref().and_then(|p| api.open_path(p).ok()),
            deadline,
        };
        if link.short.is_none() && link.long.is_none() {
            continue;
        }
        let receiver = g.product.contains("Receiver");
        // A device answering a HID++ 2.0 ping at 0xFF is connected directly; otherwise it is a receiver.
        let direct = g.bluetooth
            || (!receiver
                && matches!(link.request(0xFF, 0x00, 0x10 | SW_ID, &PING, false, Duration::from_millis(PING_MS)),
                    Reply::Ok(ref p) if p.len() >= 3 && p[0] >= 2));
        let n = seen_pid.iter().filter(|p| **p == g.pid).count();
        let suffix = if n == 0 {
            String::new()
        } else {
            format!("#{}", n + 1)
        };
        seen_pid.push(g.pid);
        out.extend(poll_link(&mut link, g.pid, &g.product, direct, &suffix));
    }
    // The same mouse can show up on its receiver and on its charging cable; keep one entry per unit.
    let mut seen = Vec::new();
    out.retain(|d| {
        !d.key.starts_with("logi:unit:") || {
            let new = !seen.contains(&d.key);
            seen.push(d.key.clone());
            new
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Answers (idx, b2, b3, first param) from a table; everything else gets no answer.
    struct Fake(HashMap<(u8, u8, u8, u8), Reply>);

    impl Link for Fake {
        fn request(
            &mut self,
            idx: u8,
            b2: u8,
            b3: u8,
            params: &[u8],
            _: bool,
            _: Duration,
        ) -> Reply {
            let k = (
                idx,
                b2,
                b3,
                if b2 == 0 {
                    params.get(1).copied().unwrap_or(0)
                } else {
                    params.first().copied().unwrap_or(0)
                },
            );
            match self.0.get(&k) {
                Some(Reply::Ok(p)) => Reply::Ok(p.clone()),
                Some(Reply::Err(e)) => Reply::Err(*e),
                _ => Reply::NoAnswer,
            }
        }
    }

    fn f(func: u8) -> u8 {
        (func << 4) | SW_ID
    }

    /// A receiver with one HID++ 2.0 mouse on index 1: name, unified battery 72%, discharging.
    fn superlight_on_receiver() -> Fake {
        let mut m = HashMap::new();
        m.insert((1, 0x00, 0x10 | SW_ID, 0x00), Reply::Ok(vec![4, 2, 0x5A]));
        // root feature lookups (second param byte is the low byte of the feature id)
        m.insert((1, 0x00, f(0), 0x05), Reply::Ok(vec![0x05])); // DEVICE_NAME at index 5
        m.insert((1, 0x00, f(0), 0x04), Reply::Ok(vec![0x08])); // UNIFIED_BATTERY at index 8
        m.insert((1, 0x00, f(0), 0x03), Reply::Ok(vec![0x02])); // DEVICE_INFORMATION at index 2
        m.insert((1, 0x05, f(0), 0), Reply::Ok(vec![26]));
        m.insert(
            (1, 0x05, f(1), 0),
            Reply::Ok(b"PRO X SUPERLIGHT 2".to_vec()),
        );
        m.insert((1, 0x05, f(1), 18), Reply::Ok(vec![0; 16]));
        m.insert((1, 0x05, f(2), 0), Reply::Ok(vec![0x03]));
        m.insert((1, 0x08, f(1), 0), Reply::Ok(vec![72, 0x04, 0x00]));
        m.insert(
            (1, 0x02, f(0), 0),
            Reply::Ok(vec![1, 0xAB, 0xCD, 0x12, 0x34]),
        );
        Fake(m)
    }

    #[test]
    fn reads_mouse_behind_receiver() {
        let devices = poll_link(
            &mut superlight_on_receiver(),
            0xC54D,
            "USB Receiver",
            false,
            "",
        );
        assert_eq!(devices.len(), 1);
        let d = &devices[0];
        assert_eq!(d.name, "PRO X SUPERLIGHT 2");
        assert_eq!(d.kind, Kind::Mouse);
        assert_eq!(d.percent, Some(72));
        assert!(!d.charging);
        assert_eq!(d.key, "logi:unit:ABCD1234");
    }

    #[test]
    fn unified_battery_charging_complete_is_full() {
        let mut fake = superlight_on_receiver();
        fake.0
            .insert((1, 0x08, f(1), 0), Reply::Ok(vec![100, 0x08, 0x03]));
        let d = &poll_link(&mut fake, 0xC54D, "", false, "")[0];
        assert!(d.charging && d.full);
        assert_eq!(d.percent, Some(100));
    }

    #[test]
    fn voltage_feature_maps_to_percent() {
        let mut m = HashMap::new();
        m.insert(
            (0xFF, 0x00, 0x10 | SW_ID, 0x00),
            Reply::Ok(vec![4, 2, 0x5A]),
        );
        m.insert((0xFF, 0x00, f(0), 0x01), Reply::Ok(vec![0x06])); // BATTERY_VOLTAGE
        m.insert((0xFF, 0x06, f(0), 0), Reply::Ok(vec![0x0F, 0x1B, 0x00])); // 3867 mV
        let d = &poll_link(&mut Fake(m), 0xC08B, "G502", true, "")[0];
        assert_eq!(d.millivolts, Some(3867));
        assert_eq!(d.percent, Some(61));
    }

    #[test]
    fn hidpp10_register_battery() {
        let mut m = HashMap::new();
        m.insert((2, 0x00, 0x10 | SW_ID, 0x00), Reply::Err(0x01));
        m.insert((2, 0x81, 0x0D, 0), Reply::Ok(vec![55, 0, 0x22]));
        let devices = poll_link(&mut Fake(m), 0xC52B, "", false, "");
        assert_eq!(devices.len(), 1);
        assert_eq!((devices[0].percent, devices[0].charging), (Some(55), true));
    }

    #[test]
    fn key_stays_stable_when_unit_id_read_fails_once() {
        // A pid no other test uses: the unit-key cache is process-wide.
        let mut fake = superlight_on_receiver();
        let first = poll_link(&mut fake, 0xBEEF, "", false, "")[0].key.clone();
        fake.0.remove(&(1, 0x02, f(0), 0)); // unit id read now times out
        let second = poll_link(&mut fake, 0xBEEF, "", false, "")[0].key.clone();
        assert_eq!(first, "logi:unit:ABCD1234");
        assert_eq!(second, first);
    }

    #[test]
    fn empty_receiver_reports_nothing() {
        assert!(poll_link(&mut Fake(HashMap::new()), 0xC54D, "", false, "").is_empty());
    }
}

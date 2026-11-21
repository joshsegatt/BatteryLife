//! Small HID helpers shared by the vendor drivers, so ports from BatteryHub map 1:1.
//!
//! Report buffers follow hidapi conventions (as BatteryHub's HidDev does): for write and feature
//! reports `data[0]` is the report id (0 when the device does not number its reports); reads return
//! the report id as the first byte for numbered reports.

use std::time::{Duration, Instant};

use hidapi::{DeviceInfo, HidApi, HidDevice};

use crate::device::{Kind, guess_kind};
use crate::drivers::generic_hid::kind_from_collections;

/// Distinct (vid, pid) pairs among the collections, in first-seen order.
pub fn products(infos: &[DeviceInfo]) -> Vec<(u16, u16)> {
    let mut out = Vec::new();
    for i in infos {
        let p = (i.vendor_id(), i.product_id());
        if !out.contains(&p) {
            out.push(p);
        }
    }
    out
}

/// Collections of one product that match `pred` (e.g. a vendor usage page and report length).
pub fn find<'a>(
    infos: &'a [DeviceInfo],
    vid: u16,
    pid: u16,
    pred: impl Fn(&DeviceInfo) -> bool,
) -> Vec<&'a DeviceInfo> {
    infos
        .iter()
        .filter(|i| i.vendor_id() == vid && i.product_id() == pid && pred(i))
        .collect()
}

/// Collection on a usage page / usage, the usual way BatteryHub drivers pick their channel.
pub fn on_usage(
    infos: &[DeviceInfo],
    vid: u16,
    pid: u16,
    page: u16,
    usage: u16,
) -> Option<&DeviceInfo> {
    find(infos, vid, pid, |i| {
        i.usage_page() == page && i.usage() == usage
    })
    .into_iter()
    .next()
}

/// Opens a collection; hidapi falls back to zero access, which is enough for feature reports on
/// mice and keyboards Windows holds exclusively.
pub fn open(api: &HidApi, info: &DeviceInfo) -> Option<HidDevice> {
    info.open_device(api).ok()
}

/// Product name reported by the device, trimmed; `fallback` when empty.
pub fn product_name(info: &DeviceInfo, fallback: &str) -> String {
    info.product_string()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(fallback)
        .to_string()
}

/// Device type from the product's top-level collections (mouse > gamepad > keyboard), else its name.
pub fn kind_of(infos: &[DeviceInfo], vid: u16, pid: u16, name: &str) -> Kind {
    let cols: Vec<(u16, u16)> = find(infos, vid, pid, |_| true)
        .iter()
        .map(|i| (i.usage_page(), i.usage()))
        .collect();
    match kind_from_collections(&cols) {
        Kind::Other => guess_kind(name),
        k => k,
    }
}

/// Linear map lo..hi -> 0..100, clamped.
pub fn map_to_100(v: i32, lo: i32, hi: i32) -> u8 {
    if hi == lo {
        return 0;
    }
    ((i64::from(v) - i64::from(lo)) * 100 / (i64::from(hi) - i64::from(lo))).clamp(0, 100) as u8
}

/// Groups HID collections of one physical interface: drops "&colNN" and the last "&NNNN" of the path.
pub fn physical_key(path: &str) -> String {
    let p = path.to_lowercase();
    let parts: Vec<&str> = p.splitn(4, '#').collect();
    if parts.len() < 4 {
        return String::new();
    }
    let hw = match parts[1].find("&col") {
        Some(i) => format!("{}{}", &parts[1][..i], parts[1].get(i + 6..).unwrap_or("")),
        None => parts[1].to_string(),
    };
    let inst = parts[2].rsplit_once('&').map_or(parts[2], |(a, _)| a);
    format!("{hw}#{inst}")
}

/// Writes `request` and waits for the first input report `accept` takes, up to `timeout`.
/// Returns the report (report id first for numbered reports). Reports `accept` rejects (other
/// traffic, replies to other apps) are skipped.
pub fn request(
    dev: &HidDevice,
    request: &[u8],
    timeout: Duration,
    accept: impl Fn(&[u8]) -> bool,
) -> Option<Vec<u8>> {
    dev.write(request).ok()?;
    read_until(dev, timeout, accept)
}

/// Reads input reports until `accept` takes one or `timeout` passes.
pub fn read_until(
    dev: &HidDevice,
    timeout: Duration,
    accept: impl Fn(&[u8]) -> bool,
) -> Option<Vec<u8>> {
    let end = Instant::now() + timeout;
    let mut buf = [0u8; 128];
    while Instant::now() < end {
        let left = end
            .saturating_duration_since(Instant::now())
            .as_millis()
            .min(50) as i32;
        match dev.read_timeout(&mut buf, left.max(1)) {
            Ok(n) if n > 0 && accept(&buf[..n]) => return Some(buf[..n].to_vec()),
            Ok(_) => {}
            Err(_) => return None,
        }
    }
    None
}

/// Feature report `report_id` of `len` bytes (including the id byte); returns the bytes read.
pub fn get_feature(dev: &HidDevice, report_id: u8, len: usize) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; len.max(2)];
    buf[0] = report_id;
    let n = dev.get_feature_report(&mut buf).ok()?;
    buf.truncate(n);
    Some(buf)
}

/// Drains queued input reports, so a request's reply is not confused with an old one.
pub fn flush(dev: &HidDevice) {
    let mut buf = [0u8; 128];
    while matches!(dev.read_timeout(&mut buf, 0), Ok(n) if n > 0) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_linearly_and_clamps() {
        assert_eq!(map_to_100(3700, 3500, 4100), 33);
        assert_eq!(map_to_100(4200, 3500, 4100), 100);
        assert_eq!(map_to_100(3000, 3500, 4100), 0);
        assert_eq!(map_to_100(5, 5, 5), 0);
    }

    #[test]
    fn physical_key_merges_collections() {
        let a = r"\\?\HID#VID_046D&PID_C54D&MI_02&Col01#7&a2e4456&0&0000#{4d1e55b2-f16f-11cf-88cb-001111000030}";
        let b = r"\\?\HID#VID_046D&PID_C54D&MI_02&Col02#7&a2e4456&0&0001#{4d1e55b2-f16f-11cf-88cb-001111000030}";
        assert_eq!(physical_key(a), physical_key(b));
        assert_eq!(physical_key(a), "vid_046d&pid_c54d&mi_02#7&a2e4456&0");
    }
}

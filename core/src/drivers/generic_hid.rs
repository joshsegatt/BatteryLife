//! Fallback for any HID device that declares a standard battery usage (Generic Device Controls
//! "Battery Strength" 06:20, or Battery System "Absolute State Of Charge" 85:65) in a feature or
//! input report. Runs only on devices no vendor driver matched.
//!
//! Ported from BatteryHub (src/drivers/generic.cpp, PollGenericHid, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0.

use hidapi::{DeviceInfo, HidApi};
use windows::Win32::Devices::HumanInterfaceDevice::{
    HIDP_CAPS, HIDP_REPORT_TYPE, HIDP_STATUS_SUCCESS, HIDP_VALUE_CAPS, HidD_FreePreparsedData,
    HidD_GetFeature, HidD_GetInputReport, HidD_GetPreparsedData, HidP_Feature, HidP_GetCaps,
    HidP_GetUsageValue, HidP_GetValueCaps, HidP_Input, PHIDP_PREPARSED_DATA,
};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::core::PCWSTR;

use crate::device::{Device, Kind, guess_kind};

fn is_battery_usage(page: u16, usage: u16) -> bool {
    (page == 0x06 && usage == 0x20) || (page == 0x85 && usage == 0x65)
}

/// Raw value to percent using the declared logical range. Apple devices send a plain percentage
/// while declaring 0-255 (Linux handles them with HID_BATTERY_QUIRK_PERCENT).
pub fn scale(value: u32, min: i32, max: i32, vid: u16) -> Option<u8> {
    if vid == 0x05AC && value <= 100 {
        return Some(value as u8);
    }
    if max > min {
        let pct =
            (i64::from(value as i32) - i64::from(min)) * 100 / (i64::from(max) - i64::from(min));
        return Some(pct.clamp(0, 100) as u8);
    }
    (value <= 100).then_some(value as u8)
}

/// Device type from the top-level collections of one product (mouse > gamepad > keyboard).
pub fn kind_from_collections(collections: &[(u16, u16)]) -> Kind {
    let has = |u: u16| collections.contains(&(0x01, u));
    if has(0x02) {
        Kind::Mouse
    } else if has(0x04) || has(0x05) {
        Kind::Gamepad
    } else if has(0x06) {
        Kind::Keyboard
    } else {
        Kind::Other
    }
}

struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Reads the battery usage from one collection, opened with zero access (enough for feature and
/// input reports, and it works on mice and keyboards Windows holds exclusively).
fn read_battery(path: &std::ffi::CStr, vid: u16) -> Option<u8> {
    let wide: Vec<u16> = path
        .to_string_lossy()
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let h = unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        )
    }
    .ok()?;
    let h = Handle(h);
    let mut pp = PHIDP_PREPARSED_DATA::default();
    if !unsafe { HidD_GetPreparsedData(h.0, &mut pp) } {
        return None;
    }
    let result = (|| {
        let mut caps = HIDP_CAPS::default();
        if unsafe { HidP_GetCaps(pp, &mut caps) } != HIDP_STATUS_SUCCESS {
            return None;
        }
        let kinds: [(HIDP_REPORT_TYPE, u16, u16); 2] = [
            (
                HidP_Feature,
                caps.NumberFeatureValueCaps,
                caps.FeatureReportByteLength,
            ),
            (
                HidP_Input,
                caps.NumberInputValueCaps,
                caps.InputReportByteLength,
            ),
        ];
        for (kind, n, len) in kinds {
            if n == 0 || len == 0 {
                continue;
            }
            let mut count = n;
            let mut vc = vec![HIDP_VALUE_CAPS::default(); n as usize];
            if unsafe { HidP_GetValueCaps(kind, vc.as_mut_ptr(), &mut count, pp) }
                != HIDP_STATUS_SUCCESS
            {
                continue;
            }
            for v in &vc[..count as usize] {
                let usage = unsafe {
                    if v.IsRange {
                        v.Anonymous.Range.UsageMin
                    } else {
                        v.Anonymous.NotRange.Usage
                    }
                };
                if !is_battery_usage(v.UsagePage, usage) {
                    continue;
                }
                let mut buf = vec![0u8; len as usize];
                buf[0] = v.ReportID;
                let got = unsafe {
                    if kind == HidP_Feature {
                        HidD_GetFeature(h.0, buf.as_mut_ptr().cast(), len as u32)
                    } else {
                        HidD_GetInputReport(h.0, buf.as_mut_ptr().cast(), len as u32)
                    }
                };
                if !got {
                    continue;
                }
                let mut value = 0u32;
                let status = unsafe {
                    HidP_GetUsageValue(
                        kind,
                        v.UsagePage,
                        Some(v.LinkCollection),
                        usage,
                        &mut value,
                        pp,
                        &buf,
                    )
                };
                if status != HIDP_STATUS_SUCCESS {
                    continue;
                }
                if let Some(p) = scale(value, v.LogicalMin, v.LogicalMax, vid) {
                    return Some(p);
                }
            }
        }
        None
    })();
    unsafe {
        let _ = HidD_FreePreparsedData(pp);
    }
    result
}

/// Polls the HID collections no vendor driver matched.
pub fn poll(_api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    // Drawing tablets (any collection on the Digitizers page) are never read: polling one for a
    // battery report flipped the Y axis of an XP-Pen tablet (BatteryHub issue #232).
    let digitizers: Vec<(u16, u16)> = infos
        .iter()
        .filter(|i| i.usage_page() == 0x0D)
        .map(|i| (i.vendor_id(), i.product_id()))
        .collect();
    let mut out: Vec<Device> = Vec::new();
    for info in infos {
        let (vid, pid) = (info.vendor_id(), info.product_id());
        let key = format!("hid:{vid:04X}:{pid:04X}");
        if out.iter().any(|d| d.key == key) || digitizers.contains(&(vid, pid)) {
            continue;
        }
        if matches!(info.usage_page(), 0x84 | 0x85) {
            continue; // UPS and power supplies (Power Device / Battery System top-level collections)
        }
        let Some(pct) = read_battery(info.path(), vid) else {
            continue;
        };
        let name = info
            .product_string()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("Wireless device")
            .trim()
            .to_string();
        let collections: Vec<(u16, u16)> = infos
            .iter()
            .filter(|i| i.vendor_id() == vid && i.product_id() == pid)
            .map(|i| (i.usage_page(), i.usage()))
            .collect();
        let mut kind = kind_from_collections(&collections);
        if kind == Kind::Other {
            kind = guess_kind(&name);
        }
        let mut d = Device::new(key, name, kind, "HID");
        d.percent = Some(pct);
        out.push(d);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scales_logical_range() {
        assert_eq!(scale(128, 0, 255, 0x1234), Some(50));
        assert_eq!(scale(5, 0, 10, 0x1234), Some(50));
        assert_eq!(scale(300, 0, 255, 0x1234), Some(100));
        assert_eq!(scale(80, 0, 0, 0x1234), Some(80));
        assert_eq!(scale(180, 0, 0, 0x1234), None);
    }

    #[test]
    fn apple_reports_plain_percent() {
        assert_eq!(scale(64, 0, 255, 0x05AC), Some(64));
    }

    #[test]
    fn collections_pick_the_kind() {
        assert_eq!(
            kind_from_collections(&[(0x01, 0x06), (0x01, 0x02)]),
            Kind::Mouse
        );
        assert_eq!(
            kind_from_collections(&[(0x01, 0x06), (0x0C, 0x01)]),
            Kind::Keyboard
        );
        assert_eq!(kind_from_collections(&[(0x01, 0x05)]), Kind::Gamepad);
        assert_eq!(kind_from_collections(&[(0xFF00, 0x01)]), Kind::Other);
    }
}

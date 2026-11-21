//! Xbox controllers.
//!  1. Windows.Gaming.Input: every Xbox controller Windows knows (Xbox Wireless adapter, USB,
//!     Bluetooth) with a real battery report (remaining / full mWh -> %).
//!  2. XInput fallback when (1) finds none: coarse levels (empty / low / medium / full).
//!
//! Ported from BatteryHub (src/drivers/xinput.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0.

use std::sync::Once;
use std::time::Duration;

use windows::Gaming::Input::RawGameController;
use windows::System::Power::BatteryStatus;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows::Win32::UI::Input::XboxController::{
    BATTERY_DEVTYPE_GAMEPAD, BATTERY_LEVEL_EMPTY, BATTERY_LEVEL_FULL, BATTERY_LEVEL_LOW,
    BATTERY_LEVEL_MEDIUM, BATTERY_TYPE_ALKALINE, BATTERY_TYPE_NIMH, BATTERY_TYPE_UNKNOWN,
    XINPUT_BATTERY_INFORMATION, XINPUT_STATE, XInputGetBatteryInformation, XInputGetState,
};

use crate::device::{Device, Kind};

/// Vendors whose controllers report a real battery through Windows.Gaming.Input: Microsoft, and
/// GameSir in Xbox (GIP) mode.
fn reports_battery(vid: u16) -> bool {
    matches!(vid, 0x045E | 0x3537)
}

/// Percent from a battery report; None when there is no battery or no capacity.
pub fn percent(remaining_mwh: i32, full_mwh: i32) -> Option<u8> {
    (full_mwh > 0)
        .then(|| (i64::from(remaining_mwh.max(0)) * 100 / i64::from(full_mwh)).clamp(0, 100) as u8)
}

pub fn poll() -> Vec<Device> {
    // WinRT on this (driver) thread; harmless when already initialized.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let precise = poll_wgi();
    if precise.is_empty() {
        poll_xinput()
    } else {
        precise
    }
}

fn poll_wgi() -> Vec<Device> {
    // The controller list fills in asynchronously after the first use in a process.
    static WARM: Once = Once::new();
    WARM.call_once(|| {
        let _ = RawGameController::RawGameControllers();
        std::thread::sleep(Duration::from_millis(300));
    });
    let Ok(list) = RawGameController::RawGameControllers() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (i, raw) in list.into_iter().enumerate() {
        let (Ok(vid), Ok(pid)) = (raw.HardwareVendorId(), raw.HardwareProductId()) else {
            continue;
        };
        if !reports_battery(vid) {
            continue;
        }
        let Ok(report) = raw.TryGetBatteryReport() else {
            continue;
        };
        let status = report.Status().unwrap_or(BatteryStatus::NotPresent);
        if status == BatteryStatus::NotPresent {
            continue;
        }
        let rem = report
            .RemainingCapacityInMilliwattHours()
            .and_then(|r| r.Value())
            .unwrap_or(0);
        let full = report
            .FullChargeCapacityInMilliwattHours()
            .and_then(|r| r.Value())
            .unwrap_or(0);
        let Some(pct) = percent(rem, full) else {
            continue;
        };
        let name = match vid {
            0x3537 => "GameSir controller".to_string(), // GIP reports a generic name for third-party pads
            _ => raw
                .DisplayName()
                .map(|s| s.to_string())
                .ok()
                .filter(|s| !s.is_empty())
                .unwrap_or("Xbox controller".into()),
        };
        let mut d = Device::new(
            format!("xbox:{pid:04X}:{i}"),
            name,
            Kind::Gamepad,
            "Xbox Wireless / Bluetooth",
        );
        d.percent = Some(pct);
        d.charging = status == BatteryStatus::Charging;
        out.push(d);
    }
    out
}

/// Coarse fallback. XInput exposes no hardware id, so entries are keyed by player slot: a pad that
/// reconnects into another slot shows up as a different row until the old one's grace runs out.
fn poll_xinput() -> Vec<Device> {
    let mut out = Vec::new();
    for user in 0..4u32 {
        let mut state = XINPUT_STATE::default();
        if unsafe { XInputGetState(user, &mut state) } != 0 {
            continue; // nothing in this slot
        }
        let mut bi = XINPUT_BATTERY_INFORMATION::default();
        if unsafe { XInputGetBatteryInformation(user, BATTERY_DEVTYPE_GAMEPAD, &mut bi) } != 0 {
            continue;
        }
        // Wired and disconnected pads have no battery to show.
        if ![
            BATTERY_TYPE_ALKALINE,
            BATTERY_TYPE_NIMH,
            BATTERY_TYPE_UNKNOWN,
        ]
        .contains(&bi.BatteryType)
        {
            continue;
        }
        let pct = match bi.BatteryLevel {
            l if l == BATTERY_LEVEL_EMPTY => 5,
            l if l == BATTERY_LEVEL_LOW => 20,
            l if l == BATTERY_LEVEL_MEDIUM => 60,
            l if l == BATTERY_LEVEL_FULL => 100,
            _ => continue,
        };
        let mut d = Device::new(
            format!("xinput:{user}"),
            format!("Controller {}", user + 1),
            Kind::Gamepad,
            "XInput",
        );
        d.percent = Some(pct);
        d.coarse = true;
        out.push(d);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_from_report() {
        assert_eq!(percent(1500, 3000), Some(50));
        assert_eq!(percent(3100, 3000), Some(100));
        assert_eq!(percent(-5, 3000), Some(0));
        assert_eq!(percent(100, 0), None);
    }
}

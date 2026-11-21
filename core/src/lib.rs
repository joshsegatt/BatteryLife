//! BatteryLife core: finds wireless devices and reads their battery. Read-only, never writes settings.

pub mod device;
pub mod logitech;
pub mod phonelink;
pub mod registry;
pub mod tracker;

use std::sync::Arc;

pub use device::{Device, Kind};
pub use tracker::Tracker;

/// Queries every supported device once. Blocking (up to the driver timeout); call off the UI thread.
pub fn poll_all() -> Vec<Device> {
    match hidapi::HidApi::new() {
        Ok(api) => registry::poll(
            Arc::new(api),
            registry::HID_DRIVERS,
            registry::SOURCES,
            registry::DRIVER_TIMEOUT,
        ),
        Err(e) => {
            eprintln!("HID unavailable: {e}");
            registry::SOURCES.iter().flat_map(|s| (s.poll)()).collect()
        }
    }
}

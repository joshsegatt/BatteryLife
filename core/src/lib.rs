//! BatteryLife core: finds wireless devices and reads their battery. Read-only, never writes settings.

pub mod device;
pub mod logitech;
pub mod phonelink;

pub use device::{Device, Kind};

/// Queries every supported source. Blocking (can take a second or two); call off the UI thread.
pub fn poll_all() -> Vec<Device> {
    let mut out = Vec::new();
    match hidapi::HidApi::new() {
        Ok(api) => out.extend(logitech::poll(&api)),
        Err(e) => eprintln!("HID unavailable: {e}"),
    }
    out.extend(phonelink::poll());
    out
}

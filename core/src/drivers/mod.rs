//! Device drivers. Vendor drivers handle the products they declare in `registry.rs`; the generic
//! ones (Bluetooth, HID battery usage, Xbox) cover what no vendor driver matched.

pub mod bluetooth;
pub mod generic_hid;
pub mod logitech;
pub mod phonelink;
pub mod xbox;

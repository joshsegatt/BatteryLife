//! Device drivers. Vendor drivers handle the products they declare in `registry.rs`; the generic
//! ones (Bluetooth, HID battery usage, Xbox) cover what no vendor driver matched.

pub mod bluetooth;
pub mod generic_hid;
pub mod hid;
pub mod logitech;
pub mod phonelink;
pub mod xbox;
// Mice, part A (ported from BatteryHub).
pub mod corsair;
pub mod endgame;
pub mod endgame_we;
pub mod finalmouse;
pub mod glorious;
pub mod lamzu;
pub mod ninjutso;
pub mod pulsar;
pub mod razer;
pub mod scyrox;
pub mod steelseries;
pub mod vaxee;
pub mod vgn;
pub mod wlmouse;
pub mod zowie;

// Keyboard batch ported from BatteryHub.
pub mod ajazz_ak992;
pub mod akko;
pub mod atk_keyboard;
pub mod aula;
pub mod cherry;
pub mod dareu;
pub mod darkrim;
pub mod keychron;
pub mod machenike;
pub mod mchose_keyboard;
pub mod niceboy;
pub mod nuphy;
pub mod redragon_evision;
pub mod rongyuan;
pub mod royal_kludge;
pub mod semico;
pub mod sonix;
pub mod via_dongle;

// Headsets ported from BatteryHub.
pub mod akg;
pub mod alienware;
pub mod astro;
pub mod audeze;
pub mod barracuda_pro;
pub mod endorfy;
pub mod fractal;
pub mod hyperx;
pub mod jabra;
pub mod jbl;
pub mod logitech_g435_0adf;
pub mod maono;
pub mod mchose_headset;
pub mod nzxt;
pub mod plantronics;
pub mod pressplay_icarus;
pub mod razer_headset;
pub mod sony_inzone;
pub mod steelseries_elite;
pub mod turtlebeach;

// Wireless mice, part B (BatteryHub ports).
pub mod arbiter;
pub mod areson;
pub mod atk_bitmouse;
pub mod aula_mouse;
pub mod chimera;
pub mod coolermaster;
pub mod darmoshark;
pub mod eweadn;
pub mod fantech;
pub mod kysona;
pub mod mchose;
pub mod mchose_a5;
pub mod mchose_g3;
pub mod mchose_modern;
pub mod misc_mice;
pub mod msi;
pub mod raigor;
pub mod rapoo;
pub mod rawm;
pub mod rog;
pub mod sinowealth_receiver;
pub mod sprime;
pub mod zenith;

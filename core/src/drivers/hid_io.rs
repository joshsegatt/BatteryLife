//! Report I/O shared by the headset drivers ported from BatteryHub, so their protocol code runs
//! unchanged against a real collection or against scripted replies in tests.
//!
//! `Coll` is one HID collection with the report lengths Windows declares for it (BatteryHub picks
//! collections by those lengths; hidapi does not expose them). `Io` mirrors BatteryHub's `HidDev`
//! (src/hid.h): writes and feature reports are zero-padded to the declared length with the report id
//! in byte 0; reads drop a leading 0 report id, as hidapi does on Windows.
//!
//! Conventions ported from BatteryHub (src/hid.h and src/hid.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0.

use std::ffi::CString;
use std::time::{Duration, Instant};

use hidapi::{DeviceInfo, HidApi, HidDevice};
use windows::Win32::Devices::HumanInterfaceDevice::{
    HIDP_CAPS, HIDP_STATUS_SUCCESS, HidD_FreePreparsedData, HidD_GetPreparsedData, HidP_GetCaps,
    PHIDP_PREPARSED_DATA,
};
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::core::PCWSTR;

use crate::device::Device;
use crate::drivers::hid_util;

/// Total time a driver spends per poll; stays under registry::DRIVER_TIMEOUT (5 s).
pub const POLL_BUDGET: Duration = Duration::from_secs(4);
/// Longest wait for one reply.
pub const MAX_REPLY_MS: u64 = 500;

/// One HID collection as BatteryHub's `HidColl` sees it.
#[derive(Debug, Clone, Default)]
pub struct Coll {
    pub vid: u16,
    pub pid: u16,
    pub page: u16,
    pub usage: u16,
    pub in_len: u16,
    pub out_len: u16,
    pub feat_len: u16,
    /// USB interface number (MI_xx in the path), -1 when unknown.
    pub iface: i32,
    pub product: String,
    pub path: CString,
}

impl Coll {
    /// A collection with the given ids and report lengths (tests and table lookups).
    pub fn new(vid: u16, pid: u16, page: u16, usage: u16, in_len: u16, out_len: u16) -> Self {
        Self {
            vid,
            pid,
            page,
            usage,
            in_len,
            out_len,
            iface: -1,
            ..Self::default()
        }
    }
}

/// Collections of the given HID infos, with their declared report lengths.
pub fn colls(infos: &[DeviceInfo]) -> Vec<Coll> {
    infos
        .iter()
        .map(|i| {
            let (in_len, out_len, feat_len) = report_lengths(i.path()).unwrap_or((0, 0, 0));
            Coll {
                vid: i.vendor_id(),
                pid: i.product_id(),
                page: i.usage_page(),
                usage: i.usage(),
                in_len,
                out_len,
                feat_len,
                iface: interface_of(&i.path().to_string_lossy()),
                product: i.product_string().unwrap_or_default().trim().to_string(),
                path: i.path().to_owned(),
            }
        })
        .collect()
}

/// Interface number from "&mi_xx" in a HID path, as BatteryHub's EnumHid reads it.
pub fn interface_of(path: &str) -> i32 {
    let lower = path.to_lowercase();
    lower
        .find("&mi_")
        .and_then(|i| {
            let hex: String = lower[i + 4..]
                .chars()
                .take_while(char::is_ascii_hexdigit)
                .collect();
            i32::from_str_radix(&hex, 16).ok()
        })
        .unwrap_or(-1)
}

/// Input / output / feature report lengths (report id byte included), read with zero access.
fn report_lengths(path: &std::ffi::CStr) -> Option<(u16, u16, u16)> {
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
    let mut pp = PHIDP_PREPARSED_DATA::default();
    let mut out = None;
    if unsafe { HidD_GetPreparsedData(h, &mut pp) } {
        let mut caps = HIDP_CAPS::default();
        if unsafe { HidP_GetCaps(pp, &mut caps) } == HIDP_STATUS_SUCCESS {
            out = Some((
                caps.InputReportByteLength,
                caps.OutputReportByteLength,
                caps.FeatureReportByteLength,
            ));
        }
        unsafe {
            let _ = HidD_FreePreparsedData(pp);
        }
    }
    unsafe {
        let _ = CloseHandle(h);
    }
    out
}

/// Collections of one product that match `pred`, in enumeration order.
pub fn find(colls: &[Coll], vid: u16, pid: u16, pred: impl Fn(&Coll) -> bool) -> Vec<&Coll> {
    colls
        .iter()
        .filter(|c| c.vid == vid && c.pid == pid && pred(c))
        .collect()
}

/// Distinct (vid, pid) pairs of one vendor, sorted (BatteryHub's `PollContext::Products`).
pub fn products(colls: &[Coll], vid: u16) -> Vec<(u16, u16)> {
    let mut out: Vec<(u16, u16)> = colls
        .iter()
        .filter(|c| c.vid == vid)
        .map(|c| (c.vid, c.pid))
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// CRC-16/XMODEM (poly 0x1021, init 0), used by the Airoha-based framings.
pub fn crc16_xmodem(data: &[u8]) -> u16 {
    data.iter().fold(0u16, |mut crc, &b| {
        crc ^= u16::from(b) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
        crc
    })
}

/// What one collection answered.
#[derive(Debug)]
pub enum Outcome {
    /// Nothing usable: try the next collection.
    NoAnswer,
    /// The receiver answered but its device is off or out of range: nothing is listed (BatteryHub's
    /// `connected = false`), and no other collection of that product is asked.
    Off,
    Found(Device),
}

impl Outcome {
    /// Adds a found device to `out`; true when the product is handled (found or off).
    pub fn settle(self, out: &mut Vec<Device>) -> bool {
        match self {
            Outcome::NoAnswer => false,
            Outcome::Off => true,
            Outcome::Found(d) => {
                out.push(d);
                true
            }
        }
    }
}

/// `base` for the first device with that key in `out`, then `base#2`, `base#3`...
pub fn stable_key(out: &[Device], base: &str) -> String {
    let taken = |k: &str| out.iter().any(|d| d.key == k);
    if !taken(base) {
        return base.to_string();
    }
    (2..)
        .map(|n| format!("{base}#{n}"))
        .find(|k| !taken(k))
        .unwrap_or_default()
}

/// One opened collection, as BatteryHub's `HidDev`.
pub trait Io {
    /// Output report (data[0] = report id); false when it was not sent.
    fn write(&mut self, data: &[u8]) -> bool;
    /// One input report, waiting up to `timeout_ms`; None on timeout or error.
    fn read(&mut self, timeout_ms: u64) -> Option<Vec<u8>>;
    fn set_feature(&mut self, data: &[u8]) -> bool;
    /// Feature report `id` (byte 0 = id), the full declared length.
    fn get_feature(&mut self, id: u8) -> Option<Vec<u8>>;
    /// Value byte of a numbered 2-byte feature report `id`.
    fn get_feature_byte(&mut self, id: u8) -> Option<u8>;
    /// Current input report `id` over the control pipe (byte 0 = id).
    fn get_input_report(&mut self, id: u8) -> Option<Vec<u8>>;
    /// Sends an output report over the control pipe and returns the first input report after it.
    fn set_output_and_read(&mut self, data: &[u8], timeout_ms: u64) -> Option<Vec<u8>>;
    /// Drops queued input reports.
    fn flush(&mut self);
    /// Waits (a no-op in tests).
    fn sleep(&mut self, ms: u64);
}

/// Opens a collection for a driver; None when it cannot be opened.
pub type Open<'a> = dyn FnMut(&Coll) -> Option<Box<dyn Io + 'a>> + 'a;

/// Writes `req` and returns the first input report `accept` takes within `timeout_ms`
/// (BatteryHub's per-driver `Query` helpers). Stops at the first read that times out.
pub fn query(
    io: &mut dyn Io,
    req: &[u8],
    timeout_ms: u64,
    accept: impl Fn(&[u8]) -> bool,
) -> Option<Vec<u8>> {
    if !io.write(req) {
        return None;
    }
    read_until(io, timeout_ms, accept)
}

/// Reads input reports until `accept` takes one, a read times out, or `timeout_ms` passes.
pub fn read_until(
    io: &mut dyn Io,
    timeout_ms: u64,
    accept: impl Fn(&[u8]) -> bool,
) -> Option<Vec<u8>> {
    let end = Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        let left = end.saturating_duration_since(Instant::now()).as_millis() as u64;
        if left == 0 {
            return None;
        }
        let r = io.read(left)?;
        if accept(&r) {
            return Some(r);
        }
    }
}

/// Runs a driver's protocol over real collections, within POLL_BUDGET.
pub fn run(
    api: &HidApi,
    infos: &[DeviceInfo],
    poll: impl FnOnce(&[Coll], &mut Open<'_>) -> Vec<Device>,
) -> Vec<Device> {
    let deadline = Instant::now() + POLL_BUDGET;
    let colls = colls(infos);
    let mut open = |c: &Coll| -> Option<Box<dyn Io>> {
        if Instant::now() >= deadline {
            return None;
        }
        let dev = api.open_path(&c.path).ok()?;
        Some(Box::new(HidIo {
            dev,
            in_len: c.in_len as usize,
            out_len: c.out_len as usize,
            feat_len: c.feat_len as usize,
            deadline,
        }))
    };
    poll(&colls, &mut open)
}

/// hidapi-backed `Io`. After the deadline every call fails at once, so a stalled device cannot push
/// the driver past the registry's deadline.
struct HidIo {
    dev: HidDevice,
    in_len: usize,
    out_len: usize,
    feat_len: usize,
    deadline: Instant,
}

impl HidIo {
    /// `ms` capped by the time left; 0 when the budget is used up.
    fn left(&self, ms: u64) -> u64 {
        (self
            .deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as u64)
            .min(ms)
    }

    fn padded(data: &[u8], len: usize) -> Vec<u8> {
        let mut buf = vec![0u8; len];
        let n = len.min(data.len());
        buf[..n].copy_from_slice(&data[..n]);
        buf
    }
}

impl Io for HidIo {
    fn write(&mut self, data: &[u8]) -> bool {
        if self.out_len == 0 || data.is_empty() || self.left(1) == 0 {
            return false;
        }
        self.dev.write(&Self::padded(data, self.out_len)).is_ok()
    }

    fn read(&mut self, timeout_ms: u64) -> Option<Vec<u8>> {
        let t = self.left(timeout_ms);
        if self.in_len == 0 || t == 0 {
            return None;
        }
        let mut buf = vec![0u8; self.in_len.max(2)];
        match self.dev.read_timeout(&mut buf, t as i32) {
            Ok(n) if n > 0 => {
                buf.truncate(n);
                Some(buf)
            }
            _ => None,
        }
    }

    fn set_feature(&mut self, data: &[u8]) -> bool {
        if self.feat_len == 0 || data.is_empty() || self.left(1) == 0 {
            return false;
        }
        self.dev
            .send_feature_report(&Self::padded(data, self.feat_len))
            .is_ok()
    }

    fn get_feature(&mut self, id: u8) -> Option<Vec<u8>> {
        if self.feat_len == 0 || self.left(1) == 0 {
            return None;
        }
        let mut buf = vec![0u8; self.feat_len];
        buf[0] = id;
        self.dev.get_feature_report(&mut buf).ok()?;
        Some(buf)
    }

    fn get_feature_byte(&mut self, id: u8) -> Option<u8> {
        if id == 0 || self.feat_len < 2 || self.left(1) == 0 {
            return None;
        }
        let mut buf = [id, 0xFF];
        let n = self.dev.get_feature_report(&mut buf).ok()?;
        (n == 2 && buf[0] == id).then_some(buf[1])
    }

    fn get_input_report(&mut self, id: u8) -> Option<Vec<u8>> {
        if self.in_len == 0 || self.left(1) == 0 {
            return None;
        }
        let mut buf = vec![0u8; self.in_len];
        buf[0] = id;
        self.dev.get_input_report(&mut buf).ok()?;
        Some(buf)
    }

    fn set_output_and_read(&mut self, data: &[u8], timeout_ms: u64) -> Option<Vec<u8>> {
        if self.in_len == 0 || self.out_len == 0 || data.is_empty() || self.left(1) == 0 {
            return None;
        }
        self.dev
            .send_output_report(&Self::padded(data, self.out_len))
            .ok()?;
        self.read(timeout_ms)
    }

    fn flush(&mut self) {
        hid_util::flush(&self.dev);
    }

    fn sleep(&mut self, ms: u64) {
        std::thread::sleep(Duration::from_millis(self.left(ms)));
    }
}

/// Scripted `Io` for driver tests: no hardware is touched.
#[cfg(test)]
pub mod fake {
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    type Replies = Box<dyn FnMut(&[u8]) -> Vec<Vec<u8>>>;
    type Inputs = Box<dyn FnMut(u8, usize) -> Option<Vec<u8>>>;

    pub struct State {
        pub write_ok: bool,
        /// When set, decides per write whether it is accepted.
        pub write_filter: Option<fn(&[u8]) -> bool>,
        pub writes: Vec<Vec<u8>>,
        /// Input reports queued for `read` by each write.
        pub on_write: Option<Replies>,
        pub pending: VecDeque<Vec<u8>>,
        pub reads: usize,
        pub set_feature_ok: bool,
        pub set_features: Vec<Vec<u8>>,
        /// Feature reports queued for `get_feature` by each `set_feature`.
        pub on_set_feature: Option<Replies>,
        pub features: VecDeque<Vec<u8>>,
        pub feature_reads: usize,
        pub feature_bytes: Vec<(u8, u8)>,
        /// Answers `get_input_report(id)`, given how many output reports were sent; else `inputs`.
        pub on_input: Option<Inputs>,
        pub inputs: VecDeque<Vec<u8>>,
        pub input_reads: usize,
        pub outputs: Vec<Vec<u8>>,
        pub output_reply: Option<Vec<u8>>,
        pub flushes: usize,
    }

    impl Default for State {
        fn default() -> Self {
            Self {
                write_ok: true,
                write_filter: None,
                writes: Vec::new(),
                on_write: None,
                pending: VecDeque::new(),
                reads: 0,
                set_feature_ok: true,
                set_features: Vec::new(),
                on_set_feature: None,
                features: VecDeque::new(),
                feature_reads: 0,
                feature_bytes: Vec::new(),
                on_input: None,
                inputs: VecDeque::new(),
                input_reads: 0,
                outputs: Vec::new(),
                output_reply: None,
                flushes: 0,
            }
        }
    }

    #[derive(Clone, Default)]
    pub struct Fake(pub Rc<RefCell<State>>);

    impl Fake {
        pub fn new() -> Self {
            Self::default()
        }
        pub fn s(&self) -> std::cell::RefMut<'_, State> {
            self.0.borrow_mut()
        }
        /// Input reports `read` returns, in order.
        pub fn queue(&self, reports: Vec<Vec<u8>>) -> &Self {
            self.s().pending.extend(reports);
            self
        }
        pub fn replies(&self, f: impl FnMut(&[u8]) -> Vec<Vec<u8>> + 'static) -> &Self {
            self.s().on_write = Some(Box::new(f));
            self
        }
        /// An opener that hands out this fake and counts the collections opened.
        pub fn opener<'a>(&'a self, opened: &'a RefCell<Vec<Coll>>) -> Box<Open<'a>> {
            Box::new(move |c: &Coll| {
                opened.borrow_mut().push(c.clone());
                Some(Box::new(self.clone()) as Box<dyn Io>)
            })
        }
    }

    impl Io for Fake {
        fn write(&mut self, data: &[u8]) -> bool {
            let mut s = self.s();
            s.writes.push(data.to_vec());
            if !s.write_ok || s.write_filter.is_some_and(|f| !f(data)) {
                return false;
            }
            if let Some(mut f) = s.on_write.take() {
                let r = f(data);
                s.pending.extend(r);
                s.on_write = Some(f);
            }
            true
        }
        fn read(&mut self, _: u64) -> Option<Vec<u8>> {
            let mut s = self.s();
            s.reads += 1;
            s.pending.pop_front()
        }
        fn set_feature(&mut self, data: &[u8]) -> bool {
            let mut s = self.s();
            s.set_features.push(data.to_vec());
            if !s.set_feature_ok {
                return false;
            }
            if let Some(mut f) = s.on_set_feature.take() {
                let r = f(data);
                s.features = r.into();
                s.on_set_feature = Some(f);
            }
            true
        }
        fn get_feature(&mut self, _: u8) -> Option<Vec<u8>> {
            let mut s = self.s();
            s.feature_reads += 1;
            s.features.pop_front()
        }
        fn get_feature_byte(&mut self, id: u8) -> Option<u8> {
            let s = self.s();
            s.feature_bytes.iter().find(|(i, _)| *i == id).map(|p| p.1)
        }
        fn get_input_report(&mut self, id: u8) -> Option<Vec<u8>> {
            let mut s = self.s();
            s.input_reads += 1;
            let sends = s.outputs.len();
            if let Some(mut f) = s.on_input.take() {
                let r = f(id, sends);
                s.on_input = Some(f);
                return r;
            }
            s.inputs.pop_front().filter(|r| !r.is_empty())
        }
        fn set_output_and_read(&mut self, data: &[u8], _: u64) -> Option<Vec<u8>> {
            let mut s = self.s();
            s.outputs.push(data.to_vec());
            s.output_reply.clone()
        }
        fn flush(&mut self) {
            self.s().flushes += 1;
        }
        fn sleep(&mut self, _: u64) {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc16_check_value() {
        assert_eq!(crc16_xmodem(b"123456789"), 0x31C3);
    }

    #[test]
    fn interface_from_path() {
        assert_eq!(
            interface_of(r"\\?\HID#VID_1038&PID_2244&MI_03&Col02#8&1#{4d1e}"),
            3
        );
        assert_eq!(interface_of(r"\\?\HID#VID_1038&PID_2244#8&1#{4d1e}"), -1);
    }

    #[test]
    fn identical_devices_get_numbered_keys() {
        let mut out = Vec::new();
        let k = |out: &Vec<Device>| stable_key(out, "astro:0B0B");
        out.push(Device::new(k(&out), "A", crate::device::Kind::Headset, "x"));
        out.push(Device::new(k(&out), "A", crate::device::Kind::Headset, "x"));
        assert_eq!(out[0].key, "astro:0B0B");
        assert_eq!(out[1].key, "astro:0B0B#2");
    }
}

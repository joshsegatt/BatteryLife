//! The one HID layer the vendor drivers ported from BatteryHub run on.
//!
//! BatteryHub's drivers pick a collection by its ids, USB interface, usage and report lengths
//! (`HidColl`; hidapi does not expose the lengths, so they come from `HidP_GetCaps`), then talk
//! through a small `HidDev`. Here that is [`Coll`], the [`Dev`] trait for an open collection and the
//! [`Hid`] trait that opens collections and keeps the clock. A driver's whole poll is written against
//! those, so it runs on hidapi in [`run`] and on scripted replies in tests ([`fake::Fake`]).
//!
//! Every real call is bounded by the poll's deadline ([`BUDGET`]): once it has passed, sends fail and
//! reads return nothing at once, so a stalled device cannot push a driver past the registry's
//! deadline.
//!
//! Buffers follow hidapi's conventions, which match BatteryHub's: `data[0]` of a write, output or
//! feature report is the report id (0 when the device does not number its reports), and sends are
//! zero padded to the declared report length; a read returns the report id first for numbered
//! reports and no id byte for unnumbered ones; feature and input reports read over the control pipe
//! come back whole, id first.
//!
//! Follows the HID layer of BatteryHub (src/hid.h and src/hid.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0.

use std::ffi::{CStr, CString};
use std::time::{Duration, Instant};

use hidapi::{DeviceInfo, HidApi, HidDevice};
use windows::Win32::Devices::HumanInterfaceDevice::{
    HIDP_CAPS, HIDP_STATUS_SUCCESS, HidD_FreePreparsedData, HidD_GetFeature, HidD_GetPreparsedData,
    HidD_SetFeature, HidP_GetCaps, PHIDP_PREPARSED_DATA,
};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::core::PCWSTR;

use crate::device::{Device, Kind, guess_kind};
use crate::drivers::generic_hid::kind_from_collections;

/// Total time one driver spends per poll; stays under registry::DRIVER_TIMEOUT (5 s).
pub const BUDGET: Duration = Duration::from_secs(4);
/// Longest wait for one reply; BatteryHub sometimes waits 1-1.5 s, but the registry gives a whole
/// driver 5 s.
pub const MAX_WAIT: Duration = Duration::from_millis(500);
/// How long a receiver device's last level stays visible while it sleeps.
pub const RECEIVER_GRACE: Duration = Duration::from_secs(30 * 60);
pub const RECEIVER: &str = "2.4 GHz receiver";
pub const CABLE: &str = "USB cable";

/// One HID collection, as BatteryHub's `HidColl` describes it. Report lengths include the id byte.
#[derive(Debug, Clone, Default)]
pub struct Coll {
    pub vid: u16,
    pub pid: u16,
    /// USB interface number (MI_xx in the path), -1 when unknown.
    pub iface: i32,
    pub page: u16,
    pub usage: u16,
    pub in_len: u16,
    pub out_len: u16,
    pub feat_len: u16,
    /// Never part of a device key: it changes with the USB port.
    pub path: String,
    /// Product string as the device reports it, trimmed.
    pub product: String,
}

impl Coll {
    /// A collection with the given ids and report lengths, at path "VVVV:PPPP:UUUU:SSSS" (tests and
    /// table lookups).
    pub fn new(vid: u16, pid: u16, page: u16, usage: u16, in_len: u16, out_len: u16) -> Self {
        Self {
            vid,
            pid,
            iface: -1,
            page,
            usage,
            in_len,
            out_len,
            feat_len: 0,
            path: format!("{vid:04X}:{pid:04X}:{page:04X}:{usage:04X}"),
            product: String::new(),
        }
    }

    pub fn is(&self, vid: u16, pid: u16) -> bool {
        self.vid == vid && self.pid == pid
    }

    /// The path in lower case, for matching collection tags such as "col02".
    pub fn path_str(&self) -> String {
        self.path.to_lowercase()
    }

    /// Product string, or `fallback` when the device reports none.
    pub fn product_or(&self, fallback: &str) -> String {
        if self.product.is_empty() {
            fallback
        } else {
            &self.product
        }
        .to_string()
    }
}

/// The collections the registry assigned to a driver, with their report lengths.
pub fn colls(infos: &[DeviceInfo]) -> Vec<Coll> {
    infos
        .iter()
        .map(|i| {
            let (in_len, out_len, feat_len) = report_lengths(i.path()).unwrap_or_default();
            Coll {
                vid: i.vendor_id(),
                pid: i.product_id(),
                iface: i.interface_number(),
                page: i.usage_page(),
                usage: i.usage(),
                in_len,
                out_len,
                feat_len,
                path: i.path().to_string_lossy().into_owned(),
                product: i.product_string().unwrap_or_default().trim().to_string(),
            }
        })
        .collect()
}

/// Distinct (vid, pid) pairs among the collections, sorted (BatteryHub iterates a std::set).
pub fn products(colls: &[Coll]) -> Vec<(u16, u16)> {
    let mut out: Vec<(u16, u16)> = colls.iter().map(|c| (c.vid, c.pid)).collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// Distinct product ids of one vendor, sorted (BatteryHub's `ctx.Products`).
pub fn pids(colls: &[Coll], vid: u16) -> Vec<u16> {
    let mut out: Vec<u16> = colls
        .iter()
        .filter(|c| c.vid == vid)
        .map(|c| c.pid)
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// Collections of one product that match `pred`, in enumeration order (BatteryHub's `ctx.Find`).
pub fn find(colls: &[Coll], vid: u16, pid: u16, pred: impl Fn(&Coll) -> bool) -> Vec<&Coll> {
    colls
        .iter()
        .filter(|c| c.vid == vid && c.pid == pid && pred(c))
        .collect()
}

/// Device type from the product's top-level collections (mouse > gamepad > keyboard), else Other.
pub fn kind_from_colls(colls: &[Coll], vid: u16, pid: u16) -> Kind {
    let cols: Vec<(u16, u16)> = find(colls, vid, pid, |_| true)
        .iter()
        .map(|c| (c.page, c.usage))
        .collect();
    kind_from_collections(&cols)
}

/// As `kind_from_colls`, falling back to the name.
pub fn kind_of(colls: &[Coll], vid: u16, pid: u16, name: &str) -> Kind {
    match kind_from_colls(colls, vid, pid) {
        Kind::Other => guess_kind(name),
        k => k,
    }
}

/// An open collection. Every call returns at once (false / None) once the poll's deadline passed.
pub trait Dev: Send {
    /// Output report through the interrupt pipe (WriteFile).
    fn write(&mut self, data: &[u8]) -> bool;
    /// Output report through the control pipe (HidD_SetOutputReport), for collections without an
    /// interrupt OUT pipe.
    fn set_output(&mut self, data: &[u8]) -> bool;
    /// One input report, waiting up to `timeout`; None on timeout or error.
    fn read(&mut self, timeout: Duration) -> Option<Vec<u8>>;
    fn set_feature(&mut self, data: &[u8]) -> bool;
    /// Feature report `id`, the collection's full declared length, id first.
    fn get_feature(&mut self, id: u8) -> Option<Vec<u8>>;
    /// Feature report `id` read into a `len`-byte buffer, id first.
    fn get_feature_len(&mut self, id: u8, len: usize) -> Option<Vec<u8>>;
    /// Current input report `id` over the control pipe (HidD_GetInputReport), id first.
    fn get_input(&mut self, id: u8) -> Option<Vec<u8>>;
    /// Drops queued input reports.
    fn flush(&mut self);
    /// Waits, as [`Hid::sleep`], for code that holds only the device.
    fn sleep(&mut self, d: Duration);
}

/// Opens collections and keeps the clock, so tests run deadline loops without waiting.
pub trait Hid {
    fn open(&mut self, c: &Coll) -> Option<Box<dyn Dev>>;
    fn sleep(&mut self, d: Duration);
    /// Time since the poll started.
    fn now(&self) -> Duration;
}

/// Reads until `accept` takes a report; None when a read times out or `timeout` passes (BatteryHub's
/// usual "if (d.Read(r, left) < 0) return false" loop).
pub fn read_until(
    d: &mut dyn Dev,
    timeout: Duration,
    mut accept: impl FnMut(&[u8]) -> bool,
) -> Option<Vec<u8>> {
    let end = Instant::now() + timeout;
    loop {
        let left = left(end);
        if left.is_zero() {
            return None;
        }
        let r = d.read(left)?;
        if accept(&r) {
            return Some(r);
        }
    }
}

/// Writes `req` and returns the first input report `accept` takes within `timeout` (BatteryHub's
/// per-driver `Query` helpers).
pub fn query(
    d: &mut dyn Dev,
    req: &[u8],
    timeout: Duration,
    accept: impl FnMut(&[u8]) -> bool,
) -> Option<Vec<u8>> {
    if !d.write(req) {
        return None;
    }
    read_until(d, timeout, accept)
}

/// Time until `end`, zero once it has passed.
pub fn left(end: Instant) -> Duration {
    end.saturating_duration_since(Instant::now())
}

/// "XXXX", as BatteryHub's Hex4.
pub fn hex4(v: u16) -> String {
    format!("{v:04X}")
}

/// Byte sum, wrapping.
pub fn sum(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0u8, |a, b| a.wrapping_add(*b))
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

/// What one collection answered.
#[derive(Debug)]
pub enum Outcome {
    /// Nothing usable: try the next collection.
    NoAnswer,
    /// The receiver answered but its device is off or out of range (BatteryHub's
    /// `connected = false`): the device as it is listed when on, which `settle` marks `off` so it is
    /// hidden at once; no other collection of that product is asked.
    Off(Device),
    Found(Device),
}

impl Outcome {
    /// Adds the device to `out` (marked `off` for `Off`); true when the product is handled.
    pub fn settle(self, out: &mut Vec<Device>) -> bool {
        match self {
            Outcome::NoAnswer => false,
            Outcome::Off(mut d) => {
                d.off = true;
                out.push(d);
                true
            }
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

/// Runs a driver's poll over the real HID stack, within [`BUDGET`].
pub fn run(
    api: &HidApi,
    infos: &[DeviceInfo],
    poll: impl FnOnce(&mut dyn Hid, &[Coll]) -> Vec<Device>,
) -> Vec<Device> {
    run_mode(api, infos, false, poll)
}

/// As [`run`], but feature reports go out and come back at exactly the length the driver passes
/// (HidD_SetFeature / HidD_GetFeature on a zero-access handle), for devices whose collection's
/// longest feature report is not the one being sent. Such a collection only does feature reports.
pub fn run_exact_features(
    api: &HidApi,
    infos: &[DeviceInfo],
    poll: impl FnOnce(&mut dyn Hid, &[Coll]) -> Vec<Device>,
) -> Vec<Device> {
    run_mode(api, infos, true, poll)
}

fn run_mode(
    api: &HidApi,
    infos: &[DeviceInfo],
    exact: bool,
    poll: impl FnOnce(&mut dyn Hid, &[Coll]) -> Vec<Device>,
) -> Vec<Device> {
    let colls = colls(infos);
    let start = Instant::now();
    let mut hid = RealHid {
        api,
        start,
        deadline: start + BUDGET,
        exact,
    };
    POLL_END.with(|e| e.set(Some(hid.deadline)));
    let out = poll(&mut hid, &colls);
    POLL_END.with(|e| e.set(None));
    out
}

thread_local! {
    /// Deadline of the poll running on this thread. A device a driver keeps open across polls is
    /// bound by the poll that uses it, not by the one that opened it.
    static POLL_END: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
}

/// The running poll's deadline; outside a poll, now (every call fails at once).
fn poll_end() -> Instant {
    POLL_END.with(|e| e.get()).unwrap_or_else(Instant::now)
}

struct Handle(HANDLE);

// SAFETY: a Win32 file handle is not tied to the thread that opened it.
unsafe impl Send for Handle {}

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Opens a collection with zero access: enough for capabilities and feature reports, and it works on
/// mice and keyboards Windows holds exclusively.
fn open_raw(path: &CStr) -> Option<Handle> {
    let wide: Vec<u16> = path
        .to_string_lossy()
        .encode_utf16()
        .chain(Some(0))
        .collect();
    unsafe {
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
    .ok()
    .map(Handle)
}

/// (input, output, feature) report lengths of a collection, report id byte included.
fn report_lengths(path: &CStr) -> Option<(u16, u16, u16)> {
    let h = open_raw(path)?;
    let mut pp = PHIDP_PREPARSED_DATA::default();
    if !unsafe { HidD_GetPreparsedData(h.0, &mut pp) } {
        return None;
    }
    let mut caps = HIDP_CAPS::default();
    let ok = unsafe { HidP_GetCaps(pp, &mut caps) } == HIDP_STATUS_SUCCESS;
    unsafe {
        let _ = HidD_FreePreparsedData(pp);
    }
    ok.then_some((
        caps.InputReportByteLength,
        caps.OutputReportByteLength,
        caps.FeatureReportByteLength,
    ))
}

struct RealHid<'a> {
    api: &'a HidApi,
    start: Instant,
    deadline: Instant,
    exact: bool,
}

impl Hid for RealHid<'_> {
    fn open(&mut self, c: &Coll) -> Option<Box<dyn Dev>> {
        if Instant::now() >= self.deadline {
            return None;
        }
        let path = CString::new(c.path.clone()).ok()?;
        if self.exact {
            return Some(Box::new(ExactDev {
                h: open_raw(&path)?,
                feat_len: c.feat_len as usize,
                deadline: self.deadline,
            }));
        }
        // hidapi opens read/write and falls back to zero access (enough for feature reports).
        let dev = self.api.open_path(&path).ok()?;
        Some(Box::new(RealDev {
            dev,
            in_len: c.in_len as usize,
            out_len: c.out_len as usize,
            feat_len: c.feat_len as usize,
        }))
    }

    fn sleep(&mut self, d: Duration) {
        std::thread::sleep(d.min(left(self.deadline)));
    }

    fn now(&self) -> Duration {
        self.start.elapsed()
    }
}

/// `data` zero padded to `len` (Windows wants the declared report length).
fn padded(data: &[u8], len: usize) -> Vec<u8> {
    let mut buf = data.to_vec();
    if buf.len() < len {
        buf.resize(len, 0);
    }
    buf
}

/// hidapi-backed collection.
struct RealDev {
    dev: HidDevice,
    in_len: usize,
    out_len: usize,
    feat_len: usize,
}

impl RealDev {
    fn live(&self) -> bool {
        Instant::now() < poll_end()
    }
}

impl Dev for RealDev {
    fn write(&mut self, data: &[u8]) -> bool {
        self.live()
            && self.out_len > 0
            && !data.is_empty()
            && self.dev.write(&padded(data, self.out_len)).is_ok()
    }

    fn set_output(&mut self, data: &[u8]) -> bool {
        self.live()
            && self.out_len > 0
            && !data.is_empty()
            && self
                .dev
                .send_output_report(&padded(data, self.out_len))
                .is_ok()
    }

    fn read(&mut self, timeout: Duration) -> Option<Vec<u8>> {
        let ms = timeout.min(left(poll_end())).as_millis();
        if self.in_len == 0 || ms == 0 {
            return None;
        }
        let mut buf = vec![0u8; self.in_len.max(1100)];
        match self
            .dev
            .read_timeout(&mut buf, ms.min(i32::MAX as u128) as i32)
        {
            Ok(n) if n > 0 => {
                buf.truncate(n);
                Some(buf)
            }
            _ => None,
        }
    }

    fn set_feature(&mut self, data: &[u8]) -> bool {
        self.live()
            && self.feat_len > 0
            && !data.is_empty()
            && self
                .dev
                .send_feature_report(&padded(data, self.feat_len))
                .is_ok()
    }

    fn get_feature(&mut self, id: u8) -> Option<Vec<u8>> {
        if self.feat_len == 0 {
            return None;
        }
        self.get_feature_len(id, self.feat_len)
    }

    fn get_feature_len(&mut self, id: u8, len: usize) -> Option<Vec<u8>> {
        if !self.live() {
            return None;
        }
        let mut buf = vec![0u8; len.max(2)];
        buf[0] = id;
        self.dev.get_feature_report(&mut buf).ok()?;
        Some(buf)
    }

    fn get_input(&mut self, id: u8) -> Option<Vec<u8>> {
        if !self.live() || self.in_len == 0 {
            return None;
        }
        let mut buf = vec![0u8; self.in_len];
        buf[0] = id;
        self.dev.get_input_report(&mut buf).ok()?;
        Some(buf)
    }

    fn flush(&mut self) {
        let mut buf = [0u8; 128];
        while matches!(self.dev.read_timeout(&mut buf, 0), Ok(n) if n > 0) {}
    }

    fn sleep(&mut self, d: Duration) {
        std::thread::sleep(d.min(left(poll_end())));
    }
}

/// Feature reports at the exact length given, on a zero-access handle ([`run_exact_features`]).
struct ExactDev {
    h: Handle,
    feat_len: usize,
    deadline: Instant,
}

impl Dev for ExactDev {
    fn write(&mut self, _: &[u8]) -> bool {
        false
    }

    fn set_output(&mut self, _: &[u8]) -> bool {
        false
    }

    fn read(&mut self, _: Duration) -> Option<Vec<u8>> {
        None
    }

    fn set_feature(&mut self, data: &[u8]) -> bool {
        let mut buf = data.to_vec();
        Instant::now() < self.deadline
            && unsafe { HidD_SetFeature(self.h.0, buf.as_mut_ptr().cast(), buf.len() as u32) }
    }

    fn get_feature(&mut self, id: u8) -> Option<Vec<u8>> {
        self.get_feature_len(id, self.feat_len)
    }

    fn get_feature_len(&mut self, id: u8, len: usize) -> Option<Vec<u8>> {
        if Instant::now() >= self.deadline {
            return None;
        }
        let mut buf = vec![0u8; len.max(2)];
        buf[0] = id;
        unsafe { HidD_GetFeature(self.h.0, buf.as_mut_ptr().cast(), buf.len() as u32) }
            .then_some(buf)
    }

    fn get_input(&mut self, _: u8) -> Option<Vec<u8>> {
        None
    }

    fn flush(&mut self) {}

    fn sleep(&mut self, d: Duration) {
        std::thread::sleep(d.min(left(self.deadline)));
    }
}

/// Scripted collections for driver tests, like BatteryHub's simulated `HidDev`: no hardware is
/// touched. Every call is logged; one handler answers it. The default handler, [`fake::queued`],
/// answers from queues of scripted reports; a test's own handler can react to what was sent and
/// fall back to it. The clock is virtual: it advances on sleeps and on reads that time out.
#[cfg(test)]
pub mod fake {
    use super::*;
    use std::collections::{HashMap, VecDeque};
    use std::sync::{Arc, Mutex, MutexGuard};

    #[derive(Debug, Clone, PartialEq)]
    pub enum Op {
        Write(Vec<u8>),
        SetOutput(Vec<u8>),
        SetFeature(Vec<u8>),
        /// Report id and the buffer length asked for.
        GetFeature(u8, usize),
        GetInput(u8),
        Read(Duration),
        Flush,
    }

    impl Op {
        /// The bytes of a write, output or feature report sent; None for the other calls.
        pub fn sent(&self) -> Option<&[u8]> {
            match self {
                Op::Write(d) | Op::SetOutput(d) | Op::SetFeature(d) => Some(d),
                _ => None,
            }
        }
    }

    /// Answers one call: for a send, Some = accepted; for a read, the report.
    pub type Handler = Box<dyn FnMut(&mut State, &Coll, &Op) -> Option<Vec<u8>> + Send>;

    type Queues = HashMap<String, VecDeque<Vec<u8>>>;

    #[derive(Default)]
    pub struct State {
        /// Every device call, with the path of the collection it went to.
        pub log: Vec<(String, Op)>,
        /// Paths of the collections the driver tried to open, in order.
        pub opened: Vec<String>,
        pub clock: Duration,
        /// Time spent in sleeps.
        pub slept: Duration,
        pub refuse_open: bool,
        pub refuse_send: bool,
        /// A flush drops the queued input reports (otherwise it is only logged).
        pub flush_clears: bool,
        /// Feature replies shorter than the buffer asked for are zero padded, as Windows fills it.
        pub pad_features: bool,
        /// Reports `read` returns, by collection path ("" = any collection).
        pub input: Queues,
        /// Reports `get_input` returns, by path ("" = any). An empty report is a failed request.
        pub input_reports: Queues,
        /// Reports `get_feature` returns, by path ("" = any). An empty report is a failed request.
        pub feature: Queues,
    }

    fn pop(q: &mut Queues, path: &str) -> Option<Vec<u8>> {
        if let Some(r) = q.get_mut(path).and_then(VecDeque::pop_front) {
            return Some(r);
        }
        q.get_mut("").and_then(VecDeque::pop_front)
    }

    impl State {
        pub fn push_input(&mut self, path: &str, r: Vec<u8>) {
            self.input.entry(path.into()).or_default().push_back(r);
        }
        pub fn push_input_report(&mut self, path: &str, r: Vec<u8>) {
            self.input_reports
                .entry(path.into())
                .or_default()
                .push_back(r);
        }
        pub fn push_feature(&mut self, path: &str, r: Vec<u8>) {
            self.feature.entry(path.into()).or_default().push_back(r);
        }
        /// Logged calls `f` takes.
        pub fn count(&self, f: impl Fn(&Op) -> bool) -> usize {
            self.log.iter().filter(|(_, op)| f(op)).count()
        }
        fn data(&self, f: impl Fn(&Op) -> Option<&Vec<u8>>) -> Vec<Vec<u8>> {
            self.log
                .iter()
                .filter_map(|(_, op)| f(op).cloned())
                .collect()
        }
        /// Output reports written, in order.
        pub fn writes(&self) -> Vec<Vec<u8>> {
            self.data(|op| match op {
                Op::Write(d) => Some(d),
                _ => None,
            })
        }
        /// Output reports sent over the control pipe, in order.
        pub fn outputs(&self) -> Vec<Vec<u8>> {
            self.data(|op| match op {
                Op::SetOutput(d) => Some(d),
                _ => None,
            })
        }
        /// Feature reports sent, in order.
        pub fn features(&self) -> Vec<Vec<u8>> {
            self.data(|op| match op {
                Op::SetFeature(d) => Some(d),
                _ => None,
            })
        }
        pub fn sends(&self) -> usize {
            self.count(|op| op.sent().is_some())
        }
        pub fn reads(&self) -> usize {
            self.count(|op| matches!(op, Op::Read(_)))
        }
        pub fn feature_reads(&self) -> usize {
            self.count(|op| matches!(op, Op::GetFeature(..)))
        }
        pub fn input_reads(&self) -> usize {
            self.count(|op| matches!(op, Op::GetInput(_)))
        }
        pub fn flushes(&self) -> usize {
            self.count(|op| matches!(op, Op::Flush))
        }
    }

    /// The default answers: sends are accepted unless `refuse_send`; reads pop the queues.
    pub fn queued(s: &mut State, c: &Coll, op: &Op) -> Option<Vec<u8>> {
        match op {
            Op::Write(_) | Op::SetOutput(_) | Op::SetFeature(_) => (!s.refuse_send).then(Vec::new),
            Op::Read(_) => pop(&mut s.input, &c.path),
            Op::GetInput(_) => pop(&mut s.input_reports, &c.path).filter(|r| !r.is_empty()),
            Op::GetFeature(_, len) => {
                let mut r = pop(&mut s.feature, &c.path).filter(|r| !r.is_empty())?;
                if s.pad_features && r.len() < *len {
                    r.resize(*len, 0);
                }
                Some(r)
            }
            Op::Flush => {
                if s.flush_clears {
                    s.input.clear();
                }
                None
            }
        }
    }

    #[derive(Clone)]
    pub struct Fake {
        state: Arc<Mutex<State>>,
        handler: Arc<Mutex<Handler>>,
    }

    impl Default for Fake {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Fake {
        /// Answers from the queues only.
        pub fn new() -> Self {
            Self::with(queued)
        }

        /// Every call goes to `handler`; it may call [`queued`] for the default answer.
        pub fn with(
            handler: impl FnMut(&mut State, &Coll, &Op) -> Option<Vec<u8>> + Send + 'static,
        ) -> Self {
            Self {
                state: Arc::new(Mutex::new(State::default())),
                handler: Arc::new(Mutex::new(Box::new(handler))),
            }
        }

        /// After each send, queues `reply(data)` as input reports.
        pub fn replies(mut reply: impl FnMut(&[u8]) -> Vec<Vec<u8>> + Send + 'static) -> Self {
            Self::with(move |s, c, op| {
                let ok = queued(s, c, op);
                if let (Some(_), Some(d)) = (&ok, op.sent()) {
                    s.input.entry(String::new()).or_default().extend(reply(d));
                }
                ok
            })
        }

        /// The n-th send queues `per_send[n]` as input reports.
        pub fn per_send(per_send: Vec<Vec<Vec<u8>>>) -> Self {
            Self::with(move |s, c, op| {
                if op.sent().is_some() {
                    let n = s.sends() - 1;
                    let r = per_send.get(n).cloned().unwrap_or_default();
                    s.input.entry(String::new()).or_default().extend(r);
                }
                queued(s, c, op)
            })
        }

        pub fn s(&self) -> MutexGuard<'_, State> {
            self.state.lock().unwrap()
        }

        /// Queues input reports for any collection.
        pub fn queue(&self, reports: Vec<Vec<u8>>) -> &Self {
            self.s()
                .input
                .entry(String::new())
                .or_default()
                .extend(reports);
            self
        }

        /// Logged calls `f` takes.
        pub fn count(&self, f: impl Fn(&Op) -> bool) -> usize {
            self.s().count(f)
        }

        fn call(&self, c: &Coll, op: Op) -> Option<Vec<u8>> {
            let mut s = self.s();
            s.log.push((c.path.clone(), op.clone()));
            let r = (self.handler.lock().unwrap())(&mut s, c, &op);
            if let (None, Op::Read(t)) = (&r, &op) {
                s.clock += *t;
                drop(s);
                // An empty queue behaves like a short timeout, so wall-clock loops do not spin hot.
                std::thread::sleep((*t).min(Duration::from_millis(5)));
            }
            r
        }

        fn sleep(&self, d: Duration) {
            let mut s = self.s();
            s.clock += d;
            s.slept += d;
        }
    }

    struct FakeDev {
        coll: Coll,
        fake: Fake,
    }

    impl Dev for FakeDev {
        fn write(&mut self, data: &[u8]) -> bool {
            self.fake
                .call(&self.coll, Op::Write(data.to_vec()))
                .is_some()
        }
        fn set_output(&mut self, data: &[u8]) -> bool {
            self.fake
                .call(&self.coll, Op::SetOutput(data.to_vec()))
                .is_some()
        }
        fn read(&mut self, timeout: Duration) -> Option<Vec<u8>> {
            self.fake.call(&self.coll, Op::Read(timeout))
        }
        fn set_feature(&mut self, data: &[u8]) -> bool {
            self.fake
                .call(&self.coll, Op::SetFeature(data.to_vec()))
                .is_some()
        }
        fn get_feature(&mut self, id: u8) -> Option<Vec<u8>> {
            let len = self.coll.feat_len as usize;
            self.fake.call(&self.coll, Op::GetFeature(id, len))
        }
        fn get_feature_len(&mut self, id: u8, len: usize) -> Option<Vec<u8>> {
            self.fake.call(&self.coll, Op::GetFeature(id, len))
        }
        fn get_input(&mut self, id: u8) -> Option<Vec<u8>> {
            self.fake.call(&self.coll, Op::GetInput(id))
        }
        fn flush(&mut self) {
            self.fake.call(&self.coll, Op::Flush);
        }
        fn sleep(&mut self, d: Duration) {
            self.fake.sleep(d);
        }
    }

    impl Hid for Fake {
        fn open(&mut self, c: &Coll) -> Option<Box<dyn Dev>> {
            let mut s = self.s();
            s.opened.push(c.path.clone());
            if s.refuse_open {
                return None;
            }
            Some(Box::new(FakeDev {
                coll: c.clone(),
                fake: self.clone(),
            }))
        }
        fn sleep(&mut self, d: Duration) {
            Fake::sleep(self, d);
        }
        fn now(&self) -> Duration {
            self.s().clock
        }
    }

    /// A collection on USB interface `iface`, with a feature report length too.
    #[allow(clippy::too_many_arguments)]
    pub fn iface_coll(
        vid: u16,
        pid: u16,
        iface: i32,
        page: u16,
        usage: u16,
        in_len: u16,
        out_len: u16,
        feat_len: u16,
    ) -> Coll {
        Coll {
            iface,
            feat_len,
            ..Coll::new(vid, pid, page, usage, in_len, out_len)
        }
    }

    /// A collection whose input and output reports are both `len` bytes.
    pub fn coll(vid: u16, pid: u16, page: u16, usage: u16, len: u16) -> Coll {
        Coll::new(vid, pid, page, usage, len, len)
    }

    /// The same collection under another path (to tell collections apart).
    pub fn at(mut c: Coll, path: &str) -> Coll {
        c.path = path.into();
        c
    }

    // Fakes whose flush drops the queued input, like the keyboard tests of BatteryHub.

    pub fn script() -> Fake {
        let f = Fake::new();
        f.s().flush_clears = true;
        f
    }

    pub fn per_write(per_write: Vec<Vec<Vec<u8>>>) -> Fake {
        let f = Fake::per_send(per_write);
        f.s().flush_clears = true;
        f
    }

    pub fn responder(reply: impl FnMut(&[u8]) -> Vec<Vec<u8>> + Send + 'static) -> Fake {
        let f = Fake::replies(reply);
        f.s().flush_clears = true;
        f
    }

    /// As `responder`, with the path of the collection each report was sent to.
    pub fn path_responder(
        mut respond: impl FnMut(&str, &[u8]) -> Vec<Vec<u8>> + Send + 'static,
    ) -> Fake {
        let f = Fake::with(move |s, c, op| {
            let ok = queued(s, c, op);
            if let (Some(_), Some(w)) = (&ok, op.sent()) {
                for r in respond(&c.path, w) {
                    s.push_input("", r);
                }
            }
            ok
        });
        f.s().flush_clears = true;
        f
    }

    /// Feature reads answered by `feature(state, path, id, len)`; everything else from the queues.
    pub fn feature_script(
        mut feature: impl FnMut(&State, &str, u8, usize) -> Option<Vec<u8>> + Send + 'static,
    ) -> Fake {
        let f = Fake::with(move |s, c, op| match op {
            Op::GetFeature(id, len) => feature(s, &c.path, *id, *len),
            _ => queued(s, c, op),
        });
        f.s().flush_clears = true;
        f
    }

    /// Input and feature reports read, together (the keyboard tests count both).
    pub fn reads(f: &Fake) -> usize {
        let s = f.s();
        s.reads() + s.feature_reads()
    }

    mod tests {
        use super::*;

        #[test]
        fn queues_answer_by_path_then_any() {
            let mut f = Fake::new();
            let a = Coll::new(1, 2, 0xFF00, 1, 64, 64);
            let mut b = a.clone();
            b.path = "b".into();
            f.s().push_input("b", vec![2]);
            f.queue(vec![vec![1]]);
            let mut da = f.open(&a).unwrap();
            let mut db = f.open(&b).unwrap();
            assert_eq!(da.read(MAX_WAIT), Some(vec![1]));
            assert_eq!(db.read(MAX_WAIT), Some(vec![2]));
            assert_eq!(db.read(MAX_WAIT), None);
            assert_eq!(
                f.now(),
                MAX_WAIT,
                "a read that times out advances the clock"
            );
            assert_eq!(f.s().opened, [a.path.as_str(), "b"]);
        }

        #[test]
        fn per_send_and_refusals() {
            let mut f = Fake::per_send(vec![vec![vec![7]], vec![vec![8]]]);
            let c = Coll::new(1, 2, 0xFF00, 1, 64, 64);
            let mut d = f.open(&c).unwrap();
            assert!(d.write(&[1]) && d.set_feature(&[2]));
            assert_eq!(
                (d.read(MAX_WAIT), d.read(MAX_WAIT)),
                (Some(vec![7]), Some(vec![8]))
            );
            f.s().refuse_send = true;
            assert!(!d.write(&[3]));
            assert_eq!(f.s().writes(), [vec![1], vec![3]]);
            assert_eq!(f.s().features(), [vec![2]]);
            f.s().refuse_open = true;
            assert!(f.open(&c).is_none());
        }
    }
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

    #[test]
    fn crc16_check_value() {
        assert_eq!(crc16_xmodem(b"123456789"), 0x31C3);
    }

    #[test]
    fn identical_devices_get_numbered_keys() {
        let mut out = Vec::new();
        let k = |out: &Vec<Device>| stable_key(out, "astro:0B0B");
        out.push(Device::new(k(&out), "A", Kind::Headset, "x"));
        out.push(Device::new(k(&out), "A", Kind::Headset, "x"));
        assert_eq!(out[0].key, "astro:0B0B");
        assert_eq!(out[1].key, "astro:0B0B#2");
    }

    #[test]
    fn padding_reaches_the_declared_length_only() {
        assert_eq!(padded(&[1, 2], 4), [1, 2, 0, 0]);
        assert_eq!(padded(&[1, 2, 3], 2), [1, 2, 3]);
    }
}

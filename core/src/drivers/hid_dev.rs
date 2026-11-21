//! A testable view of HID collections and devices for the vendor drivers ported from BatteryHub.
//!
//! `Coll` is BatteryHub's `HidColl`: ids, USB interface, usage and the report lengths
//! (`HidP_GetCaps`), which its drivers use to pick the right channel before sending anything.
//! `Dev` and `Io` stand for its `HidDev` and `Sleep`, so a driver's exchanges run against scripted
//! replies in tests. Buffers follow BatteryHub's (and hidapi's) conventions: writes and feature
//! reports carry the report id in byte 0 (0 when unnumbered) and are zero-padded to the report
//! length; reads drop a leading 0 report id; `get_feature` returns the whole report, id first.

use std::ffi::{CStr, CString};
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

use crate::device::Kind;
use crate::drivers::generic_hid::kind_from_collections;

/// Time a driver built on this module spends per poll; stays under registry::DRIVER_TIMEOUT (5 s).
pub const BUDGET: Duration = Duration::from_millis(3800);

/// One HID collection.
#[derive(Debug, Clone, Default)]
pub struct Coll {
    pub vid: u16,
    pub pid: u16,
    /// USB interface (MI_xx in the path), -1 when unknown.
    pub iface: i32,
    pub page: u16,
    pub usage: u16,
    pub in_len: u16,
    pub out_len: u16,
    pub feat_len: u16,
    pub path: CString,
    pub product: String,
}

impl Coll {
    /// The path as text, for matching collection tags such as "col02" and for per-collection state.
    /// Never part of a device key: it changes with the USB port.
    pub fn path_str(&self) -> String {
        self.path.to_string_lossy().to_lowercase()
    }

    /// Product string, or `fallback` when the device reports none.
    pub fn product_or(&self, fallback: &str) -> String {
        let p = self.product.trim();
        if p.is_empty() { fallback } else { p }.to_string()
    }
}

/// Snapshots the collections assigned to a driver, with their report lengths.
pub fn colls(infos: &[DeviceInfo]) -> Vec<Coll> {
    infos
        .iter()
        .map(|i| {
            let (in_len, out_len, feat_len) = report_lengths(i.path());
            Coll {
                vid: i.vendor_id(),
                pid: i.product_id(),
                iface: i.interface_number(),
                page: i.usage_page(),
                usage: i.usage(),
                in_len,
                out_len,
                feat_len,
                path: i.path().to_owned(),
                product: i.product_string().unwrap_or_default().trim().to_string(),
            }
        })
        .collect()
}

/// (input, output, feature) report byte lengths, id byte included; zeros when unreadable.
fn report_lengths(path: &CStr) -> (u16, u16, u16) {
    let wide: Vec<u16> = path
        .to_string_lossy()
        .encode_utf16()
        .chain(Some(0))
        .collect();
    // Zero access: enough for the preparsed data, and it works on mice Windows holds exclusively.
    let Ok(h) = (unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        )
    }) else {
        return (0, 0, 0);
    };
    let mut out = (0, 0, 0);
    let mut pp = PHIDP_PREPARSED_DATA::default();
    if unsafe { HidD_GetPreparsedData(h, &mut pp) } {
        let mut caps = HIDP_CAPS::default();
        if unsafe { HidP_GetCaps(pp, &mut caps) } == HIDP_STATUS_SUCCESS {
            out = (
                caps.InputReportByteLength,
                caps.OutputReportByteLength,
                caps.FeatureReportByteLength,
            );
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

/// Collections of one product that match `pred`, in enumeration order (BatteryHub's ctx.Find).
pub fn find(colls: &[Coll], vid: u16, pid: u16, pred: impl Fn(&Coll) -> bool) -> Vec<&Coll> {
    colls
        .iter()
        .filter(|c| c.vid == vid && c.pid == pid && pred(c))
        .collect()
}

/// Distinct product ids of one vendor, sorted (BatteryHub's ctx.Products iterates a std::set).
pub fn products(colls: &[Coll], vid: u16) -> Vec<u16> {
    let mut pids: Vec<u16> = colls
        .iter()
        .filter(|c| c.vid == vid)
        .map(|c| c.pid)
        .collect();
    pids.sort_unstable();
    pids.dedup();
    pids
}

/// Device type from the product's top-level collections (mouse > gamepad > keyboard), else Other.
pub fn kind_of(colls: &[Coll], vid: u16, pid: u16) -> Kind {
    let pairs: Vec<(u16, u16)> = find(colls, vid, pid, |_| true)
        .iter()
        .map(|c| (c.page, c.usage))
        .collect();
    kind_from_collections(&pairs)
}

/// Milliseconds until `end`, 0 once it has passed.
pub fn ms_left(end: Instant) -> u64 {
    end.saturating_duration_since(Instant::now()).as_millis() as u64
}

/// "XXXX", as BatteryHub's Hex4.
pub fn hex4(v: u16) -> String {
    format!("{v:04X}")
}

/// An open HID collection.
pub trait Dev: Send {
    /// Output report through the interrupt pipe; false when it could not be sent.
    fn write(&mut self, data: &[u8]) -> bool;
    /// Output report through the control pipe (HidD_SetOutputReport).
    fn set_output(&mut self, data: &[u8]) -> bool;
    /// One input report, or None on timeout or error (BatteryHub's Read returning -1).
    fn read(&mut self, timeout_ms: u64) -> Option<Vec<u8>>;
    fn set_feature(&mut self, data: &[u8]) -> bool;
    /// Feature report `id`, the whole report (id first), or None when the request failed.
    fn get_feature(&mut self, id: u8) -> Option<Vec<u8>>;
    /// Drops queued input reports.
    fn flush(&mut self);
}

/// Opens collections and sleeps; faked in tests.
pub trait Io {
    fn open(&mut self, c: &Coll) -> Option<Box<dyn Dev>>;
    fn sleep(&mut self, ms: u64);
}

/// Reads until `accept` takes a report; None when a read times out or `end` passes (BatteryHub's
/// usual "if (d.Read(r, left) < 0) return false" loop).
pub fn read_until(
    d: &mut dyn Dev,
    end: Instant,
    mut accept: impl FnMut(&[u8]) -> bool,
) -> Option<Vec<u8>> {
    loop {
        let left = ms_left(end);
        if left == 0 {
            return None;
        }
        let r = d.read(left)?;
        if accept(&r) {
            return Some(r);
        }
    }
}

/// The real thing, through hidapi.
pub struct HidIo<'a>(pub &'a HidApi);

impl Io for HidIo<'_> {
    fn open(&mut self, c: &Coll) -> Option<Box<dyn Dev>> {
        // hidapi opens read/write and falls back to zero access (enough for feature reports).
        let dev = self.0.open_path(&c.path).ok()?;
        Some(Box::new(HidDev {
            dev,
            in_len: c.in_len as usize,
            out_len: c.out_len as usize,
            feat_len: c.feat_len as usize,
        }))
    }

    fn sleep(&mut self, ms: u64) {
        std::thread::sleep(Duration::from_millis(ms));
    }
}

struct HidDev {
    dev: HidDevice,
    in_len: usize,
    out_len: usize,
    feat_len: usize,
}

impl Dev for HidDev {
    fn write(&mut self, data: &[u8]) -> bool {
        self.out_len > 0 && !data.is_empty() && self.dev.write(data).is_ok()
    }

    fn set_output(&mut self, data: &[u8]) -> bool {
        self.out_len > 0 && !data.is_empty() && self.dev.send_output_report(data).is_ok()
    }

    fn read(&mut self, timeout_ms: u64) -> Option<Vec<u8>> {
        if self.in_len == 0 {
            return None;
        }
        let mut buf = vec![0u8; self.in_len.max(64) + 1];
        match self
            .dev
            .read_timeout(&mut buf, timeout_ms.min(i32::MAX as u64) as i32)
        {
            Ok(n) if n > 0 => {
                buf.truncate(n);
                Some(buf)
            }
            _ => None,
        }
    }

    fn set_feature(&mut self, data: &[u8]) -> bool {
        self.feat_len > 0 && !data.is_empty() && self.dev.send_feature_report(data).is_ok()
    }

    fn get_feature(&mut self, id: u8) -> Option<Vec<u8>> {
        if self.feat_len == 0 {
            return None;
        }
        let mut buf = vec![0u8; self.feat_len];
        buf[0] = id;
        self.dev.get_feature_report(&mut buf).ok()?;
        Some(buf)
    }

    fn flush(&mut self) {
        let mut buf = vec![0u8; self.in_len.max(64) + 1];
        while matches!(self.dev.read_timeout(&mut buf, 0), Ok(n) if n > 0) {}
    }
}

/// Scripted devices for driver tests.
#[cfg(test)]
pub mod fake {
    use super::*;
    use std::collections::{HashMap, VecDeque};
    use std::sync::{Arc, Mutex};

    /// What a fake device does when the driver sends something: queue input or feature replies.
    pub type Answer = Box<dyn FnMut(&mut State, &str, Sent, &[u8]) + Send>;

    #[derive(Debug, Clone, Copy, PartialEq)]
    pub enum Sent {
        Write,
        Output,
        Feature,
    }

    #[derive(Default)]
    pub struct State {
        pub opens: usize,
        pub refuse_open: bool,
        pub refuse_send: bool,
        /// Everything sent, by kind, in order.
        pub sent: Vec<(Sent, Vec<u8>)>,
        pub feature_reads: usize,
        /// Input reports by collection path ("" = any collection).
        pub input: HashMap<String, VecDeque<Vec<u8>>>,
        /// Feature replies by collection path ("" = any collection).
        pub feature: HashMap<String, VecDeque<Vec<u8>>>,
        pub answer: Option<Answer>,
        pub slept_ms: u64,
    }

    impl State {
        pub fn push_input(&mut self, path: &str, r: Vec<u8>) {
            self.input.entry(path.to_string()).or_default().push_back(r);
        }
        pub fn push_feature(&mut self, path: &str, r: Vec<u8>) {
            self.feature
                .entry(path.to_string())
                .or_default()
                .push_back(r);
        }
        fn pop(map: &mut HashMap<String, VecDeque<Vec<u8>>>, path: &str) -> Option<Vec<u8>> {
            if let Some(r) = map.get_mut(path).and_then(VecDeque::pop_front) {
                return Some(r);
            }
            map.get_mut("").and_then(VecDeque::pop_front)
        }
        pub fn sent_of(&self, kind: Sent) -> Vec<Vec<u8>> {
            self.sent
                .iter()
                .filter(|(k, _)| *k == kind)
                .map(|(_, d)| d.clone())
                .collect()
        }
    }

    #[derive(Clone, Default)]
    pub struct Fake(pub Arc<Mutex<State>>);

    impl Fake {
        pub fn new() -> Self {
            Self::default()
        }
        pub fn with(answer: impl FnMut(&mut State, &str, Sent, &[u8]) + Send + 'static) -> Self {
            let f = Self::new();
            f.s().answer = Some(Box::new(answer));
            f
        }
        pub fn s(&self) -> std::sync::MutexGuard<'_, State> {
            self.0.lock().unwrap()
        }
    }

    impl Io for Fake {
        fn open(&mut self, c: &Coll) -> Option<Box<dyn Dev>> {
            let mut s = self.s();
            s.opens += 1;
            if s.refuse_open {
                return None;
            }
            Some(Box::new(FakeDev {
                s: self.0.clone(),
                path: c.path.to_string_lossy().to_string(),
                feat_len: c.feat_len as usize,
            }))
        }
        fn sleep(&mut self, ms: u64) {
            self.s().slept_ms += ms;
        }
    }

    struct FakeDev {
        s: Arc<Mutex<State>>,
        path: String,
        feat_len: usize,
    }

    impl FakeDev {
        fn send(&mut self, kind: Sent, data: &[u8]) -> bool {
            let mut s = self.s.lock().unwrap();
            s.sent.push((kind, data.to_vec()));
            if s.refuse_send {
                return false;
            }
            if let Some(mut a) = s.answer.take() {
                a(&mut s, &self.path, kind, data);
                s.answer = Some(a);
            }
            true
        }
    }

    impl Dev for FakeDev {
        fn write(&mut self, data: &[u8]) -> bool {
            self.send(Sent::Write, data)
        }
        fn set_output(&mut self, data: &[u8]) -> bool {
            self.send(Sent::Output, data)
        }
        fn read(&mut self, timeout_ms: u64) -> Option<Vec<u8>> {
            let r = State::pop(&mut self.s.lock().unwrap().input, &self.path);
            if r.is_none() {
                // An empty queue behaves like a short timeout, so polling loops do not spin hot.
                std::thread::sleep(Duration::from_millis(timeout_ms.min(5)));
            }
            r
        }
        fn set_feature(&mut self, data: &[u8]) -> bool {
            self.send(Sent::Feature, data)
        }
        fn get_feature(&mut self, id: u8) -> Option<Vec<u8>> {
            let mut s = self.s.lock().unwrap();
            s.feature_reads += 1;
            let mut r = State::pop(&mut s.feature, &self.path)?;
            if r.is_empty() {
                return None; // a scripted failure
            }
            if r.len() < self.feat_len {
                r.resize(self.feat_len, 0);
            }
            let _ = id;
            Some(r)
        }
        fn flush(&mut self) {}
    }

    /// A collection for tests; the path is "vid:pid:page:usage" unless set afterwards.
    #[allow(clippy::too_many_arguments)]
    pub fn coll(
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
            vid,
            pid,
            iface,
            page,
            usage,
            in_len,
            out_len,
            feat_len,
            path: CString::new(format!("{vid:04X}:{pid:04X}:{page:04X}:{usage:04X}")).unwrap(),
            product: String::new(),
        }
    }

    pub fn with_path(mut c: Coll, path: &str) -> Coll {
        c.path = CString::new(path).unwrap();
        c
    }
}

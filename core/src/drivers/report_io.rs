//! Transport seam for the report-based vendor drivers ported from BatteryHub.
//!
//! BatteryHub's drivers pick collections by report length (`featLen() == 91`, `outLen() > 0`) and
//! talk through a small `HidDev`. This module gives the Rust ports the same view: a `Coll` with the
//! collection's report lengths, and `Hid` / `Dev` traits the driver logic runs against, so a whole
//! poll can be tested with scripted replies (as BatteryHub's tests do) and no hardware.
//!
//! Report buffers follow hidapi conventions, which match BatteryHub's: writes and feature reports
//! carry the report id in byte 0 (0 when unnumbered) and are zero-padded to the report length;
//! reads strip a leading 0 report id, so byte 0 is the report id only for numbered reports.

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

use crate::device::{Device, Kind, guess_kind};
use crate::drivers::generic_hid::kind_from_collections;
use crate::drivers::hid_util;

/// One HID collection, with the report lengths Windows declares for it (each includes the id byte).
#[derive(Debug, Clone, Default)]
pub struct Coll {
    pub vid: u16,
    pub pid: u16,
    pub page: u16,
    pub usage: u16,
    /// USB interface number (MI_xx in the path), -1 when unknown.
    pub iface: i32,
    pub input: u16,
    pub output: u16,
    pub feature: u16,
    pub path: String,
    pub product: String,
}

/// An open collection. Every call is bounded by the driver's time budget.
pub trait Dev {
    /// Output report through the interrupt pipe (WriteFile).
    fn write(&mut self, data: &[u8]) -> bool;
    /// Output report through the control pipe (HidD_SetOutputReport), for collections without an
    /// interrupt OUT pipe.
    fn set_output(&mut self, data: &[u8]) -> bool;
    /// One input report, or None on timeout / error.
    fn read(&mut self, timeout: Duration) -> Option<Vec<u8>>;
    fn set_feature(&mut self, data: &[u8]) -> bool;
    /// Feature report `id`, full declared length; byte 0 is the report id.
    fn get_feature(&mut self, id: u8) -> Option<Vec<u8>>;
    /// Drops queued input reports.
    fn flush(&mut self);
}

/// Opens collections and keeps the clock, so tests can run deadline loops without waiting.
pub trait Hid {
    fn open(&mut self, c: &Coll) -> Option<Box<dyn Dev>>;
    fn sleep(&mut self, d: Duration);
    /// Time since the poll started.
    fn now(&self) -> Duration;
}

/// Longest single wait for one reply; BatteryHub sometimes waits 1-1.5 s, but the registry gives a
/// whole driver 5 s.
pub const MAX_WAIT: Duration = Duration::from_millis(500);
/// Total time one driver spends per poll; stays under registry::DRIVER_TIMEOUT (5 s).
pub const POLL_BUDGET: Duration = Duration::from_secs(4);

/// Distinct (vid, pid) pairs among the collections, sorted (BatteryHub iterates a std::set).
pub fn products(colls: &[Coll]) -> Vec<(u16, u16)> {
    let mut out: Vec<(u16, u16)> = colls.iter().map(|c| (c.vid, c.pid)).collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// Collections of one product that match `pred`, in enumeration order.
pub fn find(colls: &[Coll], vid: u16, pid: u16, pred: impl Fn(&Coll) -> bool) -> Vec<&Coll> {
    colls
        .iter()
        .filter(|c| c.vid == vid && c.pid == pid && pred(c))
        .collect()
}

/// Device type from the product's top-level collections (mouse > gamepad > keyboard).
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

/// Waits for the first input report `accept` takes, until `timeout` (capped at MAX_WAIT) passes.
pub fn read_until(
    hid: &dyn Hid,
    dev: &mut dyn Dev,
    timeout: Duration,
    accept: impl Fn(&[u8]) -> bool,
) -> Option<Vec<u8>> {
    let end = hid.now() + timeout.min(MAX_WAIT);
    loop {
        let left = end.saturating_sub(hid.now());
        if left.is_zero() {
            return None;
        }
        let r = dev.read(left)?;
        if accept(&r) {
            return Some(r);
        }
    }
}

/// A device row with BatteryHub's fields.
pub fn device(key: impl Into<String>, name: impl Into<String>, kind: Kind, via: &str) -> Device {
    Device::new(key, name, kind, via)
}

/// Runs a driver's logic over the real HID stack, within POLL_BUDGET.
pub fn run(
    api: &HidApi,
    infos: &[DeviceInfo],
    logic: impl FnOnce(&mut dyn Hid, &[Coll]) -> Vec<Device>,
) -> Vec<Device> {
    let colls: Vec<Coll> = infos.iter().map(coll_of).collect();
    let start = Instant::now();
    let mut hid = RealHid {
        api,
        start,
        deadline: start + POLL_BUDGET,
    };
    logic(&mut hid, &colls)
}

fn coll_of(info: &DeviceInfo) -> Coll {
    let (input, output, feature) = report_lengths(info.path()).unwrap_or_default();
    Coll {
        vid: info.vendor_id(),
        pid: info.product_id(),
        page: info.usage_page(),
        usage: info.usage(),
        iface: info.interface_number(),
        input,
        output,
        feature,
        path: info.path().to_string_lossy().into_owned(),
        product: hid_util::product_name(info, ""),
    }
}

/// (input, output, feature) report byte lengths, read with a zero-access handle (works on mice and
/// keyboards Windows holds exclusively).
fn report_lengths(path: &CStr) -> Option<(u16, u16, u16)> {
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
    let mut caps = HIDP_CAPS::default();
    let ok = unsafe {
        let ok =
            HidD_GetPreparsedData(h, &mut pp) && HidP_GetCaps(pp, &mut caps) == HIDP_STATUS_SUCCESS;
        if pp != PHIDP_PREPARSED_DATA::default() {
            let _ = HidD_FreePreparsedData(pp);
        }
        let _ = CloseHandle(h);
        ok
    };
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
}

impl Hid for RealHid<'_> {
    fn open(&mut self, c: &Coll) -> Option<Box<dyn Dev>> {
        if Instant::now() >= self.deadline {
            return None;
        }
        let path = CString::new(c.path.clone()).ok()?;
        let dev = self.api.open_path(&path).ok()?;
        Some(Box::new(RealDev {
            dev,
            feature_len: c.feature as usize,
            deadline: self.deadline,
        }))
    }

    fn sleep(&mut self, d: Duration) {
        std::thread::sleep(d.min(self.deadline.saturating_duration_since(Instant::now())));
    }

    fn now(&self) -> Duration {
        self.start.elapsed()
    }
}

struct RealDev {
    dev: HidDevice,
    feature_len: usize,
    deadline: Instant,
}

impl RealDev {
    fn open(&self) -> bool {
        Instant::now() < self.deadline
    }
}

impl Dev for RealDev {
    fn write(&mut self, data: &[u8]) -> bool {
        self.open() && self.dev.write(data).is_ok()
    }

    fn set_output(&mut self, data: &[u8]) -> bool {
        self.open() && self.dev.send_output_report(data).is_ok()
    }

    fn read(&mut self, timeout: Duration) -> Option<Vec<u8>> {
        let left = self.deadline.saturating_duration_since(Instant::now());
        let ms = timeout.min(MAX_WAIT).min(left).as_millis() as i32;
        if ms <= 0 {
            return None;
        }
        let mut buf = [0u8; 1024];
        match self.dev.read_timeout(&mut buf, ms) {
            Ok(n) if n > 0 => Some(buf[..n].to_vec()),
            _ => None,
        }
    }

    fn set_feature(&mut self, data: &[u8]) -> bool {
        self.open() && self.dev.send_feature_report(data).is_ok()
    }

    fn get_feature(&mut self, id: u8) -> Option<Vec<u8>> {
        if !self.open() {
            return None;
        }
        hid_util::get_feature(&self.dev, id, self.feature_len)
    }

    fn flush(&mut self) {
        hid_util::flush(&self.dev);
    }
}

/// Scripted transport for driver tests: every call goes to one handler, with a virtual clock that
/// advances on sleeps and on reads that time out.
#[cfg(test)]
pub mod fake {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    #[derive(Debug, Clone, PartialEq)]
    pub enum Op {
        Write(Vec<u8>),
        SetOutput(Vec<u8>),
        SetFeature(Vec<u8>),
        GetFeature(u8),
        Read(Duration),
        Flush,
    }

    /// Writes answer Some(anything) for success; reads and GetFeature answer the report.
    pub type Handler = Box<dyn FnMut(&Coll, Op) -> Option<Vec<u8>>>;

    #[derive(Clone)]
    pub struct Fake {
        pub clock: Rc<Cell<Duration>>,
        pub opens: Rc<Cell<usize>>,
        pub open_ok: Rc<Cell<bool>>,
        /// Every operation, with the path of the collection it went to.
        pub log: Rc<RefCell<Vec<(String, Op)>>>,
        handler: Rc<RefCell<Handler>>,
    }

    impl Fake {
        pub fn new(handler: impl FnMut(&Coll, Op) -> Option<Vec<u8>> + 'static) -> Self {
            Self {
                clock: Rc::new(Cell::new(Duration::ZERO)),
                opens: Rc::new(Cell::new(0)),
                open_ok: Rc::new(Cell::new(true)),
                log: Rc::new(RefCell::new(Vec::new())),
                handler: Rc::new(RefCell::new(Box::new(handler))),
            }
        }

        /// Logged operations of one kind, e.g. `|op| matches!(op, Op::Write(_))`.
        pub fn count(&self, f: impl Fn(&Op) -> bool) -> usize {
            self.log.borrow().iter().filter(|(_, op)| f(op)).count()
        }
    }

    struct FakeDev {
        coll: Coll,
        fake: Fake,
    }

    impl FakeDev {
        fn call(&mut self, op: Op) -> Option<Vec<u8>> {
            self.fake
                .log
                .borrow_mut()
                .push((self.coll.path.clone(), op.clone()));
            let r = (self.fake.handler.borrow_mut())(&self.coll, op.clone());
            if let (None, Op::Read(t)) = (&r, op) {
                self.fake.clock.set(self.fake.clock.get() + t);
            }
            r
        }
    }

    impl Dev for FakeDev {
        fn write(&mut self, data: &[u8]) -> bool {
            self.call(Op::Write(data.to_vec())).is_some()
        }
        fn set_output(&mut self, data: &[u8]) -> bool {
            self.call(Op::SetOutput(data.to_vec())).is_some()
        }
        fn read(&mut self, timeout: Duration) -> Option<Vec<u8>> {
            assert!(!timeout.is_zero() && timeout <= MAX_WAIT, "{timeout:?}");
            self.call(Op::Read(timeout))
        }
        fn set_feature(&mut self, data: &[u8]) -> bool {
            self.call(Op::SetFeature(data.to_vec())).is_some()
        }
        fn get_feature(&mut self, id: u8) -> Option<Vec<u8>> {
            self.call(Op::GetFeature(id))
        }
        fn flush(&mut self) {
            self.call(Op::Flush);
        }
    }

    impl Hid for Fake {
        fn open(&mut self, c: &Coll) -> Option<Box<dyn Dev>> {
            self.opens.set(self.opens.get() + 1);
            if !self.open_ok.get() {
                return None;
            }
            Some(Box::new(FakeDev {
                coll: c.clone(),
                fake: self.clone(),
            }))
        }
        fn sleep(&mut self, d: Duration) {
            self.clock.set(self.clock.get() + d);
        }
        fn now(&self) -> Duration {
            self.clock.get()
        }
    }
}

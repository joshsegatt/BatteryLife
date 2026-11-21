//! HID plumbing for drivers ported from BatteryHub that match collections the way its `HidColl`
//! does (interface number and report lengths, which hidapi's `DeviceInfo` does not carry) and talk
//! through its `HidDev` calls (write, read with a timeout, feature reports).
//!
//! A driver is written against [`Port`] and an opener, so the whole poll (collection matching,
//! request bytes, reply parsing) runs in tests against [`fake::Script`] without hardware.
//! [`HidPort`] is the hidapi implementation; it stops answering once the poll's deadline has
//! passed, so a stalled device cannot push a driver past the registry's deadline.
//!
//! Buffers follow hidapi conventions, like BatteryHub's: `data[0]` of a write or feature report is
//! the report id (0 when the device does not number its reports); a read returns the report id as
//! the first byte for numbered reports and no id byte for unnumbered ones.
//!
//! Follows the HID layer of BatteryHub (src/hid.h, commit fd2267d),
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

/// Total time a driver spends per poll; stays under registry::DRIVER_TIMEOUT (5 s).
pub const BUDGET: Duration = Duration::from_secs(4);
/// How long a receiver device's last level stays visible while it sleeps.
pub const RECEIVER_GRACE: Duration = Duration::from_secs(30 * 60);
pub const RECEIVER: &str = "2.4 GHz receiver";
pub const CABLE: &str = "USB cable";

/// One HID collection, as BatteryHub's `HidColl` describes it.
#[derive(Clone, Debug, Default)]
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
    /// Product string, as the device reports it.
    pub product: String,
    pub path: CString,
}

impl Coll {
    pub fn is(&self, vid: u16, pid: u16) -> bool {
        self.vid == vid && self.pid == pid
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
                product: i.product_string().unwrap_or_default().to_string(),
                path: i.path().to_owned(),
            }
        })
        .collect()
}

struct Handle(HANDLE);

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

/// One open collection. Every call returns at once (false / None) once the poll's deadline passed.
pub trait Port {
    /// Writes an output report, zero padded to the collection's report length.
    fn write(&mut self, data: &[u8]) -> bool;
    /// Waits up to `timeout` for one input report. None on timeout or error.
    fn read(&mut self, timeout: Duration) -> Option<Vec<u8>>;
    /// Drops queued input reports.
    fn flush(&mut self) {}
    /// Sends a feature report, zero padded to the collection's feature report length.
    fn set_feature(&mut self, _data: &[u8]) -> bool {
        false
    }
    /// Feature report `id` read into a `len`-byte buffer (id first), as Windows fills it.
    fn get_feature(&mut self, _id: u8, _len: usize) -> Option<Vec<u8>> {
        None
    }
    /// Sleeps; drivers that wait for a device between requests call this so tests need not.
    fn pause(&mut self, d: Duration) {
        std::thread::sleep(d);
    }
}

/// Opens a collection for a driver.
pub type Opener<'a> = dyn FnMut(&Coll) -> Option<Box<dyn Port>> + 'a;

/// Reads until `accept` takes a report, `timeout` passes or a read fails (BatteryHub's usual loop).
pub fn wait_for(
    port: &mut dyn Port,
    timeout: Duration,
    mut accept: impl FnMut(&[u8]) -> bool,
) -> Option<Vec<u8>> {
    let end = Instant::now() + timeout;
    loop {
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return None;
        }
        let r = port.read(left)?;
        if accept(&r) {
            return Some(r);
        }
    }
}

/// Byte sum, wrapping.
pub fn sum(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0u8, |a, b| a.wrapping_add(*b))
}

/// hidapi-backed port.
pub struct HidPort {
    dev: HidDevice,
    deadline: Instant,
}

impl HidPort {
    fn left(&self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }
}

impl Port for HidPort {
    fn write(&mut self, data: &[u8]) -> bool {
        !self.left().is_zero() && self.dev.write(data).is_ok()
    }

    fn read(&mut self, timeout: Duration) -> Option<Vec<u8>> {
        let wait = timeout.min(self.left());
        if wait.is_zero() {
            return None;
        }
        let mut buf = vec![0u8; 1100];
        let ms = wait.as_millis().clamp(1, i32::MAX as u128) as i32;
        match self.dev.read_timeout(&mut buf, ms) {
            Ok(n) if n > 0 => {
                buf.truncate(n);
                Some(buf)
            }
            _ => None,
        }
    }

    fn flush(&mut self) {
        crate::drivers::hid_util::flush(&self.dev);
    }

    fn set_feature(&mut self, data: &[u8]) -> bool {
        !self.left().is_zero() && self.dev.send_feature_report(data).is_ok()
    }

    fn get_feature(&mut self, id: u8, len: usize) -> Option<Vec<u8>> {
        if self.left().is_zero() {
            return None;
        }
        let mut buf = vec![0u8; len.max(2)];
        buf[0] = id;
        self.dev.get_feature_report(&mut buf).ok()?;
        Some(buf)
    }

    fn pause(&mut self, d: Duration) {
        std::thread::sleep(d.min(self.left()));
    }
}

/// Opener over hidapi; ports stop answering at `deadline`. hidapi falls back to zero access when a
/// collection cannot be opened for reading and writing (feature reports still work then).
pub fn hid_opener(api: &HidApi, deadline: Instant) -> impl FnMut(&Coll) -> Option<Box<dyn Port>> {
    move |c: &Coll| {
        if Instant::now() >= deadline {
            return None;
        }
        let dev = api.open_path(&c.path).ok()?;
        Some(Box::new(HidPort { dev, deadline }) as Box<dyn Port>)
    }
}

/// Feature-only port with exact report sizes (HidD_SetFeature / HidD_GetFeature on a zero-access
/// handle), for devices whose collection's longest feature report is not the one being sent.
pub struct ExactFeaturePort {
    h: Handle,
    deadline: Instant,
}

impl ExactFeaturePort {
    pub fn open(c: &Coll, deadline: Instant) -> Option<Self> {
        if Instant::now() >= deadline {
            return None;
        }
        Some(Self {
            h: open_raw(&c.path)?,
            deadline,
        })
    }

    fn expired(&self) -> bool {
        Instant::now() >= self.deadline
    }
}

impl Port for ExactFeaturePort {
    fn write(&mut self, _: &[u8]) -> bool {
        false
    }

    fn read(&mut self, _: Duration) -> Option<Vec<u8>> {
        None
    }

    fn set_feature(&mut self, data: &[u8]) -> bool {
        let mut buf = data.to_vec();
        !self.expired()
            && unsafe { HidD_SetFeature(self.h.0, buf.as_mut_ptr().cast(), buf.len() as u32) }
    }

    fn get_feature(&mut self, id: u8, len: usize) -> Option<Vec<u8>> {
        if self.expired() {
            return None;
        }
        let mut buf = vec![0u8; len.max(2)];
        buf[0] = id;
        unsafe { HidD_GetFeature(self.h.0, buf.as_mut_ptr().cast(), buf.len() as u32) }
            .then_some(buf)
    }

    fn pause(&mut self, d: Duration) {
        std::thread::sleep(d.min(self.deadline.saturating_duration_since(Instant::now())));
    }
}

#[cfg(test)]
pub mod fake {
    //! Scripted transport for driver tests, like BatteryHub's simulated `HidDev`.

    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;
    use std::time::Duration;

    use super::{Coll, Port};

    type Respond = Box<dyn FnMut(&str, &[u8]) -> Vec<Vec<u8>>>;
    type Feature = Box<dyn FnMut(&str, u8, usize) -> Option<Vec<u8>>>;

    /// What a test device answers, and a log of what the driver did.
    #[derive(Default)]
    pub struct Script {
        /// Output reports written, in order.
        pub writes: Vec<Vec<u8>>,
        /// Feature reports sent, in order.
        pub features: Vec<Vec<u8>>,
        /// Paths of the collections opened, in order.
        pub opened: Vec<String>,
        pub reads: usize,
        /// Input reports waiting to be read.
        pub pending: VecDeque<Vec<u8>>,
        /// Input reports queued after each write (index = write number).
        pub per_write: Vec<Vec<Vec<u8>>>,
        /// Replies computed from a written output or feature report (path, data).
        pub respond: Option<Respond>,
        /// Feature report reads (path, id, len).
        pub feature: Option<Feature>,
        /// Flush keeps queued reports (BatteryHub tests whose Flush is a no-op).
        pub keep_on_flush: bool,
        pub fail_open: bool,
        pub fail_write: bool,
    }

    pub type Shared = Rc<RefCell<Script>>;

    pub fn script() -> Shared {
        Rc::new(RefCell::new(Script::default()))
    }

    /// Input reports queued per write: the first write gets `per_write[0]`, and so on.
    pub fn per_write(per_write: Vec<Vec<Vec<u8>>>) -> Shared {
        let s = script();
        s.borrow_mut().per_write = per_write;
        s
    }

    /// Replies computed from each written report.
    pub fn responder(f: impl FnMut(&[u8]) -> Vec<Vec<u8>> + 'static) -> Shared {
        let s = script();
        let mut f = f;
        s.borrow_mut().respond = Some(Box::new(move |_, d| f(d)));
        s
    }

    struct FakePort {
        s: Shared,
        path: String,
    }

    impl FakePort {
        fn queue_reply(&self, data: &[u8]) {
            let mut s = self.s.borrow_mut();
            let n = s.writes.len() + s.features.len() - 1;
            let mut queued: Vec<Vec<u8>> = s.per_write.get(n).cloned().unwrap_or_default();
            if let Some(mut f) = s.respond.take() {
                queued.extend(f(&self.path, data));
                s.respond = Some(f);
            }
            s.pending.extend(queued);
        }
    }

    impl Port for FakePort {
        fn write(&mut self, data: &[u8]) -> bool {
            self.s.borrow_mut().writes.push(data.to_vec());
            if self.s.borrow().fail_write {
                return false;
            }
            self.queue_reply(data);
            true
        }

        fn read(&mut self, _: Duration) -> Option<Vec<u8>> {
            let mut s = self.s.borrow_mut();
            s.reads += 1;
            s.pending.pop_front()
        }

        fn flush(&mut self) {
            let mut s = self.s.borrow_mut();
            if !s.keep_on_flush {
                s.pending.clear();
            }
        }

        fn set_feature(&mut self, data: &[u8]) -> bool {
            self.s.borrow_mut().features.push(data.to_vec());
            if self.s.borrow().fail_write {
                return false;
            }
            self.queue_reply(data);
            true
        }

        fn get_feature(&mut self, id: u8, len: usize) -> Option<Vec<u8>> {
            let mut s = self.s.borrow_mut();
            s.reads += 1;
            let mut f = s.feature.take()?;
            drop(s);
            let r = f(&self.path, id, len);
            self.s.borrow_mut().feature = Some(f);
            r
        }

        fn pause(&mut self, _: Duration) {}
    }

    /// Opener handing out ports on the script; logs every open.
    pub fn opener(s: &Shared) -> impl FnMut(&Coll) -> Option<Box<dyn Port>> + use<> {
        let s = s.clone();
        move |c: &Coll| {
            let path = c.path.to_string_lossy().to_string();
            s.borrow_mut().opened.push(path.clone());
            if s.borrow().fail_open {
                return None;
            }
            Some(Box::new(FakePort { s: s.clone(), path }) as Box<dyn Port>)
        }
    }

    /// A collection for tests.
    pub fn coll(vid: u16, pid: u16, page: u16, usage: u16, len: u16) -> Coll {
        Coll {
            vid,
            pid,
            iface: -1,
            page,
            usage,
            in_len: len,
            out_len: len,
            feat_len: 0,
            product: String::new(),
            path: std::ffi::CString::new(format!("{vid:04X}:{pid:04X}:{page:04X}:{usage:04X}"))
                .unwrap(),
        }
    }

    /// Same collection under another path (to tell ports apart in a test).
    pub fn at(mut c: Coll, path: &str) -> Coll {
        c.path = std::ffi::CString::new(path).unwrap();
        c
    }
}

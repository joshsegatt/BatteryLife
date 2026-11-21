//! Plantronics / Poly headsets (Voyager 8200 UC) behind the BT600 USB Bluetooth adapter
//! (047F:02EE / 02F7). Report 07 on the vendor collection FFA2:0003, 64 bytes:
//!   07 01 01 10 <len> <addr> 00 00 <op> <grp> <item> <value...>
//!   addr 00 = the adapter, 20 = the headset; replies carry 00 (adapter) / 02 (headset).
//! The adapter needs the attach nudge (output report 06 01) and a HELLO to the adapter and to the
//! headset; then GET group 0A item 1A: <level> <level count> <charging> <minutes hi> <minutes lo>,
//! percent = level * 100 / (count - 1). Only the battery is read; no setting is written.
//!
//! Ported from BatteryHub (src/drivers/plantronics.cpp, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0, which credits HaloBattery
//! (providers/plantronics.py) and HeadsetControl (plantronics_bt600.hpp).

use hidapi::{DeviceInfo, HidApi};

use crate::device::{Device, Kind};
use crate::drivers::hid::{self, Coll, Dev, Hid, find};

pub const VID: u16 = 0x047F;
pub const PIDS: &[u16] = &[0x02EE, 0x02F7];

const REPORT: u8 = 0x07;
const ADDR_DONGLE: u8 = 0x00;
const ADDR_HEADSET: u8 = 0x20;
const REPLY_DONGLE: u8 = 0x00;
const REPLY_HEADSET: u8 = 0x02;
const OP_HELLO: u8 = 0x01;
const OP_GET: u8 = 0x02;
const OP_HELLO_ACK: u8 = 0x08;
const OP_GET_REPLY: u8 = 0x03;
const GRP_HELLO: u8 = 0x01;
const ITEM_HELLO: u8 = 0x02;
const GRP_BATTERY: u8 = 0x0A;
const ITEM_BATTERY: u8 = 0x1A;

pub fn frame(op: u8, grp: u8, item: u8, addr: u8) -> [u8; 64] {
    let mut f = [0u8; 64];
    let len = if op == OP_GET { 0x06 } else { 0x07 };
    f[..11].copy_from_slice(&[
        REPORT, 0x01, 0x01, 0x10, len, addr, 0x00, 0x00, op, grp, item,
    ]);
    f
}

pub fn is_reply(r: &[u8], op: u8, grp: u8, item: u8, addr: u8) -> bool {
    r.len() > 10
        && r[..4] == [REPORT, 0x01, 0x01, 0x10]
        && r[5] == addr
        && r[8] == op
        && r[9] == grp
        && r[10] == item
}

/// (percent, charging) from the battery reply.
pub fn battery(r: &[u8]) -> Option<(u8, bool)> {
    if r.len() < 14 {
        return None;
    }
    let (level, count) = (u32::from(r[11]), u32::from(r[12]));
    if count < 2 || level >= count {
        return None; // not the adapter's own scale
    }
    Some(((level * 100 / (count - 1)).min(100) as u8, r[13] != 0))
}

/// Waits for the reply to (op, grp, item), skipping events; false after 16 reports or a silence.
fn wait(io: &mut dyn Dev, op: u8, grp: u8, item: u8, addr: u8) -> Option<Vec<u8>> {
    for _ in 0..16 {
        let r = io.read(hid::MAX_WAIT).filter(|r| !r.is_empty())?;
        if is_reply(&r, op, grp, item, addr) {
            return Some(r);
        }
    }
    None
}

fn read_battery(io: &mut dyn Dev) -> Option<(u8, bool)> {
    io.flush();
    io.write(&[0x06, 0x01]); // the adapter's attach nudge; harmless if it is already attached
    io.write(&frame(OP_HELLO, GRP_HELLO, ITEM_HELLO, ADDR_DONGLE));
    wait(io, OP_HELLO_ACK, GRP_HELLO, ITEM_HELLO, REPLY_DONGLE);
    // No answer: headset off, out of range or not paired with the adapter.
    if !io.write(&frame(OP_HELLO, GRP_HELLO, ITEM_HELLO, ADDR_HEADSET)) {
        return None;
    }
    wait(io, OP_HELLO_ACK, GRP_HELLO, ITEM_HELLO, REPLY_HEADSET)?;
    if !io.write(&frame(OP_GET, GRP_BATTERY, ITEM_BATTERY, ADDR_HEADSET)) {
        return None;
    }
    battery(&wait(
        io,
        OP_GET_REPLY,
        GRP_BATTERY,
        ITEM_BATTERY,
        REPLY_HEADSET,
    )?)
}

pub fn poll_colls(colls: &[Coll], hid: &mut dyn Hid) -> Vec<Device> {
    let mut out = Vec::new();
    for &pid in PIDS {
        for c in find(colls, VID, pid, |c| c.page == 0xFFA2 && c.usage == 0x0003) {
            let Some(mut io) = hid.open(c) else { continue };
            let Some((pct, charging)) = read_battery(io.as_mut()) else {
                continue;
            };
            let mut d = Device::new(
                format!("plantronics:{pid:04X}"),
                "Poly / Plantronics headset",
                Kind::Headset,
                "BT600 adapter",
            );
            d.percent = Some(pct);
            d.charging = charging;
            out.push(d);
            break;
        }
    }
    out
}

pub fn poll(api: &HidApi, infos: &[DeviceInfo]) -> Vec<Device> {
    hid::run(api, infos, |hid, colls| poll_colls(colls, hid))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::hid::fake::Fake;

    fn answer(op: u8, grp: u8, item: u8, addr: u8, value: &[u8]) -> Vec<u8> {
        let mut r = frame(op, grp, item, addr).to_vec();
        r[11..11 + value.len()].copy_from_slice(value);
        r
    }

    #[test]
    fn battery_scale() {
        let r = answer(
            OP_GET_REPLY,
            GRP_BATTERY,
            ITEM_BATTERY,
            REPLY_HEADSET,
            &[3, 5, 1],
        );
        assert_eq!(battery(&r), Some((75, true)));
        let r = answer(
            OP_GET_REPLY,
            GRP_BATTERY,
            ITEM_BATTERY,
            REPLY_HEADSET,
            &[5, 5, 0],
        );
        assert_eq!(battery(&r), None);
    }

    #[test]
    fn attach_hello_and_battery() {
        let f = Fake::replies(|w| {
            if w.len() < 11 {
                return vec![];
            }
            match (w[8], w[5]) {
                (OP_HELLO, ADDR_DONGLE) => vec![answer(OP_HELLO_ACK, 1, 2, REPLY_DONGLE, &[])],
                (OP_HELLO, ADDR_HEADSET) => vec![
                    vec![0x07, 0x02, 0, 0], // an unrelated event first
                    answer(OP_HELLO_ACK, 1, 2, REPLY_HEADSET, &[]),
                ],
                (OP_GET, _) => vec![answer(OP_GET_REPLY, 0x0A, 0x1A, REPLY_HEADSET, &[9, 11, 0])],
                _ => vec![],
            }
        });
        let c = Coll::new(VID, 0x02EE, 0xFFA2, 0x0003, 64, 64);
        let out = poll_colls(&[c], &mut f.clone());
        assert_eq!(out[0].key, "plantronics:02EE");
        assert_eq!((out[0].percent, out[0].charging), (Some(90), false));
        assert_eq!(f.s().writes()[0], vec![0x06, 0x01]);
    }

    #[test]
    fn headset_not_paired_lists_nothing() {
        let f = Fake::new();
        let c = Coll::new(VID, 0x02F7, 0xFFA2, 0x0003, 64, 64);
        assert!(poll_colls(&[c], &mut f.clone()).is_empty());
    }
}

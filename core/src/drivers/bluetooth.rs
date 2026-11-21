//! Bluetooth fallback: the level Windows itself keeps for paired devices.
//!
//! Windows reads the battery over HFP (classic) or the GATT Battery Service (LE) and stores it in a
//! device property, the same value Settings > Bluetooth & devices shows. Any vendor, no device I/O.
//!
//! Ported from BatteryHub (src/drivers/generic.cpp, PollBluetooth, commit fd2267d),
//! https://github.com/krisgfx9/Wireless-Device-Battery-Monitor, GPL-3.0. Fast Pair left/right/case
//! split is not ported yet.

use std::collections::HashMap;

use windows::Win32::Devices::DeviceAndDriverInstallation::{
    DIGCF_ALLCLASSES, DIGCF_PRESENT, HDEVINFO, SP_DEVICE_INTERFACE_DATA, SP_DEVINFO_DATA,
    SetupDiCreateDeviceInfoList, SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo,
    SetupDiGetClassDevsW, SetupDiGetDeviceInstanceIdW, SetupDiGetDeviceInterfaceDetailW,
    SetupDiGetDevicePropertyW, SetupDiOpenDeviceInterfaceW,
};
use windows::Win32::Devices::Properties::{
    DEVPKEY_Device_ContainerId, DEVPKEY_Device_DeviceDesc, DEVPKEY_Device_FriendlyName,
    DEVPKEY_DeviceContainer_Category, DEVPROP_TYPE_BOOLEAN, DEVPROP_TYPE_BYTE, DEVPROP_TYPE_GUID,
    DEVPROP_TYPE_STRING, DEVPROP_TYPE_STRING_LIST, DEVPROP_TYPE_UINT32, DEVPROPTYPE,
};
use windows::Win32::Foundation::DEVPROPKEY;
use windows::core::{GUID, PCWSTR};

use crate::device::{Device, Kind};

const BATTERY: DEVPROPKEY = DEVPROPKEY {
    fmtid: GUID::from_u128(0x104EA319_6EE2_4701_BD47_8DDBF425BBE5),
    pid: 2,
};
const IS_CONNECTED: DEVPROPKEY = DEVPROPKEY {
    fmtid: GUID::from_u128(0x83DA6326_97A6_4088_9453_A1923F573B29),
    pid: 15,
};
const CLASS_OF_DEVICE: DEVPROPKEY = DEVPROPKEY {
    fmtid: GUID::from_u128(0x2BD67D8B_8BEB_48D5_87E0_6CDA3428040A),
    pid: 10,
};

/// Device kind from a classic Bluetooth Class of Device.
pub fn kind_from_class(cod: u32) -> Kind {
    let (major, minor) = ((cod >> 8) & 0x1F, (cod >> 2) & 0x3F);
    match major {
        0x02 => Kind::Phone,
        0x04 => Kind::Headset,
        0x05 if matches!(minor & 0x0F, 0x01 | 0x02) => Kind::Gamepad,
        0x05 if minor >> 4 == 1 => Kind::Keyboard,
        0x05 if minor >> 4 == 2 => Kind::Mouse,
        _ => Kind::Other,
    }
}

/// Device kind from the category Windows files a (usually LE) device under.
pub fn kind_from_category(c: &str) -> Kind {
    match c {
        _ if c == "Communication.Phone" || c.starts_with("Communication.Phone.") => Kind::Phone,
        "Input.Keyboard" => Kind::Keyboard,
        "Input.Mouse" => Kind::Mouse,
        "Input.Gaming" => Kind::Gamepad,
        _ if c.starts_with("Communication.Headset") || c == "Audio.Headphones" => Kind::Headset,
        _ => Kind::Other,
    }
}

/// "Buds Hands-Free AG" -> "Buds": service nodes carry the level but a suffixed name.
pub fn strip_service_suffix(n: &str) -> &str {
    for suf in [
        " Hands-Free AG",
        " Hands-Free",
        " Avrcp Transport",
        " Stereo",
        " Audio Gateway",
    ] {
        if let Some(base) = n.strip_suffix(suf)
            && !base.is_empty()
        {
            return base;
        }
    }
    n
}

fn prop(
    set: HDEVINFO,
    dd: &SP_DEVINFO_DATA,
    key: &DEVPROPKEY,
    buf: &mut [u8],
) -> Option<DEVPROPTYPE> {
    let mut t = DEVPROPTYPE::default();
    unsafe { SetupDiGetDevicePropertyW(set, dd, key, &mut t, Some(buf), None, 0) }.ok()?;
    Some(t)
}

fn string_prop(set: HDEVINFO, dd: &SP_DEVINFO_DATA, key: &DEVPROPKEY) -> String {
    let mut buf = [0u8; 512];
    match prop(set, dd, key, &mut buf) {
        // A string list holds NUL-separated entries; the first one is the useful one.
        Some(t) if t == DEVPROP_TYPE_STRING || t == DEVPROP_TYPE_STRING_LIST => {
            let wide: Vec<u16> = buf
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            let end = wide.iter().position(|c| *c == 0).unwrap_or(wide.len());
            String::from_utf16_lossy(&wide[..end])
        }
        _ => String::new(),
    }
}

fn guid_prop(set: HDEVINFO, dd: &SP_DEVINFO_DATA, key: &DEVPROPKEY) -> Option<String> {
    let mut buf = [0u8; 16];
    match prop(set, dd, key, &mut buf) {
        // The property buffer holds a GUID struct as laid out in memory.
        Some(t) if t == DEVPROP_TYPE_GUID => Some(format!("{:?}", guid_from_mem(&buf))),
        _ => None,
    }
}

fn guid_from_mem(b: &[u8; 16]) -> GUID {
    GUID {
        data1: u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
        data2: u16::from_le_bytes([b[4], b[5]]),
        data3: u16::from_le_bytes([b[6], b[7]]),
        data4: [b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]],
    }
}

struct Level {
    pct: u8,
    name: String,
    connected: bool,
}

/// Every connected Bluetooth device for which Windows has a battery level.
pub fn poll() -> Vec<Device> {
    let Ok(set) = (unsafe {
        SetupDiGetClassDevsW(None, PCWSTR::null(), None, DIGCF_ALLCLASSES | DIGCF_PRESENT)
    }) else {
        return Vec::new();
    };
    let mut names: HashMap<String, String> = HashMap::new();
    let mut classes: HashMap<String, u32> = HashMap::new();
    let mut categories: HashMap<String, String> = HashMap::new();
    let mut connected: HashMap<String, bool> = HashMap::new();
    let mut levels: HashMap<String, Level> = HashMap::new();

    let mut dd = SP_DEVINFO_DATA {
        cbSize: size_of::<SP_DEVINFO_DATA>() as u32,
        ..Default::default()
    };
    let mut i = 0;
    while unsafe { SetupDiEnumDeviceInfo(set, i, &mut dd) }.is_ok() {
        i += 1;
        let mut inst = [0u16; 512];
        if unsafe { SetupDiGetDeviceInstanceIdW(set, &dd, Some(&mut inst), None) }.is_err() {
            continue;
        }
        let end = inst.iter().position(|c| *c == 0).unwrap_or(inst.len());
        let inst = String::from_utf16_lossy(&inst[..end]);
        if !(inst.starts_with("BTHENUM\\")
            || inst.starts_with("BTHLE\\")
            || inst.starts_with("BTHLEDEVICE\\"))
        {
            continue;
        }
        let cid = guid_prop(set, &dd, &DEVPKEY_Device_ContainerId).unwrap_or_else(|| inst.clone());
        let mut name = string_prop(set, &dd, &DEVPKEY_Device_FriendlyName);
        if name.is_empty() {
            name = string_prop(set, &dd, &DEVPKEY_Device_DeviceDesc);
        }
        let mut b = [0u8; 4];
        let is_connected = match prop(set, &dd, &IS_CONNECTED, &mut b[..1]) {
            Some(t) if t == DEVPROP_TYPE_BOOLEAN => Some(b[0] != 0),
            _ => None,
        };

        // The main device node (not a service node) names the device and says if it is connected.
        if inst.starts_with("BTHENUM\\DEV_") || inst.starts_with("BTHLE\\DEV_") {
            names.insert(cid.clone(), name.clone());
            let mut cod = [0u8; 4];
            if let Some(t) = prop(set, &dd, &CLASS_OF_DEVICE, &mut cod)
                && t == DEVPROP_TYPE_UINT32
            {
                classes.insert(cid.clone(), u32::from_le_bytes(cod));
            }
            let cat = string_prop(set, &dd, &DEVPKEY_DeviceContainer_Category);
            if !cat.is_empty() {
                categories.insert(cid.clone(), cat);
            }
            if let Some(c) = is_connected {
                // Classic and LE nodes can share a container: connected if either transport is.
                *connected.entry(cid.clone()).or_default() |= c;
            }
        }

        let mut pct = [0u8; 1];
        match prop(set, &dd, &BATTERY, &mut pct) {
            Some(t) if t == DEVPROP_TYPE_BYTE && pct[0] <= 100 => {}
            _ => continue,
        }
        let live = is_connected.unwrap_or(true);
        // Prefer a live node's level over a stale one in the same container.
        if levels.get(&cid).is_some_and(|l| l.connected && !live) {
            continue;
        }
        levels.insert(
            cid,
            Level {
                pct: pct[0],
                name: strip_service_suffix(&name).to_string(),
                connected: live,
            },
        );
    }
    unsafe {
        let _ = SetupDiDestroyDeviceInfoList(set);
    }

    let mut out = Vec::new();
    for (cid, l) in levels {
        // The main node decides; headphones keep the level on a Hands-Free node that reads as
        // disconnected while audio runs over A2DP.
        if !connected.get(&cid).copied().unwrap_or(l.connected) {
            continue;
        }
        let name = names
            .get(&cid)
            .filter(|n| !n.is_empty())
            .cloned()
            .unwrap_or(l.name);
        let name = if name.is_empty() {
            "Bluetooth device".to_string()
        } else {
            name
        };
        let mut kind = classes
            .get(&cid)
            .map_or(Kind::Other, |c| kind_from_class(*c));
        if kind == Kind::Other {
            kind = categories
                .get(&cid)
                .map_or(Kind::Other, |c| kind_from_category(c));
        }
        if kind == Kind::Other {
            kind = crate::device::guess_kind(&name);
        }
        let mut d = Device::new(format!("bt:{cid}"), name, kind, "Bluetooth");
        d.percent = Some(l.pct);
        out.push(d);
    }
    out.sort_by(|a, b| a.key.cmp(&b.key));
    out
}

/// Container id ("{...}" form, as in the "bt:" keys) of the physical device a HID path belongs to.
/// Lets the poller drop a Bluetooth entry that a vendor driver already reports over HID.
pub fn container_of(path: &std::ffi::CStr) -> Option<String> {
    let wide: Vec<u16> = path
        .to_string_lossy()
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let set = unsafe { SetupDiCreateDeviceInfoList(None, None) }.ok()?;
    let mut ifd = SP_DEVICE_INTERFACE_DATA {
        cbSize: size_of::<SP_DEVICE_INTERFACE_DATA>() as u32,
        ..Default::default()
    };
    let mut dd = SP_DEVINFO_DATA {
        cbSize: size_of::<SP_DEVINFO_DATA>() as u32,
        ..Default::default()
    };
    let found = unsafe {
        SetupDiOpenDeviceInterfaceW(set, PCWSTR(wide.as_ptr()), 0, Some(&mut ifd)).is_ok() && {
            // Fails with "insufficient buffer" by design; it still fills in the device node.
            let _ = SetupDiGetDeviceInterfaceDetailW(set, &ifd, None, 0, None, Some(&mut dd));
            dd.DevInst != 0
        }
    };
    let cid = if found {
        guid_prop(set, &dd, &DEVPKEY_Device_ContainerId)
    } else {
        None
    };
    unsafe {
        let _ = SetupDiDestroyDeviceInfoList(set);
    }
    cid
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_of_device_maps_kinds() {
        assert_eq!(kind_from_class(0x0200), Kind::Phone);
        assert_eq!(kind_from_class(0x0204), Kind::Phone);
        assert_eq!(kind_from_class(0x0400), Kind::Headset);
        assert_eq!(kind_from_class(0x0540), Kind::Keyboard);
        assert_eq!(kind_from_class(0x0580), Kind::Mouse);
        assert_eq!(kind_from_class(0x0504), Kind::Gamepad);
        assert_eq!(kind_from_class(0x0000), Kind::Other);
    }

    #[test]
    fn category_maps_le_devices() {
        assert_eq!(kind_from_category("Input.Mouse"), Kind::Mouse);
        assert_eq!(kind_from_category("Communication.Phone.Smart"), Kind::Phone);
        assert_eq!(kind_from_category("Audio.Headphones"), Kind::Headset);
        assert_eq!(kind_from_category("Display.Monitor"), Kind::Other);
    }

    #[test]
    fn strips_service_suffixes() {
        assert_eq!(
            strip_service_suffix("Galaxy Buds2 Hands-Free AG"),
            "Galaxy Buds2"
        );
        assert_eq!(strip_service_suffix("WH-1000XM5 Stereo"), "WH-1000XM5");
        assert_eq!(strip_service_suffix("Stereo"), "Stereo");
        assert_eq!(strip_service_suffix("MX Master 3S"), "MX Master 3S");
    }

    #[test]
    fn guid_reads_from_property_memory() {
        let g = GUID::from_u128(0x104EA319_6EE2_4701_BD47_8DDBF425BBE5);
        let mut mem = [0u8; 16];
        mem[..4].copy_from_slice(&g.data1.to_le_bytes());
        mem[4..6].copy_from_slice(&g.data2.to_le_bytes());
        mem[6..8].copy_from_slice(&g.data3.to_le_bytes());
        mem[8..].copy_from_slice(&g.data4);
        assert_eq!(guid_from_mem(&mem), g);
    }
}

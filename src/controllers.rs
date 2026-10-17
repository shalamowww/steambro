//! Which kinds of game controllers are connected, for the settings window's checks on the Steam
//! Input bridge.

use serde::Serialize;
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    CM_DEVNODE_STATUS_FLAGS, CM_Get_DevNode_Status, CM_Get_Parent, CM_PROB, CR_SUCCESS, DIGCF_ALLCLASSES, DIGCF_PRESENT, DN_STARTED,
    SP_DEVINFO_DATA, SPDRP_CLASS, SPDRP_COMPATIBLEIDS, SPDRP_HARDWAREID, SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo,
    SetupDiGetClassDevsW,
};

use crate::driver::property_strings as strings;
use windows::core::PCWSTR;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Xbox,
    Steam,
    PlayStation,
    Nintendo,
    Other,
}

/// The kinds of controllers connected now. Virtual controllers on the ViGEm bus (the bridge's own)
/// don't count.
pub fn connected() -> Vec<Kind> {
    let virtual_parents = crate::driver::bus_instance_ids();
    let mut kinds = Vec::new();
    unsafe {
        let Ok(set) = SetupDiGetClassDevsW(None, PCWSTR::null(), None, DIGCF_PRESENT | DIGCF_ALLCLASSES) else { return kinds };
        let mut data = SP_DEVINFO_DATA { cbSize: size_of::<SP_DEVINFO_DATA>() as u32, ..Default::default() };
        let mut index = 0;
        while SetupDiEnumDeviceInfo(set, index, &mut data).is_ok() {
            index += 1;
            if !started(data.DevInst) || virtual_parents.iter().any(|p| parent_id(data.DevInst).is_some_and(|id| id.eq_ignore_ascii_case(p))) {
                continue;
            }
            let ids = strings(set, &data, SPDRP_HARDWAREID);
            let Some(vendor) = ids.iter().find_map(|id| vendor_id(id)) else { continue };
            let class = strings(set, &data, SPDRP_CLASS).join("").to_lowercase();
            let game = strings(set, &data, SPDRP_COMPATIBLEIDS).iter().any(|id| id.eq_ignore_ascii_case("HID_DEVICE_SYSTEM_GAME"));
            if let Some(kind) = classify(vendor, &class, game, &ids)
                && !kinds.contains(&kind)
            {
                kinds.push(kind);
            }
        }
        let _ = SetupDiDestroyDeviceInfoList(set);
    }
    kinds.sort();
    kinds
}

/// The kinds of controllers Steam lists in `ControllerTypesUsed`, like `controller_xboxone`.
pub fn seen_by_steam(types: &str) -> Vec<Kind> {
    let mut kinds: Vec<Kind> = types
        .split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(|t| match t {
            t if t.contains("xbox") => Kind::Xbox,
            t if t.contains("ps3") || t.contains("ps4") || t.contains("ps5") => Kind::PlayStation,
            t if t.contains("switch") => Kind::Nintendo,
            t if t.contains("neptune") || t.contains("triton") || t.contains("steamcontroller") => Kind::Steam,
            _ => Kind::Other,
        })
        .collect();
    kinds.sort();
    kinds.dedup();
    kinds
}

fn classify(vendor: u16, class: &str, game: bool, ids: &[String]) -> Option<Kind> {
    match vendor {
        0x28DE => Some(Kind::Steam),
        0x054C if game => Some(Kind::PlayStation),
        0x057E if game => Some(Kind::Nintendo),
        // Microsoft also makes keyboards and mice; Xbox controllers have their own device classes,
        // or XInput HID IDs (`IG_`) over Bluetooth.
        0x045E if class.starts_with("xbox") || class.starts_with("xna") || ids.iter().any(|id| id.to_uppercase().contains("IG_")) => Some(Kind::Xbox),
        0x045E => None,
        _ if game => Some(Kind::Other),
        _ => None,
    }
}

/// The vendor ID in a hardware ID: `VID_045E` (USB) or `VID&02045E` (Bluetooth).
fn vendor_id(id: &str) -> Option<u16> {
    let upper = id.to_uppercase();
    let rest = if let Some(i) = upper.find("VID_") {
        &upper[i + 4..]
    } else {
        let i = upper.find("VID&")?;
        upper.get(i + 6..)?
    };
    u16::from_str_radix(rest.get(..4)?, 16).ok()
}

fn started(devinst: u32) -> bool {
    let mut status = CM_DEVNODE_STATUS_FLAGS(0);
    let mut problem = CM_PROB(0);
    unsafe { CM_Get_DevNode_Status(&mut status, &mut problem, devinst, 0) == CR_SUCCESS && status.0 & DN_STARTED.0 != 0 }
}

fn parent_id(devinst: u32) -> Option<String> {
    let mut parent = 0u32;
    if unsafe { CM_Get_Parent(&mut parent, devinst, 0) } != CR_SUCCESS {
        return None;
    }
    crate::driver::instance_id(parent)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vendor_ids_from_usb_and_bluetooth_ids() {
        assert_eq!(vendor_id(r"USB\VID_28DE&PID_1304&MI_00"), Some(0x28DE));
        assert_eq!(vendor_id(r"HID\{00001812-0000-1000-8000-00805f9b34fb}_VID&02045e_PID&0b13"), Some(0x045E));
        assert_eq!(vendor_id(r"ACPI\PNP0303"), None);
    }

    #[test]
    fn steam_controller_types() {
        assert_eq!(seen_by_steam("controller_xboxone,controller_neptune,"), [Kind::Xbox, Kind::Steam]);
        assert_eq!(seen_by_steam("controller_ps5,controller_switch_pro,controller_generic"), [Kind::PlayStation, Kind::Nintendo, Kind::Other]);
    }

    /// Lists this PC's controllers; it can't know what's plugged in, so it only has to run.
    #[test]
    fn connected_controllers_run() {
        let kinds = connected();
        assert!(kinds.windows(2).all(|w| w[0] < w[1]), "{kinds:?}");
    }
}

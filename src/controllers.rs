//! Game controllers: which kind a controller is, which kinds are connected, which the Steam Input
//! bridge copies, and whether Steam Input is on for each kind. Every part of steambro that tells
//! controllers apart asks this module.

use serde::Serialize;
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    CM_DEVNODE_STATUS_FLAGS, CM_Get_DevNode_Status, CM_Get_Parent, CM_PROB, CR_SUCCESS, DIGCF_ALLCLASSES, DIGCF_PRESENT, DN_STARTED,
    SP_DEVINFO_DATA, SPDRP_CLASS, SPDRP_COMPATIBLEIDS, SPDRP_HARDWAREID, SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo,
    SetupDiGetClassDevsW,
};

use crate::driver::property_strings as strings;
use crate::steam::InputSettings;
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

impl Kind {
    /// The kind of a controller, from its USB vendor ID. Only for a device known to be a
    /// controller: Microsoft and Sony also make keyboards, mice, and headsets.
    pub fn of_vendor(vendor: u16) -> Self {
        match vendor {
            0x28DE => Self::Steam,
            0x054C => Self::PlayStation,
            0x057E => Self::Nintendo,
            0x045E => Self::Xbox,
            _ => Self::Other,
        }
    }

    /// Whether the Steam Input bridge copies a controller of this kind for a game that Steam
    /// doesn't see. Not an Xbox controller: a real one reaches the game through Windows, so a copy
    /// would double it, and the bridge's own virtual controllers are Xbox 360 controllers, which
    /// Steam lists too, so copying them would copy the copies.
    pub fn bridged(self) -> bool {
        self != Self::Xbox
    }

    /// Whether Steam Input is on for controllers of this kind in the account's settings. `None`
    /// while the setting is Steam's default, which steambro can't tell. Steam always handles its
    /// own controllers.
    pub fn steam_input(self, settings: &InputSettings) -> Option<bool> {
        let value = match self {
            Self::Steam => return Some(true),
            Self::Xbox => &settings.xbox,
            Self::PlayStation => &settings.playstation,
            Self::Nintendo => &settings.nintendo,
            Self::Other => &settings.generic,
        };
        value.as_ref().map(|v| v != "0")
    }
}

/// A kind of controller connected now or used with the account before, as the settings page checks
/// it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Check {
    pub kind: Kind,
    pub connected: bool,
    /// The bridge copies it for Xbox games (`Kind::bridged`).
    pub bridged: bool,
    /// Steam Input is on for it (`Kind::steam_input`).
    pub steam_input: Option<bool>,
}

/// Each kind of controller connected now, and then each kind used with the account before, from
/// its controller settings.
pub fn checks(input: Option<&InputSettings>) -> Vec<Check> {
    let connected = connected();
    let seen = input.map(|i| seen_by_steam(&i.types_used)).unwrap_or_default();
    let mut kinds: Vec<Kind> = connected.iter().chain(&seen).copied().collect();
    kinds.sort_by_key(|kind| (!connected.contains(kind), *kind));
    kinds.dedup();
    kinds
        .into_iter()
        .map(|kind| Check { kind, connected: connected.contains(&kind), bridged: kind.bridged(), steam_input: input.and_then(|i| kind.steam_input(i)) })
        .collect()
}

/// The kinds of controllers connected now. Virtual controllers on the ViGEm bus (the bridge's own)
/// don't count.
fn connected() -> Vec<Kind> {
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
fn seen_by_steam(types: &str) -> Vec<Kind> {
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

/// The device's kind, when it's a controller: a game device in HID's terms, any of Valve's
/// devices, or a Microsoft device with an Xbox controller's device class or XInput HID ID (`IG_`,
/// over Bluetooth).
fn classify(vendor: u16, class: &str, game: bool, ids: &[String]) -> Option<Kind> {
    let controller = match vendor {
        0x28DE => true,
        0x045E => class.starts_with("xbox") || class.starts_with("xna") || ids.iter().any(|id| id.to_uppercase().contains("IG_")),
        _ => game,
    };
    controller.then(|| Kind::of_vendor(vendor))
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
    fn devices_and_their_kinds() {
        let none: [String; 0] = [];
        assert_eq!(classify(0x28DE, "hidclass", false, &none), Some(Kind::Steam));
        assert_eq!(classify(0x054C, "hidclass", true, &none), Some(Kind::PlayStation));
        assert_eq!(classify(0x054C, "media", false, &none), None, "a Sony headset");
        assert_eq!(classify(0x045E, "xboxcomposite", false, &none), Some(Kind::Xbox));
        assert_eq!(classify(0x045E, "hidclass", false, &[r"HID\VID_045E&PID_0B13&IG_00".to_string()]), Some(Kind::Xbox));
        assert_eq!(classify(0x045E, "keyboard", false, &none), None, "a Microsoft keyboard");
        assert_eq!(classify(0x2DC8, "hidclass", true, &none), Some(Kind::Other));
        assert_eq!(classify(0x046D, "mouse", false, &none), None);
    }

    #[test]
    fn the_bridge_copies_all_but_xbox_controllers() {
        let bridged: Vec<Kind> = [Kind::Xbox, Kind::Steam, Kind::PlayStation, Kind::Nintendo, Kind::Other].into_iter().filter(|k| k.bridged()).collect();
        assert_eq!(bridged, [Kind::Steam, Kind::PlayStation, Kind::Nintendo, Kind::Other]);
    }

    #[test]
    fn steam_input_by_kind() {
        let settings = InputSettings { xbox: Some("0".into()), playstation: Some("1".into()), ..Default::default() };
        assert_eq!(Kind::Xbox.steam_input(&settings), Some(false));
        assert_eq!(Kind::PlayStation.steam_input(&settings), Some(true), "on in games without PlayStation support");
        assert_eq!(Kind::Nintendo.steam_input(&settings), None);
        assert_eq!(Kind::Steam.steam_input(&InputSettings::default()), Some(true));
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

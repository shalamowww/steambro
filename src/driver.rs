//! The ViGEmBus driver that the Steam Input bridge needs: whether it's installed and answers.
//! steambro doesn't install it. The settings link its official installer (steambro.app/driver), the
//! same way for every kind of install. Its author retired it in 2023; it gets no updates but keeps
//! working.

use windows::Win32::Devices::DeviceAndDriverInstallation::{
    CM_Get_Device_IDW, CR_SUCCESS, DIGCF_PRESENT, GUID_DEVCLASS_SYSTEM, HDEVINFO, SETUP_DI_REGISTRY_PROPERTY, SP_DEVINFO_DATA, SPDRP_DRIVER, SPDRP_HARDWAREID,
    SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo, SetupDiGetClassDevsW, SetupDiGetDeviceRegistryPropertyW,
};
use windows::core::PCWSTR;

/// The hardware ID of ViGEmBus's root device.
const HARDWARE_ID: &str = r"Nefarius\ViGEmBus\Gen1";

pub struct Status {
    /// A ViGEmBus device exists.
    pub installed: bool,
    /// The installed driver's version, from its driver key.
    pub version: Option<String>,
    /// The bridge can reach the driver now.
    pub available: bool,
}

/// The driver's state now. Never fails: a driver that can't be read counts as not installed.
pub fn status() -> Status {
    let devices = devices().unwrap_or_default();
    let version = devices.iter().find_map(|d| d.version.clone());
    Status { installed: !devices.is_empty(), version, available: vigem_client::Client::connect().is_ok() }
}

struct Device {
    /// The device instance ID, like `ROOT\SYSTEM\0001`; the virtual controllers are its children.
    instance_id: String,
    version: Option<String>,
}

/// The ViGEmBus devices present, with their driver version.
fn devices() -> crate::Result<Vec<Device>> {
    let mut found = Vec::new();
    unsafe {
        let set = SetupDiGetClassDevsW(Some(&GUID_DEVCLASS_SYSTEM), PCWSTR::null(), None, DIGCF_PRESENT)?;
        let mut data = SP_DEVINFO_DATA { cbSize: size_of::<SP_DEVINFO_DATA>() as u32, ..Default::default() };
        let mut index = 0;
        while SetupDiEnumDeviceInfo(set, index, &mut data).is_ok() {
            if property_strings(set, &data, SPDRP_HARDWAREID).iter().any(|id| id.eq_ignore_ascii_case(HARDWARE_ID)) {
                let key = property_strings(set, &data, SPDRP_DRIVER).pop();
                let class_key = key.and_then(|k| windows_registry::LOCAL_MACHINE.open(format!(r"SYSTEM\CurrentControlSet\Control\Class\{k}")).ok());
                found.push(Device {
                    instance_id: instance_id(data.DevInst).unwrap_or_default(),
                    version: class_key.as_ref().and_then(|k| k.get_string("DriverVersion").ok()),
                });
            }
            index += 1;
        }
        let _ = SetupDiDestroyDeviceInfoList(set);
    }
    Ok(found)
}

/// The device instance IDs of the ViGEmBus devices, so their virtual controllers can be told apart
/// from real ones (src/controllers.rs).
pub fn bus_instance_ids() -> Vec<String> {
    devices().unwrap_or_default().into_iter().map(|d| d.instance_id).filter(|id| !id.is_empty()).collect()
}

/// A device's instance ID, like `ROOT\SYSTEM\0001`. `None` when Windows doesn't give it.
pub fn instance_id(devinst: u32) -> Option<String> {
    let mut buffer = [0u16; 512];
    if unsafe { CM_Get_Device_IDW(devinst, &mut buffer, 0) } != CR_SUCCESS {
        return None;
    }
    let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    Some(String::from_utf16_lossy(&buffer[..len]))
}

/// A device's string or string-list property, like its hardware IDs. Empty when it has none.
pub unsafe fn property_strings(set: HDEVINFO, data: &SP_DEVINFO_DATA, property: SETUP_DI_REGISTRY_PROPERTY) -> Vec<String> {
    let mut buffer = vec![0u8; 2048];
    let mut size = 0u32;
    if unsafe { SetupDiGetDeviceRegistryPropertyW(set, data, property, None, Some(&mut buffer), Some(&mut size)) }.is_err() {
        return Vec::new();
    }
    let units: Vec<u16> = buffer[..size as usize].as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
    String::from_utf16_lossy(&units).split('\0').filter(|s| !s.is_empty()).map(String::from).collect()
}

//! Starting an Xbox app inside its package, so that it has the package's identity, which Gaming
//! Services needs for the Xbox sign-in, cloud saves, and the licence.

use windows::Win32::System::Com::{CLSCTX_LOCAL_SERVER, CoCreateInstance};
use windows::Win32::UI::Shell::{AO_NONE, ApplicationActivationManager, IApplicationActivationManager};
use windows::Win32::UI::WindowsAndMessaging::{ASFW_ANY, AllowSetForegroundWindow};
use windows::core::HSTRING;

/// Asks Windows' activation manager to start the app, as the Start menu does. COM must be
/// initialized.
pub fn through_windows(aumid: &str) -> crate::Result<()> {
    // Steam, in front when the user pressed Play, started steambro, so steambro may let the game
    // take the foreground; without this, the game can open behind Steam's window.
    let _ = unsafe { AllowSetForegroundWindow(ASFW_ANY) };
    unsafe {
        let manager: IApplicationActivationManager = CoCreateInstance(&ApplicationActivationManager, None, CLSCTX_LOCAL_SERVER)?;
        manager.ActivateApplication(&HSTRING::from(aumid), &HSTRING::new(), AO_NONE)?;
    }
    Ok(())
}

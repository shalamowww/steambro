//! "Start with Windows": a value in the current user's `Run` key that starts steambro in the tray.

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE: &str = "steambro";

pub fn is_enabled() -> bool {
    windows_registry::CURRENT_USER.open(RUN_KEY).and_then(|key| key.get_string(VALUE)).is_ok()
}

pub fn set(enabled: bool) -> crate::Result<()> {
    let key = windows_registry::CURRENT_USER.create(RUN_KEY)?;
    if enabled {
        let exe = crate::steambro_exe()?;
        key.set_string(VALUE, format!("\"{}\" --background", exe.display()))?;
    } else if is_enabled() {
        key.remove_value(VALUE)?;
    }
    Ok(())
}

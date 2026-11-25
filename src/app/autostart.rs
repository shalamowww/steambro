//! "Start with Windows". A download writes a value in the current user's `Run` key that starts
//! steambro in the tray. An installed package can't: Windows keeps a package's registry writes to
//! itself. It has a startup task instead, declared in the package's manifest, which the user also
//! sees in Task Manager's startup apps; `main` takes a start by that task as `--background`.

use windows::ApplicationModel::{StartupTask, StartupTaskState};
use windows::core::HSTRING;

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE: &str = "steambro";
/// The startup task's `TaskId` in the package's manifest.
const TASK: &str = "steambro";

pub fn is_enabled() -> bool {
    if crate::packaged() {
        return task(|task| task.State()).is_ok_and(|state| matches!(state, StartupTaskState::Enabled | StartupTaskState::EnabledByPolicy));
    }
    windows_registry::CURRENT_USER.open(RUN_KEY).and_then(|key| key.get_string(VALUE)).is_ok()
}

/// Why "Start with Windows" didn't change. Windows doesn't let an app undo the user's choice in
/// its own startup settings, or the organization's.
pub enum Problem {
    OffInWindows,
    Policy,
    Other(String),
}

pub fn set(enabled: bool) -> Result<(), Problem> {
    change(enabled).map_err(|e| Problem::Other(e.to_string()))?
}

fn change(enabled: bool) -> crate::Result<Result<(), Problem>> {
    if crate::packaged() {
        if !enabled {
            task(|task| task.Disable())?;
            return Ok(Ok(()));
        }
        return Ok(match task(|task| task.RequestEnableAsync()?.join())? {
            StartupTaskState::DisabledByUser => Err(Problem::OffInWindows),
            StartupTaskState::DisabledByPolicy => Err(Problem::Policy),
            _ => Ok(()),
        });
    }
    let key = windows_registry::CURRENT_USER.create(RUN_KEY)?;
    if enabled {
        let exe = crate::steambro_exe()?;
        key.set_string(VALUE, format!("\"{}\" --background", exe.display()))?;
    } else if is_enabled() {
        key.remove_value(VALUE)?;
    }
    Ok(Ok(()))
}

/// Asks Windows about the package's startup task, on a thread of its own, outside the
/// apartment of the window's thread, which the tray menu runs on.
fn task<T: Send + 'static>(ask: impl FnOnce(&StartupTask) -> windows::core::Result<T> + Send + 'static) -> windows::core::Result<T> {
    std::thread::spawn(move || ask(&StartupTask::GetAsync(&HSTRING::from(TASK))?.join()?)).join().unwrap_or_else(|_| Err(windows::core::Error::new(windows::Win32::Foundation::E_FAIL, "the thread that asked Windows stopped")))
}

//! steambro's log, `%LOCALAPPDATA%\steambro\steambro.log`: what the tray app and each game launch
//! did, and what went wrong. It is for finding out why a game didn't start or wasn't added.

use std::fmt::Display;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use windows::Win32::System::SystemInformation::GetLocalTime;

/// Above this size, the log becomes `steambro.previous.log` and a new one starts.
const MAX_BYTES: u64 = 1 << 20;

static ROLE: OnceLock<&'static str> = OnceLock::new();
static WRITING: Mutex<()> = Mutex::new(());

/// Writes a line to the log: `log!("added {} games", n)`.
#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => { $crate::log::write(format_args!($($arg)*)) };
}

/// Names this process in each line: `app`, `launch`, `bridge`, or `list`. Several
/// steambro processes can write at the same time: the tray app, a game launch, and its helper.
pub fn set_role(role: &'static str) {
    let _ = ROLE.set(role);
}

/// Appends one line. A log that can't be written never stops the work it describes.
pub fn write(message: impl Display) {
    let _writing = WRITING.lock();
    let t = unsafe { GetLocalTime() };
    let line = format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}  {:<6}  {message}\r\n",
        t.wYear,
        t.wMonth,
        t.wDay,
        t.wHour,
        t.wMinute,
        t.wSecond,
        t.wMilliseconds,
        ROLE.get().unwrap_or(&"?"),
    );
    let _ = append(&line);
}

/// The log file. The tray menu opens it.
pub fn path() -> crate::Result<PathBuf> {
    Ok(crate::data_dir()?.join("steambro.log"))
}

fn append(line: &str) -> crate::Result<()> {
    let path = path()?;
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_BYTES) {
        let _ = std::fs::rename(&path, path.with_file_name("steambro.previous.log"));
    }
    std::fs::create_dir_all(path.parent().expect("the log path has a parent"))?;
    std::fs::OpenOptions::new().create(true).append(true).open(&path)?.write_all(line.as_bytes())?;
    Ok(())
}

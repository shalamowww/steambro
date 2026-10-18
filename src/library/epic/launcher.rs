//! The Epic Games launcher: where it is, whether it runs, the launch URI it takes, and closing it.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::Value;
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess};

const EXE: &str = "EpicGamesLauncher.exe";
/// The launcher's helper processes, which end with it.
const HELPERS: [&str; 2] = ["EpicWebHelper.exe", "EpicOnlineServicesUserHelper.exe"];
/// How long the launcher and its helpers may take to end.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(10);

/// The launcher's exe, from the handler Windows has for its `com.epicgames.launcher:` links.
pub fn exe() -> Option<PathBuf> {
    let command = windows_registry::CLASSES_ROOT.open(r"com.epicgames.launcher\shell\open\command").ok()?.get_string("").ok()?;
    let path = match command.strip_prefix('"') {
        Some(rest) => rest.split('"').next()?,
        None => command.split(' ').next()?,
    };
    Some(PathBuf::from(path))
}

pub fn is_running() -> bool {
    crate::process::is_running(EXE)
}

/// The link that makes the launcher start a game, as on the desktop shortcuts it creates:
/// `com.epicgames.launcher://apps/<namespace>%3A<item id>%3A<app name>?action=launch&silent=true`.
pub fn launch_uri(namespace: &str, item_id: &str, app_name: &str) -> String {
    format!("com.epicgames.launcher://apps/{namespace}%3A{item_id}%3A{app_name}?action=launch&silent=true")
}

/// Hands the launch URI to the launcher. A launcher that isn't running starts as a child of
/// steambro, so inside Steam's launch; it signs in and then starts the game. A running one gets
/// the URI from the process started here, which then exits.
pub fn launch(exe: &PathBuf, uri: &str) -> crate::Result<()> {
    Command::new(exe).arg(uri).spawn()?;
    Ok(())
}

/// Ends the launcher and waits until its helpers have ended too. The launcher has no quit command,
/// and a close message or Windows' sign-out messages only hide it in the tray, so steambro ends its
/// process, as Task Manager does; the launcher continues an interrupted download at its next start.
/// It stays open while it installs or updates something (`busy`). The error says why it is still
/// open.
pub fn close(data: &Path) -> crate::Result<()> {
    if let Some(reason) = busy(data) {
        return Err(reason.into());
    }
    for process in crate::process::list().into_iter().filter(|p| p.name.eq_ignore_ascii_case(EXE)) {
        let ended = unsafe { OpenProcess(PROCESS_TERMINATE, false, process.pid) }.and_then(|handle| unsafe {
            let result = TerminateProcess(handle, 0);
            let _ = CloseHandle(handle);
            result
        });
        if let Err(e) = ended {
            return Err(format!("Windows didn't let steambro end it: {e}").into());
        }
    }
    let deadline = Instant::now() + CLOSE_TIMEOUT;
    loop {
        let left: Vec<String> = crate::process::list().into_iter().map(|p| p.name).filter(|n| n.eq_ignore_ascii_case(EXE) || HELPERS.iter().any(|h| n.eq_ignore_ascii_case(h))).collect();
        if left.is_empty() {
            return Ok(());
        }
        if Instant::now() > deadline {
            return Err(format!("{} still running", left.join(", ")).into());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// Why ending the launcher now would interrupt it: a download in its queue
/// (`DownloadManager\DownloadState_*.json` isn't an empty list) or an install with a pending
/// manifest (`Manifests\Pending`).
pub fn busy(data: &Path) -> Option<&'static str> {
    let files = |dir: &Path| std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.is_file()).collect::<Vec<_>>();
    if !files(&data.join(r"Manifests\Pending")).is_empty() {
        return Some("it is installing or updating a game");
    }
    let queued = files(&data.join("DownloadManager")).iter().any(|path| {
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        name.starts_with("DownloadState_")
            && !matches!(std::fs::read(path).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()), Some(Value::Array(ref a)) if a.is_empty())
    });
    queued.then_some("it has downloads queued")
}

/// The parts of an Epic launch URI after `apps/`, decoded: `["fn", "<item id>", "Fortnite"]`, or
/// `["Fortnite"]` for the older short form.
pub fn uri_parts(text: &str) -> Vec<String> {
    let lower = text.to_lowercase();
    let Some(start) = lower.find("com.epicgames.launcher://apps/") else { return Vec::new() };
    let rest = &text[start + "com.epicgames.launcher://apps/".len()..];
    let path = rest.split(['?', '"', ' ']).next().unwrap_or_default();
    path.replace("%3A", ":").replace("%3a", ":").split(':').filter(|p| !p.is_empty()).map(String::from).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_uri_round_trips() {
        let uri = launch_uri("fn", "4fe75bbc5a674f4f9b356b5c90567da5", "Fortnite");
        assert_eq!(uri, "com.epicgames.launcher://apps/fn%3A4fe75bbc5a674f4f9b356b5c90567da5%3AFortnite?action=launch&silent=true");
        assert_eq!(uri_parts(&uri), ["fn", "4fe75bbc5a674f4f9b356b5c90567da5", "Fortnite"]);
        assert_eq!(uri_parts("\"com.epicgames.launcher://apps/Fortnite?action=launch&silent=true\""), ["Fortnite"]);
        assert!(uri_parts("steam://rungameid/1").is_empty());
    }

    #[test]
    fn busy_with_downloads_or_pending_installs() {
        // Not inside `steambro-test`: the sync test empties that folder while tests run in parallel.
        let data = std::env::temp_dir().join("steambro-test-epic-busy");
        let _ = std::fs::remove_dir_all(&data);
        std::fs::create_dir_all(data.join(r"Manifests\Pending")).unwrap();
        std::fs::create_dir_all(data.join("DownloadManager")).unwrap();
        let state = data.join(r"DownloadManager\DownloadState_52e3.json");
        std::fs::write(&state, "[]").unwrap();
        assert_eq!(busy(&data), None);
        std::fs::write(&state, r#"[{"AppName": "Fortnite"}]"#).unwrap();
        assert_eq!(busy(&data), Some("it has downloads queued"));
        std::fs::write(&state, "[]").unwrap();
        std::fs::write(data.join(r"Manifests\Pending\71EF.manifest"), "x").unwrap();
        assert_eq!(busy(&data), Some("it is installing or updating a game"));
        let _ = std::fs::remove_dir_all(&data);
    }
}

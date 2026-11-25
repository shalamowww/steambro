//! The Battle.net app: whether it runs, starting it and waiting until it has signed in, and the
//! `--exec` commands it takes.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime};

/// How long the app may take to sign in. Someone who has to type a password needs time.
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(300);
/// Signed in, the app takes a moment more before it acts on a launch command.
const SETTLE: Duration = Duration::from_secs(3);
const SIGNED_IN: &[u8] = b"Logged into Battle.net successfully";
/// How long the app and its Agent may take to close.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(30);
/// A Battle.net that starts right after the last one closed can hang on its login screen without
/// writing its log: on 2026-11-08 one opened 26 ms after the last process ended and never signed in.
/// So `reopen` waits this long first.
const REOPEN_AFTER: Duration = Duration::from_secs(5);
/// How long a Battle.net opened again may take to sign in before the log says it hasn't.
const REOPEN_SIGN_IN: Duration = Duration::from_secs(90);

pub fn is_running() -> bool {
    crate::process::is_running("Battle.net.exe")
}

/// Starts the app and waits until it has signed in. A launch command sent earlier only opens the
/// game's page in the app.
pub fn start(client: &Path) -> crate::Result<()> {
    // File times can be a little behind the clock.
    let since = SystemTime::now() - Duration::from_secs(2);
    Command::new(client).spawn()?;
    let deadline = Instant::now() + SIGN_IN_TIMEOUT;
    while Instant::now() < deadline {
        if signed_in_since(since) {
            std::thread::sleep(SETTLE);
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    Err("Battle.net didn't sign in within 5 minutes".into())
}

/// Asks the app to close (`shutdown`) and waits until it and its Agent have closed. The Agent
/// would otherwise start the next game outside Steam's launch. The error says which is still open.
/// A command that no Battle.net answers starts a whole new one, which then closes itself. Without
/// a Battle.net.exe, only its Agent is waited for; a Battle.net.exe that is already closing doesn't
/// answer either, which only `exits_with_game` foresees (launch.rs, `hand_over_app`).
pub fn close(client: &Path) -> crate::Result<()> {
    if is_running() {
        command(client, "shutdown")?;
    }
    let deadline = Instant::now() + CLOSE_TIMEOUT;
    loop {
        let app = is_running();
        let agent = agent_running();
        if !app && !agent {
            return Ok(());
        }
        if Instant::now() > deadline {
            let left = match (app, agent) {
                (true, true) => "Battle.net and its Agent are",
                (true, false) => "Battle.net is",
                _ => "Battle.net closed, but its Agent is",
            };
            return Err(format!("{left} still running after {} seconds", CLOSE_TIMEOUT.as_secs()).into());
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn agent_running() -> bool {
    crate::process::list().into_iter().any(|p| {
        p.name.eq_ignore_ascii_case("Agent.exe") && crate::process::image_path(p.pid).is_some_and(|path| path.to_string_lossy().to_lowercase().contains(r"\battle.net\agent"))
    })
}

/// Opens the app outside Steam's launch (`process::start_outside_steam`), minimized, `REOPEN_AFTER`
/// after the last one closed, and returns its main process once it runs: the Battle.net.exe that no
/// other Battle.net.exe started. Whether it signs in goes to the log later (`watch_sign_in`).
pub fn reopen(client: &Path) -> crate::Result<u32> {
    std::thread::sleep(REOPEN_AFTER);
    // File times can be a little behind the clock.
    let since = SystemTime::now() - Duration::from_secs(2);
    crate::process::start_outside_steam(client, "", true)?;
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        let running = crate::process::list();
        let app = |pid: u32| running.iter().any(|p| p.pid == pid && p.name.eq_ignore_ascii_case("Battle.net.exe"));
        if let Some(main) = running.iter().find(|p| p.name.eq_ignore_ascii_case("Battle.net.exe") && !app(p.parent)) {
            watch_sign_in(since);
            return Ok(main.pid);
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Err("Battle.net didn't start within 20 seconds".into())
}

/// Logs whether a Battle.net opened again has signed in, in the background, so that a report for a
/// bug shows one that hangs on its login screen. It may wait for a password, so steambro never
/// closes it. The launch may end first, and the watch with it.
fn watch_sign_in(since: SystemTime) {
    std::thread::spawn(move || {
        let deadline = Instant::now() + REOPEN_SIGN_IN;
        while Instant::now() < deadline {
            if signed_in_since(since) {
                crate::log!("Battle.net has signed in again");
                return;
            }
            std::thread::sleep(Duration::from_secs(1));
        }
        crate::log!("Battle.net hasn't signed in {}s after it opened again: it may wait for a password, or hang on its login screen", REOPEN_SIGN_IN.as_secs());
    });
}

/// Battle.net's "When I launch a game: Exit Battle.net completely": `Client.GameLaunchWindowBehavior`
/// is 2, a string in the file. Its log then says "game launch behavior set to exit", and it closes a
/// few seconds after the game starts, also when steambro opened it (2026-11-08).
pub fn exits_with_game() -> bool {
    config().is_some_and(|config| exits_with_game_in(&config))
}

fn exits_with_game_in(config: &serde_json::Value) -> bool {
    match config.pointer("/Client/GameLaunchWindowBehavior") {
        Some(serde_json::Value::String(value)) => value.trim() == "2",
        Some(value) => value.as_i64() == Some(2),
        None => false,
    }
}

/// `%APPDATA%\Battle.net\Battle.net.config`. Readers take only the keys they need: the file also has
/// account names.
fn config_path() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join(r"Battle.net\Battle.net.config"))
}

fn config() -> Option<serde_json::Value> {
    std::fs::read(config_path()?).ok().and_then(|b| serde_json::from_slice(&b).ok())
}

/// Battle.net's own settings that change what happens at a launch, from
/// `%APPDATA%\Battle.net\Battle.net.config`, and the game's update state there. Only these keys:
/// the file also has account names.
pub fn settings(game_uid: Option<&str>) -> Vec<(String, String)> {
    if config_path().is_none() {
        return Vec::new();
    }
    let Some(config) = config() else {
        return vec![("Battle.net.config".into(), "can't be read".into())];
    };
    let text = |v: &serde_json::Value| match v {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    let mut found = Vec::new();
    for key in ["Version/Release/LastBuildVersion", "GameLaunchWindowBehavior", "SingleInstance", "AutoLogin", "DefaultStartupScreen", "Install/DownloadLimitNextPatchInBps"] {
        if let Some(v) = config.pointer(&format!("/Client/{key}")) {
            found.push((format!("Battle.net {}", key.replace('/', ".")), text(v)));
        }
    }
    if let Some(uid) = game_uid
        && let Some(serde_json::Value::Object(game)) = config.pointer(&format!("/Games/{uid}"))
    {
        for key in ["AutoUpdateCriteriaPassed", "UserPausedUpdate", "Resumable"] {
            if let Some(v) = game.get(key) {
                found.push((format!("Battle.net {uid}.{key}"), text(v)));
            }
        }
    }
    found
}

/// Sends a command to the running app: `launch <code>` or `shutdown`. The process started for it
/// passes the command on and exits.
pub fn command(client: &Path, command: &str) -> crate::Result<()> {
    Command::new(client).arg(format!("--exec={command}")).spawn()?;
    Ok(())
}

/// Whether a log that the app wrote since `since` says it has signed in. The app starts a new log
/// file, `%LOCALAPPDATA%\Battle.net\Logs\battle.net-<time>.log`, each time it starts.
fn signed_in_since(since: SystemTime) -> bool {
    let Some(dir) = std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join(r"Battle.net\Logs")) else { return false };
    let Ok(entries) = std::fs::read_dir(dir) else { return false };
    entries
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("battle.net-"))
        .filter(|e| e.metadata().and_then(|m| m.modified()).is_ok_and(|t| t >= since))
        .any(|e| std::fs::read(e.path()).is_ok_and(|bytes| bytes.windows(SIGNED_IN.len()).any(|w| w == SIGNED_IN)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn exit_on_game_launch_is_behavior_two() {
        assert!(exits_with_game_in(&json!({ "Client": { "GameLaunchWindowBehavior": "2" } })));
        assert!(exits_with_game_in(&json!({ "Client": { "GameLaunchWindowBehavior": 2 } })));
        assert!(!exits_with_game_in(&json!({ "Client": { "GameLaunchWindowBehavior": "1" } })));
        assert!(!exits_with_game_in(&json!({ "Client": { "GameLaunchWindowBehavior": "0" } })));
        assert!(!exits_with_game_in(&json!({ "Client": {} })));
        assert!(!exits_with_game_in(&json!({})));
    }
}

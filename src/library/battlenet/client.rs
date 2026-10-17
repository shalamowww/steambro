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
/// would otherwise start the next game outside Steam's launch.
pub fn close(client: &Path) -> crate::Result<()> {
    command(client, "shutdown")?;
    let deadline = Instant::now() + CLOSE_TIMEOUT;
    while Instant::now() < deadline {
        let agent = crate::process::list().into_iter().any(|p| {
            p.name.eq_ignore_ascii_case("Agent.exe")
                && crate::process::image_path(p.pid).is_some_and(|path| path.to_string_lossy().to_lowercase().contains(r"\battle.net\agent"))
        });
        if !is_running() && !agent {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    Err("Battle.net didn't close within 30 seconds".into())
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

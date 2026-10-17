//! A question for the user while a game starts. `steambro launch` has no window, so it asks the
//! tray app to show one: it writes the question to `prompts\<id>.json`, runs
//! `steambro --background --prompt <id>` (which reaches the running tray app, or starts it), and
//! waits for `prompts\<id>.answer.json`.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use windows::Win32::UI::WindowsAndMessaging::{ASFW_ANY, AllowSetForegroundWindow};

/// The store's app already runs outside Steam: may steambro restart it inside Steam's launch?
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    pub store: String,
    pub store_name: String,
    /// The store's app, like `Battle.net`.
    pub app: String,
    pub game: String,
}

#[derive(Serialize, Deserialize)]
pub struct Answer {
    pub restart: bool,
    /// The answer becomes the store's setting.
    pub always: bool,
}

/// Shows the question and waits up to `timeout` for the answer (`LaunchSettings::question_timeout_s`);
/// `None` when there was none in time, and the launch goes on as if the user said no.
pub fn ask(request: &Request, timeout: Duration) -> Option<Answer> {
    let id = std::process::id().to_string();
    let (question, answer) = paths(&id).ok()?;
    std::fs::create_dir_all(question.parent()?).ok()?;
    let _ = std::fs::remove_file(&answer);
    std::fs::write(&question, serde_json::to_vec(request).ok()?).ok()?;
    // Steam started this launch, so it may bring a window to the front; let the tray app do it.
    let _ = unsafe { AllowSetForegroundWindow(ASFW_ANY) };
    if let Err(e) = std::process::Command::new(std::env::current_exe().ok()?).args(["--background", "--prompt", &id]).spawn() {
        crate::log!("couldn't show the question: {e}");
        return None;
    }
    let deadline = Instant::now() + timeout;
    let found = loop {
        if let Some(found) = std::fs::read(&answer).ok().and_then(|bytes| serde_json::from_slice::<Answer>(&bytes).ok()) {
            break Some(found);
        }
        if Instant::now() > deadline {
            crate::log!("no answer within {} seconds", timeout.as_secs());
            break None;
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    let _ = std::fs::remove_file(&question);
    let _ = std::fs::remove_file(&answer);
    found
}

/// The question with this ID, for the tray app's prompt window.
pub fn request(id: &str) -> crate::Result<Request> {
    Ok(serde_json::from_slice(&std::fs::read(paths(id)?.0).map_err(|_| "the game launch stopped waiting for an answer")?)?)
}

pub fn answer(id: &str, answer: &Answer) -> crate::Result<()> {
    std::fs::write(paths(id)?.1, serde_json::to_vec(answer)?)?;
    Ok(())
}

/// The question's and the answer's files. An ID is a launch's process ID, so only digits.
fn paths(id: &str) -> crate::Result<(PathBuf, PathBuf)> {
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("{id} isn't a question ID").into());
    }
    let dir = crate::data_dir()?.join("prompts");
    Ok((dir.join(format!("{id}.json")), dir.join(format!("{id}.answer.json"))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn question_ids_are_process_ids() {
        assert!(paths("4242").is_ok());
        assert!(paths("").is_err());
        assert!(paths(r"..\settings").is_err());
        assert!(paths("12a").is_err());
    }
}

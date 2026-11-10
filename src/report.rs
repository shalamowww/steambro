//! The report for a bug: one text file with everything that decides what steambro does on this PC,
//! for the user to look over and send. steambro's version and Windows', Steam's settings that
//! matter to steambro, the driver and the controllers, steambro's settings and what they make of
//! each game, each store's app and its own settings, and the end of steambro's log and of Steam's
//! log of game processes. The tray menu, the settings window and `steambro report` write it.
//!
//! It names no person: the Windows user name, the PC's name and Steam account IDs are replaced,
//! and from the stores' settings files only the settings that change a launch are read, never an
//! account or a sign-in. Game names and folders stay; they are what a bug is about.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::library::{Game, Library};
use crate::plan::Behavior;
use crate::settings::Settings;
use crate::steam::Steam;

/// How much of each log goes into a report, from its end.
const STEAMBRO_LOG_LINES: usize = 6000;
const STEAM_PROCESS_LOG_LINES: usize = 400;
const STEAM_CONTROLLER_LOG_LINES: usize = 200;

/// Writes a report to `%LOCALAPPDATA%\steambro\reports` and returns its path.
pub fn write() -> crate::Result<PathBuf> {
    let text = redact(&build(), &Redactions::here());
    let dir = crate::data_dir()?.join("reports");
    std::fs::create_dir_all(&dir)?;
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    let path = dir.join(format!("steambro-report-{:04}-{:02}-{:02}-{:02}{:02}{:02}.txt", t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond));
    std::fs::write(&path, text.replace('\n', "\r\n"))?;
    crate::log!("wrote a report: {}", path.display());
    Ok(path)
}

/// steambro's and Windows' versions, and how this steambro runs.
pub fn environment() -> Vec<(String, String)> {
    let mut lines = vec![("steambro".to_string(), crate::VERSION.to_string())];
    if let Ok(exe) = std::env::current_exe() {
        lines.push(("exe".into(), exe.display().to_string()));
    }
    if let Ok(alias) = crate::steambro_exe() {
        lines.push(("Steam's shortcuts run".into(), alias.display().to_string()));
    }
    lines.push(("Windows".into(), windows_version()));
    lines
}

/// Steam's state and the settings of the current account that change a launch.
pub fn steam() -> Vec<(String, String)> {
    let mut lines = Vec::new();
    let steam = match Steam::locate() {
        Ok(steam) => steam,
        Err(e) => return vec![("Steam".into(), e.to_string())],
    };
    lines.push(("Steam".into(), format!("{}, version {}", steam.root().display(), steam.version().unwrap_or_else(|| "unknown".into()))));
    lines.push(("Steam running".into(), Steam::is_running().to_string()));
    lines.push((
        "virtual controllers in Steam's blocklist".into(),
        steam.ignores_controller(crate::bridge::PAD_ID.0, crate::bridge::PAD_ID.1).to_string(),
    ));
    match steam.current_user() {
        Ok(Some(user)) => {
            lines.push(("current account".into(), user.id.to_string()));
            lines.push(("overlay".into(), user.overlay_settings().map_or_else(|e| e.to_string(), |o| format!("{o:?}"))));
            lines.push(("Steam Input".into(), user.input_settings().map_or_else(|e| e.to_string(), |i| format!("{i:?}"))));
        }
        Ok(None) => lines.push(("current account".into(), "none signed in".into())),
        Err(e) => lines.push(("current account".into(), e.to_string())),
    }
    lines
}

/// The ViGEmBus driver and the controllers, as the settings window checks them.
pub fn controllers() -> Vec<(String, String)> {
    let mut lines = vec![("driver".to_string(), format!("{:?}", crate::driver::status()))];
    let input = Steam::locate().ok().and_then(|s| s.current_user().ok().flatten()).and_then(|u| u.input_settings().ok());
    for check in crate::controllers::checks(input.as_ref()) {
        lines.push(("controller".into(), format!("{check:?}")));
    }
    if let Ok(numbers) = std::fs::read_to_string(crate::data_dir().map(|d| d.join("controllers.json")).unwrap_or_default()) {
        lines.push(("virtual controller numbers".into(), numbers.split_whitespace().collect::<Vec<_>>().join(" ")));
    }
    lines
}

fn build() -> String {
    let mut out = String::new();
    let _ = writeln!(out, "steambro report\n\nLook it over before you share it. Your Windows user name, your PC's name and your Steam account IDs are replaced.\n");
    section(&mut out, "steambro and Windows", &environment());
    section(&mut out, "Steam", &steam());
    section(&mut out, "Driver and controllers", &controllers());

    let settings = Settings::load();
    let _ = writeln!(out, "== Settings (settings.json) ==\n{}\n", serde_json::to_string_pretty(&settings).unwrap_or_default());

    let library = Library::detect();
    let mut stores = Vec::new();
    for store in crate::library::stores() {
        let games = library.games.iter().filter(|g| g.store.id() == store.id()).count();
        let failed = library.failed.contains(&store.id());
        stores.push((store.name().to_string(), if failed { "couldn't list its games".into() } else { format!("{games} games") }));
        stores.push((format!("{} defaults", store.name()), format!("{:?}", store.defaults())));
        if let Some(app) = store.app_name() {
            stores.push((format!("{} app", store.name()), format!("{app}, running: {}, busy: {}", store.app_running(), store.app_busy().unwrap_or_else(|| "no".into()))));
        }
        stores.extend(store.diagnostics(None));
    }
    for warning in &library.warnings {
        stores.push(("warning".into(), warning.clone()));
    }
    section(&mut out, "Stores", &stores);

    let _ = writeln!(out, "== Games ==");
    let shortcuts = Steam::locate().ok().and_then(|s| s.current_user().ok().flatten()).and_then(|u| u.shortcuts().ok()).unwrap_or_default();
    for game in &library.games {
        game_lines(&mut out, game, &settings, &shortcuts);
    }
    let _ = writeln!(out);

    let steam_logs = Steam::locate().map(|s| s.root().join("logs")).ok();
    if let Some(logs) = &steam_logs {
        tail_section(&mut out, "Steam's log of game processes (gameprocess_log.txt)", &[logs.join("gameprocess_log.previous.txt"), logs.join("gameprocess_log.txt")], STEAM_PROCESS_LOG_LINES);
        tail_section(&mut out, "Steam's controller log (controller.txt)", &[logs.join("controller.previous.txt"), logs.join("controller.txt")], STEAM_CONTROLLER_LOG_LINES);
    }
    if let Ok(log) = crate::log::path() {
        tail_section(&mut out, "steambro's log", &[log.with_file_name("steambro.previous.log"), log], STEAMBRO_LOG_LINES);
    }
    out
}

/// A game, its Steam shortcut, and what its launch would do with the settings now.
fn game_lines(out: &mut String, game: &Game, settings: &Settings, shortcuts: &[crate::steam::Shortcut]) {
    let key = game.key();
    let _ = writeln!(out, "{} ({key}), {}", game.name(), game.store.name());
    let _ = writeln!(out, "    folder: {}", game.install_dir().display());
    let _ = writeln!(out, "    starts: {}", game.launch_description());
    let own = settings.game(&key);
    if !own.is_empty() {
        let _ = writeln!(out, "    own choices: {own:?}");
    }
    let behavior = Behavior::of(settings, game);
    let _ = writeln!(out, "    launch: {:?}", behavior.knobs);
    if let Some(shortcut) = shortcuts.iter().find(|s| s.launch_options.to_lowercase().contains(&key.to_lowercase())) {
        let _ = writeln!(out, "    Steam shortcut {}: {} {}, overlay {}", shortcut.app_id, shortcut.exe, shortcut.launch_options, shortcut.allow_overlay);
    }
}

fn section(out: &mut String, title: &str, lines: &[(String, String)]) {
    let _ = writeln!(out, "== {title} ==");
    for (label, value) in lines {
        let _ = writeln!(out, "{label}: {value}");
    }
    let _ = writeln!(out);
}

/// The last `max` lines of the files, read in order, as one log.
fn tail_section(out: &mut String, title: &str, files: &[PathBuf], max: usize) {
    let text: String = files.iter().filter_map(|f| std::fs::read(f).ok()).map(|b| String::from_utf8_lossy(&b).into_owned()).collect::<Vec<_>>().join("\n");
    let lines: Vec<&str> = text.lines().collect();
    let _ = writeln!(out, "== {title}, last {} lines ==", lines.len().min(max));
    for line in &lines[lines.len().saturating_sub(max)..] {
        let _ = writeln!(out, "{line}");
    }
    let _ = writeln!(out);
}

/// Windows' edition and build, like `Windows 11 Pro 24H2, build 26100.4652`.
fn windows_version() -> String {
    let Ok(key) = windows_registry::LOCAL_MACHINE.open(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion") else { return "unknown".into() };
    let text = |name: &str| key.get_string(name).unwrap_or_default();
    let build: u32 = text("CurrentBuildNumber").parse().unwrap_or(0);
    // Windows 11 still says "Windows 10" in its product name.
    let product = if build >= 22000 { text("ProductName").replace("Windows 10", "Windows 11") } else { text("ProductName") };
    format!("{product} {}, build {build}.{}", text("DisplayVersion"), key.get_u32("UBR").unwrap_or(0))
}

/// What a report replaces, so that it names no person.
struct Redactions {
    /// The Windows user name, inside paths.
    user: Option<String>,
    computer: Option<String>,
    /// Steam account IDs, each replaced by `account-<n>`.
    accounts: Vec<u32>,
}

impl Redactions {
    fn here() -> Self {
        let name = |var: &str| std::env::var(var).ok().filter(|v| !v.is_empty());
        let accounts = Steam::locate().and_then(|s| s.users()).map(|users| users.into_iter().map(|u| u.id).collect()).unwrap_or_default();
        Self { user: name("USERNAME"), computer: name("COMPUTERNAME").filter(|c| c.len() >= 3), accounts }
    }
}

fn redact(text: &str, r: &Redactions) -> String {
    let mut text = text.to_string();
    if let Some(user) = &r.user {
        for sep in ['\\', '/'] {
            text = replace_folder(&text, &format!("users{sep}{user}"), &format!("Users{sep}<user>"));
        }
    }
    if let Some(computer) = &r.computer {
        text = replace_word(&text, computer, "<pc>", |c| c.is_ascii_alphanumeric() || c == '-');
    }
    for (n, id) in r.accounts.iter().enumerate() {
        text = replace_word(&text, &id.to_string(), &format!("account-{}", n + 1), |c| c.is_ascii_digit());
    }
    hide_emails(&text)
}

/// Replaces `from` (any case) where a path folder ends after it.
fn replace_folder(text: &str, from: &str, to: &str) -> String {
    replace_matches(text, from, to, |_, after| after.is_none_or(|c| matches!(c, '\\' | '/' | '"' | '\'' | ' ' | ',' | ')' | ';')))
}

/// Replaces `from` (any case) where neither side continues the word, as `inside` says.
fn replace_word(text: &str, from: &str, to: &str, inside: fn(char) -> bool) -> String {
    replace_matches(text, from, to, |before, after| !before.is_some_and(inside) && !after.is_some_and(inside))
}

/// Replaces each match of `from`, in any case, that `keep` accepts by the characters around it.
fn replace_matches(text: &str, from: &str, to: &str, keep: impl Fn(Option<char>, Option<char>) -> bool) -> String {
    let lower = text.to_lowercase();
    // Lowercasing changed the length (some letters do), so positions wouldn't match; leave it.
    if lower.len() != text.len() || from.is_empty() {
        return text.to_string();
    }
    let needle = from.to_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for (at, _) in lower.match_indices(&needle) {
        let end = at + needle.len();
        if at < last || !keep(text[..at].chars().next_back(), text[end..].chars().next()) {
            continue;
        }
        out.push_str(&text[last..at]);
        out.push_str(to);
        last = end;
    }
    out.push_str(&text[last..]);
    out
}
/// Replaces anything shaped like an email address with `<email>`.
fn hide_emails(text: &str) -> String {
    let part = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '+');
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('@') {
        let start = rest[..at].char_indices().rev().take_while(|(_, c)| part(*c)).last().map_or(at, |(i, _)| i);
        let end = at + 1 + rest[at + 1..].char_indices().take_while(|(_, c)| part(*c)).last().map_or(0, |(i, c)| i + c.len_utf8());
        let domain = &rest[at + 1..end];
        if start < at && domain.contains('.') && !domain.starts_with('.') {
            out.push_str(&rest[..start]);
            out.push_str("<email>");
        } else {
            out.push_str(&rest[..end.max(at + 1)]);
        }
        rest = &rest[end.max(at + 1)..];
    }
    out.push_str(rest);
    out
}

/// Opens Explorer with the report selected, so the user can look at it and attach it.
pub fn show(path: &Path) -> crate::Result<()> {
    std::process::Command::new("explorer.exe").arg(format!("/select,{}", path.display())).spawn()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_names_no_person() {
        let r = Redactions { user: Some("Me".into()), computer: Some("GAMING-PC".into()), accounts: vec![12345678] };
        let text = r#"C:\Users\me\AppData\Local\steambro and C:/Users/ME/x, "C:\Users\me" but C:\Users\meg\ stays; GAMING-PC; account 12345678 (112345678); pat.doe+x@mail.example.com; @mention; me"#;
        assert_eq!(
            redact(text, &r),
            r#"C:\Users\<user>\AppData\Local\steambro and C:/Users/<user>/x, "C:\Users\<user>" but C:\Users\meg\ stays; <pc>; account account-1 (112345678); <email>; @mention; me"#
        );
    }
}

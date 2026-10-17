//! The tray app: one window with the library, a tray icon to reach it, and the background work
//! done at each start.

mod autostart;
mod commands;
mod notify;
mod protocol;
mod tray;

use std::sync::{Arc, Mutex, RwLock};

use tauri::{AppHandle, Emitter, Manager, WindowEvent};

use crate::art::cache::Cache;
use crate::library::Library;
use crate::steam::Steam;
use crate::sync::{self, State};

/// What the commands share. Locks are held only for short reads; library detection and Steam
/// changes run outside them.
pub struct Shared {
    library: RwLock<Option<Arc<Library>>>,
    /// Also serializes changes: whoever changes Steam or the state holds this lock.
    state: Mutex<State>,
    cache: Cache,
}

impl Shared {
    /// The detected library, detecting it first if this is the first use or `refresh` is set.
    fn library(&self, refresh: bool) -> Arc<Library> {
        if !refresh && let Some(library) = self.library.read().unwrap().as_ref() {
            return library.clone();
        }
        let library = Arc::new(Library::detect());
        *self.library.write().unwrap() = Some(library.clone());
        library
    }
}

pub fn run(background: bool, prompt: Option<String>) {
    let shared = match (State::load(), Cache::open()) {
        (Ok(state), Ok(cache)) => Shared { library: RwLock::new(None), state: Mutex::new(state), cache },
        (Err(e), _) | (_, Err(e)) => {
            crate::show_error(&format!("steambro can't start: {e}"));
            return;
        }
    };
    let result = tauri::Builder::default()
        // A second start (for example, from the Start menu) opens the running steambro instead; one
        // with `--prompt` shows a game launch's question there.
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| match prompt_argument(&args) {
            Some(id) => open_prompt(app, &id),
            None => show_window(app),
        }))
        .manage(shared)
        .invoke_handler(tauri::generate_handler![
            commands::library,
            commands::change_steam,
            commands::set_crop,
            commands::replace_art,
            commands::reset_art,
            commands::open_folder,
            commands::add_exe,
            commands::forget_exe,
            commands::set_mood,
            commands::settings_view,
            commands::save_store_settings,
            commands::set_game_settings,
            commands::save_launch_settings,
            commands::save_new_game_notices,
            commands::open_link,
            commands::prompt_request,
            commands::answer_prompt,
            commands::open_steam_controller_settings,
        ])
        .register_asynchronous_uri_scheme_protocol("art", protocol::handle)
        .setup(move |app| {
            // Here, not earlier: a second start hands over to the running app before this.
            crate::log!("steambro {} started{}", env!("CARGO_PKG_VERSION"), if background { " in the background" } else { "" });
            tray::create(app.handle())?;
            if !background {
                show_window(app.handle());
            }
            if let Some(id) = &prompt {
                open_prompt(app.handle(), id);
            }
            let handle = app.handle().clone();
            std::thread::spawn(move || start_of_run(&handle));
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                let mut tick: u32 = 0;
                loop {
                    crate::bridge::release_orphans();
                    if tick.is_multiple_of(NEW_GAMES_EVERY) {
                        look_for_new_games(&handle);
                    }
                    tick = tick.wrapping_add(1);
                    std::thread::sleep(std::time::Duration::from_secs(5));
                }
            });
            Ok(())
        })
        // Closing the window keeps steambro in the tray; "Quit" in the tray menu ends it.
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event
                && window.label() == "main"
            {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .run(tauri::generate_context!());
    if let Err(e) = result {
        crate::show_error(&format!("steambro stopped: {e}"));
    }
}

/// How often the tray app looks for new games, in turns of its 5-second loop: every 5 minutes.
const NEW_GAMES_EVERY: u32 = 60;

/// Looks for games installed since the last look (`State::seen`). A new game that isn't in the
/// current Steam account gets a notification, as the settings say. The first look only records
/// what's there. An uninstalled game leaves the list, so installing it again counts as new.
fn look_for_new_games(app: &AppHandle) {
    let shared = app.state::<Shared>();
    let library = shared.library(true);
    let user = crate::steam::Steam::locate().ok().and_then(|s| s.current_user().ok().flatten());
    let shortcuts = user.as_ref().and_then(|u| u.shortcuts().ok()).unwrap_or_default();
    let mut state = shared.state.lock().unwrap();
    let first = state.seen.is_none();
    let mut seen = state.seen.take().unwrap_or_default();
    seen.retain(|key| !library.is_uninstalled(key));
    let new: Vec<&crate::library::Game> = library.games.iter().filter(|g| !seen.contains(&g.key())).collect();
    let not_in_steam: Vec<String> = new
        .iter()
        .filter(|g| user.as_ref().is_some_and(|u| crate::sync::status(&library, &state, u.id, &shortcuts, g) == crate::sync::Status::NotAdded))
        .map(|g| g.name().to_string())
        .collect();
    seen.extend(new.iter().map(|g| g.key()));
    state.seen = Some(seen);
    if let Err(e) = state.save() {
        crate::log!("couldn't save the games seen: {e}");
    }
    drop(state);
    if new.is_empty() {
        return;
    }
    let _ = app.emit("library-changed", ());
    if first || not_in_steam.is_empty() || !crate::settings::Settings::load().new_game_notices {
        return;
    }
    crate::log!("new games, not in Steam: {}", not_in_steam.join(", "));
    let (title, text) = match not_in_steam.as_slice() {
        [one] => (format!("{one} is installed"), "Add it to Steam? Click to open steambro.".to_string()),
        many => (format!("{} new games", many.len()), format!("{}. Click to open steambro and add them to Steam.", many.join(", "))),
    };
    notify::show(app, &title, &text);
}

/// Shows the window. When it comes back from the tray, the window looks for new games, because
/// WebView2 doesn't tell the page that it was hidden.
pub fn show_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        if matches!(window.is_visible(), Ok(false)) {
            let _ = app.emit("window-shown", ());
        }
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

/// The question ID after `--prompt` in a second start's arguments.
fn prompt_argument(args: &[String]) -> Option<String> {
    args.iter().position(|a| a == "--prompt").and_then(|i| args.get(i + 1)).cloned()
}

/// Shows a game launch's question (src/prompt.rs) in a small window of its own, on top of
/// everything, Steam's Big Picture included. ui/prompt.html reads the question and answers it.
fn open_prompt(app: &AppHandle, id: &str) {
    if !id.bytes().all(|b| b.is_ascii_digit()) {
        return;
    }
    let label = format!("prompt-{id}");
    if let Some(window) = app.get_webview_window(&label) {
        let _ = window.set_focus();
        return;
    }
    let built = tauri::WebviewWindowBuilder::new(app, &label, tauri::WebviewUrl::App("prompt.html".into()))
        .initialization_script(format!("window.PROMPT_ID = '{id}';"))
        .title("steambro")
        .inner_size(560.0, 340.0)
        .resizable(false)
        .decorations(false)
        .always_on_top(true)
        .center()
        .focused(true)
        .build();
    match built {
        Ok(window) => {
            let _ = window.set_focus();
        }
        Err(e) => crate::log!("couldn't show the question: {e}"),
    }
}

/// Detects the library, forgets steambro shortcuts deleted in Steam, and retries pending artwork,
/// in every account. Then it tells the window to reload.
fn start_of_run(app: &AppHandle) {
    let shared = app.state::<Shared>();
    let library = shared.library(true);
    crate::log!("found {} games", library.games.len());
    for warning in &library.warnings {
        crate::log!("note: {warning}");
    }
    let users = Steam::locate().and_then(|steam| steam.users()).unwrap_or_default();
    let mut state = shared.state.lock().unwrap();
    for user in &users {
        match sync::forget_deleted(&mut state, user) {
            Ok(gone) if !gone.is_empty() => crate::log!("deleted in Steam, so forgotten: {}", gone.join(", ")),
            Ok(_) => {}
            Err(e) => crate::log!("couldn't check account {} for deleted shortcuts: {e}", user.id),
        }
        match sync::retry_pending(&library, &shared.cache, &mut state, user) {
            Ok(true) => crate::log!("tried pending artwork again for account {}", user.id),
            Ok(false) => {}
            Err(e) => crate::log!("couldn't retry artwork for account {}: {e}", user.id),
        }
    }
    drop(state);
    let _ = app.emit("library-changed", ());
}

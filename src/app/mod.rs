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
    /// Start fresh is deleting steambro's files: nothing may write them again before the restart.
    starting_fresh: std::sync::atomic::AtomicBool,
    /// When steambro asked Steam to close and stopped waiting for it (`Steam::shut_down`). A Steam
    /// that closed soon after was the user's open Steam, so the next change starts it again
    /// (`commands::steam_open`).
    steam_closing: Mutex<Option<std::time::Instant>>,
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
        (Ok(state), Ok(cache)) => Shared { library: RwLock::new(None), state: Mutex::new(state), cache, starting_fresh: Default::default(), steam_closing: Default::default() },
        (Err(e), _) | (_, Err(e)) => {
            crate::show_error(&crate::i18n::text("error.cant_start", &[("error", &e.to_string())]));
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
            commands::pick_art,
            commands::set_art_follow,
            commands::reset_art,
            commands::open_folder,
            commands::add_exe,
            commands::forget_exe,
            commands::set_mood,
            commands::settings_view,
            commands::save_settings,
            commands::strings,
            commands::set_game_settings,
            commands::set_ignored,
            commands::hide_virtual_controllers,
            commands::forget_virtual_controllers,
            commands::open_link,
            commands::save_report,
            commands::start_fresh,
            commands::prompt_request,
            commands::answer_prompt,
            commands::open_steam_controller_settings,
        ])
        .register_asynchronous_uri_scheme_protocol("art", protocol::handle)
        .setup(move |app| {
            // Here, not earlier: a second start hands over to the running app before this.
            crate::log!("steambro {} started{}", crate::VERSION, if background { " in the background" } else { "" });
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
        crate::show_error(&crate::i18n::text("error.stopped", &[("error", &e.to_string())]));
    }
}

/// How often the tray app looks for new games, in turns of its 5-second loop: every 5 minutes.
const NEW_GAMES_EVERY: u32 = 60;

/// Looks for games installed since the last look (`State::seen`). A new game that isn't in the
/// current Steam account gets a notification, as the settings say, unless the user ignores it. The
/// first look only records what's there. An uninstalled game leaves the list, so installing it
/// again counts as new.
fn look_for_new_games(app: &AppHandle) {
    let shared = app.state::<Shared>();
    let settings = crate::settings::Settings::load();
    let library = shared.library(true);
    let user = crate::steam::Steam::locate().ok().and_then(|s| s.current_user().ok().flatten());
    let shortcuts = user.as_ref().and_then(|u| u.shortcuts().ok()).unwrap_or_default();
    let mut state = shared.state.lock().unwrap();
    if shared.starting_fresh.load(std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    let first = state.seen.is_none();
    let mut seen = state.seen.take().unwrap_or_default();
    seen.retain(|key| !library.is_uninstalled(key));
    let new: Vec<&crate::library::Game> = library.games.iter().filter(|g| !seen.contains(&g.key())).collect();
    let not_in_steam: Vec<String> = new
        .iter()
        .filter(|g| !settings.ignored.contains(&g.key()))
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
    if first || not_in_steam.is_empty() || !settings.new_game_notices {
        return;
    }
    crate::log!("new games, not in Steam: {}", not_in_steam.join(", "));
    let (title, text) = match not_in_steam.as_slice() {
        [one] => (crate::i18n::text("notice.one_installed", &[("name", one)]), crate::i18n::text("notice.one_add", &[])),
        many => (crate::i18n::plural("notice.many_installed", many.len(), &[]), crate::i18n::text("notice.many_add", &[("names", &many.join(", "))])),
    };
    notify::show(app, &title, &text);
}

/// Shows the window. When it comes back from the tray, the window looks for new games, because
/// WebView2 doesn't tell the page that it was hidden, and steambro checks the stores for new
/// artwork (`refresh_art`). At the first start, `start_of_run` checks once it found the games.
pub fn show_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        if matches!(window.is_visible(), Ok(false)) {
            let _ = app.emit("window-shown", ());
            if app.state::<Shared>().library.read().unwrap().is_some() {
                refresh_art(app);
            }
        }
        apply_text_size(&window);
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

/// The text size for the window, in percent: the user's, or Auto for the window's monitor.
pub fn text_size(window: &tauri::WebviewWindow) -> (u16, u16) {
    let width = window.current_monitor().ok().flatten().map_or(1920, |m| m.size().width);
    let auto = crate::settings::auto_text_size(width);
    (crate::settings::Settings::load().text_size.unwrap_or(auto), auto)
}

/// Zooms the window's page to its text size, as the browser's zoom does: text, images, and layout
/// grow together, and the page's coordinates stay right.
pub fn apply_text_size(window: &tauri::WebviewWindow) {
    let (size, _) = text_size(window);
    let zoom = f64::from(size) / 100.0;
    if let Err(e) = window.set_zoom(zoom) {
        crate::log!("couldn't set the text size: {e}");
    }
    fit_window(window, zoom);
}

/// The window's size at 100 % (tauri.conf.json), which zoom makes room for.
const WINDOW: (f64, f64) = (1180.0, 760.0);
const WINDOW_MIN: (f64, f64) = (760.0, 520.0);

/// Zoom leaves the page less room in the same window, so the window grows to its size at 100 %
/// times the zoom, up to 90 % of its screen. It never shrinks, and a maximized window stays.
fn fit_window(window: &tauri::WebviewWindow, zoom: f64) {
    use tauri::LogicalSize;
    let (Ok(inner), Ok(scale), Some(monitor)) = (window.inner_size(), window.scale_factor(), window.current_monitor().ok().flatten()) else { return };
    let screen = monitor.size().to_logical::<f64>(scale);
    let _ = window.set_min_size(Some(LogicalSize::new((WINDOW_MIN.0 * zoom).min(screen.width * 0.9), (WINDOW_MIN.1 * zoom).min(screen.height * 0.9))));
    if window.is_maximized().unwrap_or(false) {
        return;
    }
    let now = inner.to_logical::<f64>(scale);
    let wanted = ((WINDOW.0 * zoom).min(screen.width * 0.9), (WINDOW.1 * zoom).min(screen.height * 0.9));
    if now.width + 1.0 < wanted.0 || now.height + 1.0 < wanted.1 {
        let _ = window.set_size(LogicalSize::new(now.width.max(wanted.0), now.height.max(wanted.1)));
        let _ = window.center();
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
    // No shadow: for an undecorated window with a shadow, tao takes the invisible resize border out
    // of the page, and a window that can't be resized has no invisible border, so that strip showed
    // as a white frame around the question. Windows 11 rounds the corners on request instead.
    let built = tauri::WebviewWindowBuilder::new(app, &label, tauri::WebviewUrl::App("prompt.html".into()))
        .initialization_script(format!("window.PROMPT_ID = '{id}';"))
        .title("steambro")
        .inner_size(560.0, 340.0)
        .resizable(false)
        .maximizable(false)
        .shadow(false)
        .theme(Some(tauri::Theme::Dark))
        .background_color(tauri::window::Color(22, 26, 35, 255))
        .decorations(false)
        .always_on_top(true)
        .center()
        .focused(true)
        .build();
    match built {
        Ok(window) => {
            let _ = window.set_focus();
            // Windows may keep the focus with Big Picture, where a controller's keys would go.
            if let Ok(hwnd) = window.hwnd() {
                let hwnd = windows::Win32::Foundation::HWND(hwnd.0 as _);
                round_corners(hwnd);
                if !crate::window::take_front(hwnd) {
                    crate::log!("the question shows on top, but Windows kept the focus elsewhere");
                }
            }
        }
        Err(e) => crate::log!("couldn't show the question: {e}"),
    }
}

/// Asks Windows 11 to round a window's corners, as it does for windows with a frame. Windows 10
/// doesn't know the attribute and keeps square corners.
fn round_corners(hwnd: windows::Win32::Foundation::HWND) {
    use windows::Win32::Graphics::Dwm::{DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmSetWindowAttribute};
    let preference = DWMWCP_ROUND;
    let _ = unsafe { DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, std::ptr::from_ref(&preference).cast(), std::mem::size_of_val(&preference) as u32) };
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
    if shared.starting_fresh.load(std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    for user in &users {
        match sync::forget_deleted(&mut state, user) {
            Ok(gone) if !gone.is_empty() => crate::log!("deleted in Steam, so forgotten: {}", gone.join(", ")),
            Ok(_) => {}
            Err(e) => crate::log!("couldn't check account {} for deleted shortcuts: {e}", user.id),
        }
        match sync::adopt(&mut state, user) {
            Ok(adopted) if !adopted.is_empty() => crate::log!("steambro's shortcuts without a record, taken back: {}", adopted.join(", ")),
            Ok(_) => {}
            Err(e) => crate::log!("couldn't look for steambro's shortcuts in account {}: {e}", user.id),
        }
        match sync::retry_pending(&library, &shared.cache, &mut state, user) {
            Ok(true) => crate::log!("tried pending artwork again for account {}", user.id),
            Ok(false) => {}
            Err(e) => crate::log!("couldn't retry artwork for account {}: {e}", user.id),
        }
    }
    drop(state);
    let _ = app.emit("library-changed", ());
    if app.get_webview_window("main").is_some_and(|w| w.is_visible().unwrap_or(false)) {
        refresh_art(app);
    }
}

/// When steambro last checked the stores for new artwork.
static ART_CHECKED: Mutex<Option<std::time::Instant>> = Mutex::new(None);
/// Opening the window again sooner than this doesn't ask the stores again.
const ART_CHECK_GAP: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// Checks the stores for new artwork in the background (`sync::refresh`), at most once in
/// `ART_CHECK_GAP`. The window shows the new images.
fn refresh_art(app: &AppHandle) {
    {
        let mut checked = ART_CHECKED.lock().unwrap();
        if checked.is_some_and(|at| at.elapsed() < ART_CHECK_GAP) {
            return;
        }
        *checked = Some(std::time::Instant::now());
    }
    let app = app.clone();
    std::thread::spawn(move || {
        let shared = app.state::<Shared>();
        let library = shared.library(false);
        let users = Steam::locate().and_then(|steam| steam.users()).unwrap_or_default();
        let stop = || shared.starting_fresh.load(std::sync::atomic::Ordering::SeqCst);
        let changed = sync::refresh(&library, &shared.cache, &shared.state, &users, &stop);
        if !changed.is_empty() {
            crate::log!("new artwork from the stores: {}", changed.join(", "));
            let _ = app.emit("art-changed", ());
        }
    });
}

//! The commands the window calls (`invoke` in ui/app.js). Each slow one runs on a worker thread,
//! because detection, downloads and closing Steam take time.

use percent_encoding::percent_decode_str;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use windows::Win32::Foundation::HWND;

use super::Shared;
use super::tray::Mood;
use crate::art::{Anchor, Crop, Slot};
use crate::controllers::{self, Kind};
use crate::library::Game;
use crate::settings::{self, GameSettings, LaunchSettings, Settings, StoreSettings};
use crate::steam::{Steam, SteamUser};
use crate::sync::{self, Placement, Request, Status, Step};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct View {
    steam: SteamView,
    games: Vec<GameView>,
    /// steambro's shortcuts in the current account whose game is gone from the PC.
    gone: Vec<GoneView>,
    warnings: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GoneView {
    key: String,
    /// The shortcut's name in Steam.
    name: String,
    app_id: u32,
    /// The store's ID and name, from the game key; empty for a store steambro no longer has.
    store: &'static str,
    source: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SteamView {
    installed: bool,
    running: bool,
    /// The current account; the games' status is for this one.
    account: Option<AccountView>,
    /// Every account that has signed in on this PC.
    accounts: Vec<AccountView>,
}

#[derive(Serialize)]
struct AccountView {
    id: u32,
    name: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GameView {
    key: String,
    name: String,
    source: &'static str,
    art_source: &'static str,
    /// `steambro`, `user` or `none`; see `sync::Status`.
    status: &'static str,
    /// A steambro shortcut that starts the game an older way, or runs steambro from an old path.
    outdated: bool,
    app_id: Option<u32>,
    folder: String,
    launch: String,
    slots: Vec<SlotView>,
    /// The store's ID; its behavior is in the settings view's `stores`.
    store: &'static str,
    /// An app rather than a game, like Netflix: the bridge is off unless its own setting says on.
    is_app: bool,
    /// This game's overrides of its store's behavior.
    game_settings: GameSettings,
    /// Steam Input is turned off in the game's properties in Steam.
    steam_input_off: bool,
    /// Steam's overlay is turned off in the game's properties in Steam.
    steam_overlay_off: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SlotView {
    slot: Slot,
    label: &'static str,
    /// The slot's width and height; `None` for slots that use the whole image.
    shape: Option<(u32, u32)>,
    placement: Option<Placement>,
    own: bool,
    crop: Option<Crop>,
    /// Where the default crop sits when `crop` is `None`.
    anchor: Anchor,
    /// Steam's grid folder has an image for this slot, served at `art/steam/<slot>/<app id>`.
    in_steam: bool,
}

#[tauri::command]
pub async fn library(app: AppHandle, refresh: bool) -> Result<View, String> {
    blocking(app, "read the library", move |shared| {
        let library = shared.library(refresh);
        let steam = Steam::locate().ok();
        let user = steam.as_ref().and_then(|s| s.current_user().ok().flatten());
        let accounts = steam.as_ref().and_then(|s| s.users().ok()).unwrap_or_default();
        let steambro = crate::steambro_exe().ok();
        let shortcuts = match &user {
            Some(user) => user.shortcuts()?,
            None => Vec::new(),
        };
        let input_off = user.as_ref().and_then(|u| u.input_settings().ok()).map(|i| i.off_for).unwrap_or_default();
        let settings = Settings::load();
        let state = shared.state.lock().unwrap();
        let games = library
            .games
            .iter()
            .map(|game| {
                let key = game.key();
                let status = user.as_ref().map_or(Status::NotAdded, |u| sync::status(&library, &state, u.id, &shortcuts, game));
                let app_id = match status {
                    Status::Steambro(id) | Status::User(id) => Some(id),
                    _ => None,
                };
                let tracked = user.as_ref().and_then(|u| state.find(u.id, &key)).filter(|_| matches!(status, Status::Steambro(_)));
                let slots = crate::art::Slot::ALL
                    .iter()
                    .map(|&slot| {
                        let choice = state.choice(&key, slot);
                        let in_steam = match (&user, app_id) {
                            (Some(u), Some(id)) => sync::existing_file(&u.grid_dir(), id, slot).is_some(),
                            _ => false,
                        };
                        SlotView {
                            slot,
                            label: slot.label(),
                            shape: slot.size(),
                            placement: tracked.and_then(|t| t.art.get(&slot).copied()),
                            own: choice.own,
                            anchor: sync::anchor(game, &choice, slot),
                            crop: choice.crop,
                            in_steam,
                        }
                    })
                    .collect();
                GameView {
                    name: game.name().to_string(),
                    source: game.source(),
                    art_source: game.art_source(),
                    status: match status {
                        Status::Steambro(_) => "steambro",
                        Status::User(_) => "user",
                        Status::NotAdded => "none",
                    },
                    outdated: match (status, &steambro) {
                        (Status::Steambro(id), Some(steambro)) => shortcuts.iter().find(|s| s.app_id == id).is_some_and(|s| sync::is_outdated(game, s, steambro)),
                        _ => false,
                    },
                    app_id,
                    folder: game.install_dir().display().to_string(),
                    launch: game.launch_description(),
                    slots,
                    store: game.store.id(),
                    is_app: game.store.is_app(game),
                    game_settings: settings.game(&key),
                    steam_input_off: app_id.is_some_and(|id| input_off.contains(&id)),
                    steam_overlay_off: app_id.is_some_and(|id| shortcuts.iter().any(|s| s.app_id == id && !s.allow_overlay)),
                    key,
                }
            })
            .collect();
        let gone = match &user {
            Some(user) => state
                .shortcuts
                .iter()
                .filter(|t| t.steam_user == user.id && library.is_uninstalled(&t.game))
                .filter_map(|t| {
                    let shortcut = shortcuts.iter().find(|s| s.app_id == t.app_id)?;
                    let store_id = t.game.split_once(':').map_or(t.game.as_str(), |(id, _)| id);
                    let store = crate::library::stores().iter().find(|s| s.id() == store_id);
                    Some(GoneView {
                        key: t.game.clone(),
                        name: shortcut.name.clone(),
                        app_id: shortcut.app_id,
                        store: store.map_or("", |s| s.id()),
                        source: store.map_or("", |s| s.name()),
                    })
                })
                .collect(),
            None => Vec::new(),
        };
        Ok(View {
            gone,
            steam: SteamView {
                installed: steam.is_some(),
                running: Steam::is_running(),
                account: user.map(|u| AccountView { id: u.id, name: u.name }),
                accounts: accounts.into_iter().map(|u| AccountView { id: u.id, name: u.name }).collect(),
            },
            games,
            warnings: library.warnings.clone(),
        })
    })
    .await
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeOutcome {
    /// Steam is running and the caller didn't allow a restart; nothing was changed.
    needs_restart: bool,
    /// The names of the games changed.
    changed: Vec<String>,
}

/// Adds games (`Request::Add`, which also updates outdated steambro shortcuts) or removes
/// steambro's shortcuts of games (`Request::Remove`), in the current account or in every account.
/// Steam has to be closed while its shortcuts change, so when it runs, the window first asks the
/// user and calls again with `restart_steam`. Artwork is fetched before Steam closes, so Steam is
/// closed only while files are written; when nothing needs changing, Steam isn't closed at all.
/// Progress goes to the window as events: `progress-game` with the game's key and its
/// `sync::Step`, and `progress-steam` with `closing`, `writing`, or `starting`.
#[tauri::command]
pub async fn change_steam(app: AppHandle, keys: Vec<String>, request: Request, all_accounts: bool, restart_steam: bool) -> Result<ChangeOutcome, String> {
    let events = app.clone();
    blocking(app, "change Steam's shortcuts", move |shared| {
        let steam = Steam::locate()?;
        let current = steam.current_user()?.ok_or("no Steam account has signed in on this PC yet")?;
        let users = if all_accounts { steam.users()? } else { vec![current] };
        let running = Steam::is_running();
        if running && !restart_steam {
            return Ok(ChangeOutcome { needs_restart: true, changed: Vec::new() });
        }
        let accounts: Vec<String> = users.iter().map(|u| u.name.clone().unwrap_or_else(|| u.id.to_string())).collect();
        crate::log!("{request:?}: {} in {}", keys.join(", "), accounts.join(", "));
        let mut report = |key: &str, step: Step| {
            match &step {
                Step::Added { later } => crate::log!("added {key}; {later} images to fetch later"),
                Step::Updated => crate::log!("updated the shortcut of {key}"),
                Step::Removed => crate::log!("removed {key}"),
                Step::AlreadyInSteam => crate::log!("{key} is already in Steam"),
                Step::NotInSteam => crate::log!("{key} has no steambro shortcut"),
                Step::Failed { error } => crate::log!("couldn't change {key}: {error}"),
                Step::Artwork { .. } | Step::Ready => {}
            }
            let _ = events.emit("progress-game", GameProgress { key, step: &step });
        };
        let library = shared.library(false);
        let mut state = shared.state.lock().unwrap();
        let prepared = sync::prepare(&library, &shared.cache, &state, &users, &keys, request, &mut report)?;
        if prepared.is_empty() {
            return Ok(ChangeOutcome { needs_restart: false, changed: Vec::new() });
        }
        if running {
            crate::log!("closing Steam");
            let _ = events.emit("progress-steam", "closing");
            steam.shut_down()?;
        }
        let _ = events.emit("progress-steam", "writing");
        let changed = sync::apply(&library, &shared.cache, &mut state, &users, &prepared, &mut report);
        // Start Steam again even when the change failed: the user had it open.
        if running {
            crate::log!("starting Steam");
            let _ = events.emit("progress-steam", "starting");
            steam.start()?;
        }
        Ok(ChangeOutcome { needs_restart: false, changed: changed? })
    })
    .await
}

#[derive(Clone, Serialize)]
struct GameProgress<'a> {
    key: &'a str,
    step: &'a Step,
}

#[tauri::command]
pub async fn set_crop(app: AppHandle, key: String, slot: Slot, crop: Option<Crop>) -> Result<(), String> {
    edit(app, move |shared, library, state, users| sync::set_crop(library, &shared.cache, state, users, &key, slot, crop)).await
}

#[tauri::command]
pub async fn reset_art(app: AppHandle, key: String, slot: Slot) -> Result<(), String> {
    edit(app, move |shared, library, state, users| sync::reset(library, &shared.cache, state, users, &key, slot)).await
}

/// The image file arrives as the raw request body; the game and slot as `x-game` (URL-encoded)
/// and `x-slot` headers.
#[tauri::command]
pub async fn replace_art(app: AppHandle, request: tauri::ipc::Request<'_>) -> Result<(), String> {
    let tauri::ipc::InvokeBody::Raw(bytes) = request.body() else { return Err("no image in the request".into()) };
    let header = |name: &str| request.headers().get(name).and_then(|v| v.to_str().ok()).map(String::from);
    let key = header("x-game").map(|k| percent_decode_str(&k).decode_utf8_lossy().into_owned()).ok_or("no game in the request")?;
    let slot = header("x-slot").and_then(|s| Slot::from_name(&s)).ok_or("no artwork slot in the request")?;
    let bytes = bytes.clone();
    edit(app, move |shared, library, state, users| sync::replace(library, &shared.cache, state, users, &key, slot, &bytes)).await
}

/// Asks for an exe with Windows' file dialog and adds it as a game (`library::exe`). Returns its game
/// key, or `None` when the user cancelled.
#[tauri::command]
pub async fn add_exe(app: AppHandle) -> Result<Option<String>, String> {
    // A window handle is a number that other threads may use; HWND itself isn't Send.
    let owner = app.get_webview_window("main").and_then(|w| w.hwnd().ok()).map(|h| h.0 as isize);
    blocking(app, "add the game", move |shared| {
        // The dialog needs a thread of its own: worker threads may already use COM's other model.
        let picked = std::thread::spawn(move || pick_exe(owner.map(|h| HWND(h as _))).map_err(|e| e.to_string())).join().map_err(|_| "the file dialog failed")?;
        let Some(exe) = picked? else { return Ok(None) };
        let key = crate::library::exe::add(&exe)?;
        shared.library(true);
        Ok(Some(key))
    })
    .await
}

/// Takes a game the user added by its exe out of the list.
#[tauri::command]
pub async fn forget_exe(app: AppHandle, key: String) -> Result<(), String> {
    blocking(app, "forget the game", move |shared| {
        let id = key.strip_prefix("exe:").ok_or("only games added by their exe can be forgotten")?;
        crate::library::exe::forget(id)?;
        shared.library(true);
        Ok(())
    })
    .await
}

/// Windows' file dialog for one exe, over `owner`.
fn pick_exe(owner: Option<HWND>) -> crate::Result<Option<std::path::PathBuf>> {
    use windows::Win32::Foundation::ERROR_CANCELLED;
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree};
    use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
    use windows::Win32::UI::Shell::{FileOpenDialog, IFileOpenDialog, SIGDN_FILESYSPATH};
    use windows::core::{HRESULT, w};
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
        dialog.SetTitle(w!("Add a game by its exe"))?;
        dialog.SetFileTypes(&[COMDLG_FILTERSPEC { pszName: w!("Programs"), pszSpec: w!("*.exe") }])?;
        match dialog.Show(owner) {
            Err(e) if e.code() == HRESULT::from_win32(ERROR_CANCELLED.0) => return Ok(None),
            result => result?,
        }
        let name = dialog.GetResult()?.GetDisplayName(SIGDN_FILESYSPATH)?;
        let path = name.to_string();
        CoTaskMemFree(Some(name.0 as _));
        Ok(Some(path?.into()))
    }
}

/// Shows `mood` on the tray icon. Quick, so it runs on the main thread.
#[tauri::command]
pub fn set_mood(app: AppHandle, mood: Mood) -> Result<(), String> {
    super::tray::set_mood(&app, mood).map_err(|e| e.to_string())
}

/// Opens the game's install folder in Explorer.
#[tauri::command]
pub async fn open_folder(app: AppHandle, key: String) -> Result<(), String> {
    blocking(app, "open the game's folder", move |shared| {
        let library = shared.library(false);
        let game: &Game = library.find(&key).ok_or("the game isn't installed any more")?;
        std::process::Command::new("explorer.exe").arg(game.install_dir()).spawn()?;
        Ok(())
    })
    .await
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsView {
    settings: Settings,
    driver: DriverView,
    /// Steam's controller settings in the current account; `None` without Steam or an account.
    steam_input: Option<SteamInputView>,
    /// The kinds of controllers connected now (`controllers::connected`).
    connected: Vec<Kind>,
    /// The kinds Steam has seen with the current account (`ControllerTypesUsed`).
    seen: Vec<Kind>,
    /// Some installed games need the bridge.
    bridge_games: bool,
    /// Every store, with what can be set for it.
    stores: Vec<StoreView>,
    /// Steam's overlay settings in the current account; `None` without Steam or an account.
    steam_overlay: Option<SteamOverlayView>,
    version: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SteamOverlayView {
    /// "Enable the Steam Overlay while in-game".
    enabled: bool,
    /// The key that opens it, like `Shift+Tab`; `None` when steambro can't read it.
    key: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StoreView {
    id: &'static str,
    name: &'static str,
    /// The store's own app that starts its games, like `Battle.net`.
    app: Option<&'static str>,
    /// Steam never sees the store's games, so the bridge always matters (Xbox).
    needs_bridge: bool,
    /// How many of its games are installed.
    games: usize,
    /// How many of those are apps, like Netflix.
    apps: usize,
    settings: StoreSettings,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DriverView {
    installed: bool,
    version: Option<String>,
    /// The bridge can reach the driver now.
    available: bool,
}

/// Each value: `None` for Steam's default, `"0"` off, else on (see `steam::InputSettings`).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SteamInputView {
    xbox: Option<String>,
    playstation: Option<String>,
    nintendo: Option<String>,
    generic: Option<String>,
}

/// What the settings window shows: the settings and the checks on the Steam Input bridge.
#[tauri::command]
pub async fn settings_view(app: AppHandle) -> Result<SettingsView, String> {
    blocking(app, "read the settings", |shared| {
        let settings = Settings::load();
        let library = shared.library(false);
        let driver = crate::driver::status();
        let user = Steam::locate().ok().and_then(|s| s.current_user().ok().flatten());
        let input = user.as_ref().and_then(|u| u.input_settings().ok());
        let overlay = user.as_ref().and_then(|u| u.overlay_settings().ok());
        Ok(SettingsView {
            steam_overlay: overlay.map(|o| SteamOverlayView {
                enabled: o.enabled,
                key: crate::overlay::Hotkey::parse(&o.key).map(|k| k.display()),
            }),
            version: env!("CARGO_PKG_VERSION"),
            driver: DriverView {
                installed: driver.installed,
                version: driver.version,
                available: driver.available,
            },
            seen: input.as_ref().map(|i| controllers::seen_by_steam(&i.types_used)).unwrap_or_default(),
            steam_input: input.map(|i| SteamInputView { xbox: i.xbox, playstation: i.playstation, nintendo: i.nintendo, generic: i.generic }),
            connected: controllers::connected(),
            bridge_games: library.games.iter().any(|g| g.store.needs_bridge()),
            stores: crate::library::stores()
                .iter()
                .map(|store| StoreView {
                    id: store.id(),
                    name: store.name(),
                    app: store.app_name(),
                    needs_bridge: store.needs_bridge(),
                    games: library.games.iter().filter(|g| g.store.id() == store.id()).count(),
                    apps: library.games.iter().filter(|g| g.store.id() == store.id() && g.store.is_app(g)).count(),
                    settings: settings.store(store.id()),
                })
                .collect(),
            settings,
        })
    })
    .await
}

/// Saves one store's behavior.
#[tauri::command]
pub async fn save_store_settings(app: AppHandle, store: String, settings: StoreSettings) -> Result<(), String> {
    blocking(app, "save the settings", move |_| {
        crate::log!("settings for {store}: {settings:?}");
        Settings::update(|all| {
            all.stores.insert(store, settings);
        })
    })
    .await
}

/// Saves one game's overrides of its store's behavior.
#[tauri::command]
pub async fn set_game_settings(app: AppHandle, key: String, settings: GameSettings) -> Result<(), String> {
    blocking(app, "save the settings", move |_| {
        crate::log!("settings for {key}: {settings:?}");
        Settings::update(|all| all.set_game(&key, settings))
    })
    .await
}

/// Saves whether the tray app tells about new games.
#[tauri::command]
pub async fn save_new_game_notices(app: AppHandle, on: bool) -> Result<(), String> {
    blocking(app, "save the settings", move |_| {
        crate::log!("settings: notifications about new games {}", if on { "on" } else { "off" });
        Settings::update(|all| all.new_game_notices = on)
    })
    .await
}

/// Saves how game launches follow every game.
#[tauri::command]
pub async fn save_launch_settings(app: AppHandle, launch: LaunchSettings) -> Result<(), String> {
    blocking(app, "save the settings", move |_| {
        crate::log!("settings for game launches: {launch:?}");
        Settings::update(|all| all.launch = launch)
    })
    .await
}

/// Opens steambro's license (`license`), its source (`source`), or the page with the ViGEmBus
/// driver's official installer (`driver`). The license is a file beside the exe in a download;
/// without it, as in a development build, the copy on GitHub opens.
#[tauri::command]
pub async fn open_link(app: AppHandle, what: String) -> Result<(), String> {
    blocking(app, "open the page", move |_| {
        const REPO: &str = "https://github.com/shalamowww/steambro";
        let target = match what.as_str() {
            "license" => {
                let local = std::env::current_exe()?.with_file_name("LICENSE.txt");
                if local.is_file() { local.into_os_string() } else { format!("{REPO}/blob/master/LICENSE").into() }
            }
            "source" => REPO.into(),
            "driver" => "https://steambro.app/driver".into(),
            _ => return Err(format!("{what} isn't something steambro can open").into()),
        };
        std::process::Command::new("explorer.exe").arg(target).spawn()?;
        Ok(())
    })
    .await
}

/// The question a game launch asks (src/prompt.rs), for the prompt window.
#[tauri::command]
pub async fn prompt_request(app: AppHandle, id: String) -> Result<crate::prompt::Request, String> {
    blocking(app, "read the question", move |_| crate::prompt::request(&id)).await
}

/// Answers a game launch's question. With `always`, the answer becomes the store's setting.
#[tauri::command]
pub async fn answer_prompt(app: AppHandle, id: String, restart: bool, always: bool) -> Result<(), String> {
    blocking(app, "answer the question", move |_| {
        let request = crate::prompt::request(&id)?;
        crate::log!("{}: {} the app{}", request.game, if restart { "restart" } else { "leave" }, if always { ", every time" } else { "" });
        if always {
            Settings::update(|all| {
                let mut store = all.store(&request.store);
                store.app_running = if restart { settings::AppRunning::Restart } else { settings::AppRunning::Leave };
                all.stores.insert(request.store.clone(), store);
            })?;
        }
        crate::prompt::answer(&id, &crate::prompt::Answer { restart, always })
    })
    .await
}

/// Opens Steam's controller settings, where the user turns on Steam Input for a kind of controller.
#[tauri::command]
pub async fn open_steam_controller_settings(app: AppHandle) -> Result<(), String> {
    blocking(app, "open Steam's controller settings", |_| Steam::locate()?.open_url("steam://settings/controller")).await
}

/// An artwork edit, applied in every account where the game is a steambro shortcut.
async fn edit(
    app: AppHandle,
    work: impl FnOnce(&Shared, &crate::library::Library, &mut sync::State, &[SteamUser]) -> crate::Result<()> + Send + 'static,
) -> Result<(), String> {
    blocking(app, "change artwork", move |shared| {
        let library = shared.library(false);
        let users = Steam::locate().and_then(|s| s.users()).unwrap_or_default();
        let mut state = shared.state.lock().unwrap();
        work(shared, &library, &mut state, &users)
    })
    .await
}

/// Runs `work` on a worker thread. A failure goes to the log as "couldn't <what>".
async fn blocking<T: Send + 'static>(app: AppHandle, what: &'static str, work: impl FnOnce(&Shared) -> crate::Result<T> + Send + 'static) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(move || work(&app.state::<Shared>()))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| {
            crate::log!("couldn't {what}: {e}");
            e.to_string()
        })
}

//! The commands the window calls (`invoke` in ui/app.js). Each slow one runs on a worker thread,
//! because detection, downloads and closing Steam take time.

use std::collections::BTreeMap;

use percent_encoding::percent_decode_str;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};
use windows::Win32::Foundation::HWND;

use super::Shared;
use super::tray::Mood;
use crate::art::{Anchor, Crop, Slot};
use crate::controllers::{self, Kind};
use crate::library::{Game, Unseen};
use crate::plan::{self, Behavior, Inherited};
use crate::settings::{self, Knobs, LaunchSettings, Overrides, Settings};
use crate::steam::{Steam, SteamUser};
use crate::sync::{self, Placement, Request, Status, Step};

/// The library as the window shows it, for the current account.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct View {
    steam: SteamView,
    /// Every store, in the order the window shows them (`library::stores`).
    stores: Vec<StoreName>,
    games: Vec<GameView>,
    /// steambro's shortcuts in the current account whose game is gone from the PC.
    gone: Vec<GoneView>,
    /// Changes that wait for Steam to restart (`sync::Pending`), one per game, in any account.
    pending: Vec<PendingView>,
    warnings: Vec<String>,
}

#[derive(Serialize)]
struct PendingView {
    key: String,
    name: String,
    request: Request,
}

#[derive(Serialize)]
struct StoreName {
    id: &'static str,
    name: &'static str,
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
    /// The shortcut's artwork in Steam. The window shows Steam's images of a gone game.
    slots: Vec<SlotView>,
    /// The user ignores it (`Settings::ignored`).
    ignored: bool,
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
    /// The user's choices for this game.
    game_settings: Overrides,
    /// What the game gets where it has no choice of its own (plan.rs).
    inherited: Inherited,
    /// Steam Input is turned off in the game's properties in Steam.
    steam_input_off: bool,
    /// Steam's overlay is turned off in the game's properties in Steam.
    steam_overlay_off: bool,
    /// The user ignores it (`Settings::ignored`).
    ignored: bool,
    /// When steambro added it to Steam in the current account, in seconds since 1970.
    added: Option<u64>,
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
    /// The slot takes the store's newer images (`ArtChoice::follows`).
    follows: bool,
    /// The store can fill the slot at all (`Store::art_slots`), so the window offers its updates.
    fillable: bool,
    /// Where the store's image for the slot comes from, for display, like `Microsoft Store`.
    source: &'static str,
    crop: Option<Crop>,
    /// Where the default crop sits when `crop` is `None`.
    anchor: Anchor,
    /// Steam's grid folder has an image for this slot, served at `art/steam/<slot>/<app id>`.
    in_steam: bool,
}

/// Every game and its place in the current account. With `refresh`, the stores are asked again;
/// without, the library detected last is used.
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
                            follows: choice.follows(),
                            fillable: game.store_fills(slot),
                            source: game.slot_source(slot),
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
                    inherited: plan::inherited(&settings, game),
                    steam_input_off: app_id.is_some_and(|id| input_off.contains(&id)),
                    steam_overlay_off: app_id.is_some_and(|id| shortcuts.iter().any(|s| s.app_id == id && !s.allow_overlay)),
                    ignored: settings.ignored.contains(&key),
                    added: tracked.and_then(|t| t.added),
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
                    let slots = Slot::ALL
                        .iter()
                        .map(|&slot| SlotView {
                            slot,
                            label: slot.label(),
                            shape: slot.size(),
                            placement: t.art.get(&slot).copied(),
                            own: false,
                            follows: false,
                            fillable: false,
                            source: store.map_or("", |s| s.art_source()),
                            crop: None,
                            anchor: Anchor::Center,
                            in_steam: sync::existing_file(&user.grid_dir(), shortcut.app_id, slot).is_some(),
                        })
                        .collect();
                    Some(GoneView {
                        key: t.game.clone(),
                        name: shortcut.name.clone(),
                        app_id: shortcut.app_id,
                        store: store.map_or("", |s| s.id()),
                        source: store.map_or("", |s| s.name()),
                        slots,
                        ignored: settings.ignored.contains(&t.game),
                    })
                })
                .collect(),
            None => Vec::new(),
        };
        let mut pending: Vec<PendingView> = Vec::new();
        for q in sync::Pending::load().changes {
            if !pending.iter().any(|p| p.key == q.key) {
                let name = library.find(&q.key).map_or_else(|| gone.iter().find(|g| g.key == q.key).map_or_else(|| q.key.clone(), |g| g.name.clone()), |g| g.name().to_string());
                pending.push(PendingView { key: q.key, name, request: q.request });
            }
        }
        Ok(View {
            stores: crate::library::stores().iter().map(|s| StoreName { id: s.id(), name: s.name() }).collect(),
            pending,
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
    /// Steam is running and the caller allowed neither a restart nor waiting; nothing was changed.
    needs_restart: bool,
    /// The names of the games changed.
    changed: Vec<String>,
    /// The changes now waiting for Steam to restart, these and earlier ones.
    waiting: usize,
    /// steambro started Steam again after the change.
    steam_started: bool,
}

/// How long after steambro stopped waiting for Steam to close a closed Steam still counts as
/// closed by that request. Later, the user closed it.
const CLOSED_LATE: std::time::Duration = std::time::Duration::from_secs(600);

/// Whether Steam runs now, and whether a change starts it afterwards: when it runs, or when it
/// closed late after steambro stopped waiting for it (`Shared::steam_closing`).
fn steam_open(shared: &Shared) -> (bool, bool) {
    let running = Steam::is_running();
    let asked = shared.steam_closing.lock().unwrap().take();
    let late = closed_late(running, asked.map(|at| at.elapsed()));
    if late {
        crate::log!("Steam has closed since steambro stopped waiting for it; it starts again after this change");
    }
    (running, running || late)
}

/// Steam is closed, and steambro stopped waiting for it to close this long ago, not longer than
/// `CLOSED_LATE`.
fn closed_late(running: bool, asked: Option<std::time::Duration>) -> bool {
    !running && asked.is_some_and(|ago| ago <= CLOSED_LATE)
}

/// `Steam::shut_down`, remembering a Steam that didn't close in time (`steam_open`).
fn close_steam(shared: &Shared, steam: &Steam) -> crate::Result<()> {
    steam.shut_down().inspect_err(|_| *shared.steam_closing.lock().unwrap() = Some(std::time::Instant::now()))
}

/// Adds the games in `add` (`Request::Add`, which also updates outdated steambro shortcuts) and
/// removes steambro's shortcuts of the games in `remove` (`Request::Remove`), in the current
/// account or in every account. Both go in one change, so Steam closes once. Steam has to be
/// closed while its shortcuts change, so when it runs, the window first asks the user and calls
/// again with `restart_steam`, or with `later`: then the artwork is fetched and the changes wait
/// (`sync::Pending`) until a change restarts Steam. Every change that writes Steam's shortcuts
/// writes the waiting ones too, so the window calls this with no games to restart Steam for them.
/// A game's newer request replaces the one that waits for it. Artwork is fetched before Steam
/// closes, so Steam is closed only while files are written; when nothing needs changing, Steam
/// isn't closed at all. When Steam doesn't close (`Steam::shut_down`), nothing changes: the
/// requests wait like the "later" ones, and the error says so. A Steam that closed after steambro
/// stopped waiting starts again with the next change (`steam_open`). Progress goes to the window
/// as events: `progress-game` with the game's key and its `sync::Step`, and `progress-steam` with
/// `closing`, `writing`, or `starting`.
#[tauri::command]
pub async fn change_steam(app: AppHandle, add: Vec<String>, remove: Vec<String>, all_accounts: bool, restart_steam: bool, later: bool) -> Result<ChangeOutcome, String> {
    let events = app.clone();
    blocking(app, "change Steam's shortcuts", move |shared| {
        let steam = Steam::locate()?;
        let current = steam.current_user()?.ok_or("no Steam account has signed in on this PC yet")?;
        let users = if all_accounts { steam.users()? } else { vec![current] };
        let running = Steam::is_running();
        let mut pending = sync::Pending::load();
        if running && !restart_steam && !later {
            return Ok(ChangeOutcome { needs_restart: true, changed: Vec::new(), waiting: pending.changes.len(), steam_started: false });
        }
        let accounts: Vec<String> = users.iter().map(|u| u.name.clone().unwrap_or_else(|| u.id.to_string())).collect();
        for (request, keys) in [("add", &add), ("remove", &remove)] {
            if !keys.is_empty() {
                crate::log!("{request}: {} in {}", keys.join(", "), accounts.join(", "));
            }
        }
        let requests: Vec<(String, Request)> = add.into_iter().map(|key| (key, Request::Add)).chain(remove.into_iter().map(|key| (key, Request::Remove))).collect();
        let mut report = |key: &str, step: Step| {
            match &step {
                Step::Added { later } => crate::log!("added {key}; {later} images to fetch later"),
                Step::Updated => crate::log!("updated the shortcut of {key}"),
                Step::Removed => crate::log!("removed {key}"),
                Step::AlreadyInSteam => crate::log!("{key} is already in Steam"),
                Step::NotInSteam => crate::log!("{key} has no steambro shortcut"),
                Step::Failed { error } => crate::log!("couldn't change {key}: {error}"),
                Step::Artwork { .. } | Step::Ready | Step::Queued => {}
            }
            let _ = events.emit("progress-game", GameProgress { key, step: &step });
        };
        let library = shared.library(false);
        let mut state = shared.state.lock().unwrap();
        let mut prepared = sync::prepare(&library, &shared.cache, &state, &users, &requests, &mut report)?;
        // The user's newer requests replace what waits for the same games.
        let user_ids: Vec<u32> = users.iter().map(|u| u.id).collect();
        let keys: Vec<&str> = requests.iter().map(|(key, _)| key.as_str()).collect();
        pending.forget(&user_ids, &keys);
        // Also with Steam closed: the window takes a waiting change back this way, with the opposite
        // request, which then queues nothing.
        if later {
            let mut queued: Vec<&str> = Vec::new();
            for (user, key, request) in prepared.requests() {
                pending.queue(user, key, request);
                if !queued.contains(&key) {
                    queued.push(key);
                }
            }
            pending.save()?;
            for key in queued {
                crate::log!("{key} waits for Steam to restart");
                let _ = events.emit("progress-game", GameProgress { key, step: &Step::Queued });
            }
            return Ok(ChangeOutcome { needs_restart: false, changed: Vec::new(), waiting: pending.changes.len(), steam_started: false });
        }
        // Steam closes, or is closed: what waits goes in now too.
        let mut users = users;
        if !pending.changes.is_empty() {
            let accounts = steam.users()?;
            for id in pending.users() {
                let Some(user) = accounts.iter().find(|u| u.id == id) else { continue };
                crate::log!("also the changes that waited: {}", pending.requests_for(id).iter().map(|(key, _)| key.as_str()).collect::<Vec<_>>().join(", "));
                let waited = sync::prepare(&library, &shared.cache, &state, std::slice::from_ref(user), &pending.requests_for(id), &mut report)?;
                prepared.merge(waited);
                if !users.iter().any(|u| u.id == id) {
                    users.push(user.clone());
                }
            }
        }
        if prepared.is_empty() {
            pending.changes.clear();
            pending.save()?;
            return Ok(ChangeOutcome { needs_restart: false, changed: Vec::new(), waiting: 0, steam_started: false });
        }
        let (running, start_after) = steam_open(shared);
        if running {
            crate::log!("closing Steam");
            let _ = events.emit("progress-steam", "closing");
            if let Err(e) = close_steam(shared, &steam) {
                // Steam still runs, so nothing changed: the requests wait, as with "later", and go
                // in with the next change that finds Steam closed or closes it.
                let mut queued: Vec<&str> = Vec::new();
                for (user, key, request) in prepared.requests() {
                    pending.queue(user, key, request);
                    if !queued.contains(&key) {
                        queued.push(key);
                    }
                }
                if let Err(e) = pending.save() {
                    crate::log!("couldn't keep the changes that wait: {e}");
                }
                crate::log!("Steam didn't close, so {} changes wait for its next restart", pending.changes.len());
                for key in queued {
                    let _ = events.emit("progress-game", GameProgress { key, step: &Step::Queued });
                }
                return Err(format!("{e}. Nothing changed in Steam; your changes wait until Steam restarts").into());
            }
        }
        let _ = events.emit("progress-steam", "writing");
        let changed = sync::apply(&library, &shared.cache, &mut state, &users, &prepared, &mut report);
        if changed.is_ok() {
            pending.changes.clear();
            pending.save()?;
        }
        hide_virtual_controllers_in(&steam);
        // Start Steam again even when the change failed: the user had it open.
        if start_after {
            crate::log!("starting Steam");
            let _ = events.emit("progress-steam", "starting");
            steam.start()?;
        }
        Ok(ChangeOutcome { needs_restart: false, changed: changed?, waiting: pending.changes.len(), steam_started: start_after })
    })
    .await
}

/// While Steam is closed anyway: puts the bridge's virtual controllers in Steam's controller
/// blocklist, when their driver is installed and the user hasn't turned the "Steam ignores" switch
/// off. Otherwise Steam takes them for Xbox controllers and, with its Xbox controller support on,
/// gives them a layout and a place in its list. This also moves the entry of an earlier steambro, which
/// Steam never read, to where Steam reads it.
fn hide_virtual_controllers_in(steam: &Steam) {
    if !crate::driver::status().installed || !Settings::load().hide_virtual_controllers {
        return;
    }
    let (vendor, product) = crate::bridge::PAD_ID;
    match steam.ignore_controller(vendor, product) {
        Ok(true) => crate::log!("Steam ignores steambro's virtual controllers from its next start"),
        Ok(false) => {}
        Err(e) => crate::log!("couldn't hide steambro's virtual controllers from Steam: {e}"),
    }
}

/// Forgets the bridge's virtual controllers with these IDs, like `28de:1302#0`
/// (`bridge::forget_controllers`), so their numbers are free for new controllers.
#[tauri::command]
pub async fn forget_virtual_controllers(app: AppHandle, ids: Vec<String>) -> Result<(), String> {
    blocking(app, "forget the virtual controllers", move |_| {
        crate::bridge::forget_controllers(&ids)?;
        crate::log!("forgot the virtual controllers of {}", ids.join(", "));
        Ok(())
    })
    .await
}

/// Hides the bridge's virtual controllers from Steam (`hide_virtual_controllers_in`), or shows
/// them again, closing Steam for it and starting it again when it runs.
#[tauri::command]
pub async fn hide_virtual_controllers(app: AppHandle, hidden: bool) -> Result<(), String> {
    blocking(app, "change whether Steam ignores the virtual controllers", move |shared| {
        let steam = Steam::locate()?;
        let (running, start_after) = steam_open(shared);
        if running {
            crate::log!("closing Steam to change whether it ignores steambro's virtual controllers");
            close_steam(shared, &steam)?;
        }
        let (vendor, product) = crate::bridge::PAD_ID;
        let changed = if hidden { steam.ignore_controller(vendor, product) } else { steam.unignore_controllers(&[(vendor, product)]) };
        if start_after {
            steam.start()?;
        }
        let changed = changed?;
        // Remembered, so that the next change of Steam's shortcuts keeps the user's choice.
        Settings::update(|all| all.hide_virtual_controllers = hidden)?;
        if changed {
            crate::log!("Steam {} steambro's virtual controllers from now on", if hidden { "ignores" } else { "no longer ignores" });
        }
        Ok(())
    })
    .await
}

#[derive(Clone, Serialize)]
struct GameProgress<'a> {
    key: &'a str,
    step: &'a Step,
}

/// Sets the slot's crop, or clears it (`None`) so that the default crop applies.
#[tauri::command]
pub async fn set_crop(app: AppHandle, key: String, slot: Slot, crop: Option<Crop>) -> Result<(), String> {
    edit(app, move |shared, library, state, users| sync::set_crop(library, &shared.cache, state, users, &key, slot, crop)).await
}

/// Makes the store's image the slot's source again, with the default crop.
#[tauri::command]
pub async fn reset_art(app: AppHandle, key: String, slot: Slot) -> Result<(), String> {
    edit(app, move |shared, library, state, users| sync::reset(library, &shared.cache, state, users, &key, slot)).await
}

/// Makes an image file the slot's source. The file is the raw request body; the `x-game`
/// (URL-encoded) and `x-slot` headers name the game and the slot.
#[tauri::command]
pub async fn replace_art(app: AppHandle, request: tauri::ipc::Request<'_>) -> Result<(), String> {
    let tauri::ipc::InvokeBody::Raw(bytes) = request.body() else { return Err("no image in the request".into()) };
    let header = |name: &str| request.headers().get(name).and_then(|v| v.to_str().ok()).map(String::from);
    let key = header("x-game").map(|k| percent_decode_str(&k).decode_utf8_lossy().into_owned()).ok_or("no game in the request")?;
    let slot = header("x-slot").and_then(|s| Slot::from_name(&s)).ok_or("no artwork slot in the request")?;
    let bytes = bytes.clone();
    edit(app, move |shared, library, state, users| sync::replace(library, &shared.cache, state, users, &key, slot, &bytes, None)).await
}

/// Turns the slot's updates from the store on or off (`sync::set_follow`).
#[tauri::command]
pub async fn set_art_follow(app: AppHandle, key: String, slot: Slot, follow: bool) -> Result<(), String> {
    edit(app, move |shared, library, state, users| sync::set_follow(library, &shared.cache, state, users, &key, slot, follow)).await
}

/// Asks for an image with Windows' file dialog, titled `title`, and makes it the slot's source. The
/// dialog opens beside the slot's image (`sync::picker_folder`). Returns false when the user cancelled.
#[tauri::command]
pub async fn pick_art(app: AppHandle, key: String, slot: Slot, title: String) -> Result<bool, String> {
    let owner = main_window(&app);
    blocking(app, "change artwork", move |shared| {
        let library = shared.library(false);
        // While a change to Steam holds the state, the dialog opens where Windows would open it,
        // at once; only the replacing waits.
        let start = shared.state.try_lock().ok().and_then(|state| sync::picker_folder(&library, &shared.cache, &state, &key, slot));
        let images = Picker { title, kind: "PNG, JPEG, WebP", pattern: "*.png;*.jpg;*.jpeg;*.webp", start };
        let Some(file) = pick_file(owner, images)? else { return Ok(false) };
        let bytes = std::fs::read(&file)?;
        let users = Steam::locate().and_then(|s| s.users()).unwrap_or_default();
        let mut state = shared.state.lock().unwrap();
        not_starting_fresh(shared)?;
        sync::replace(&library, &shared.cache, &mut state, &users, &key, slot, &bytes, Some(file))?;
        Ok(true)
    })
    .await
}

/// Asks for an exe with Windows' file dialog and adds it as a game (`library::exe`). Returns its game
/// key, or `None` when the user cancelled.
#[tauri::command]
pub async fn add_exe(app: AppHandle) -> Result<Option<String>, String> {
    let owner = main_window(&app);
    blocking(app, "add the game", move |shared| {
        let programs = Picker { title: "Add a game by its exe".into(), kind: "Programs", pattern: "*.exe", start: None };
        let Some(exe) = pick_file(owner, programs)? else { return Ok(None) };
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

/// The main window's handle, as a number: worker threads may use it, and HWND itself isn't Send.
fn main_window(app: &AppHandle) -> Option<isize> {
    app.get_webview_window("main").and_then(|w| w.hwnd().ok()).map(|h| h.0 as isize)
}

/// What Windows' file dialog asks for.
struct Picker {
    title: String,
    /// The kind of file, like `Programs`, and its pattern, like `*.exe`.
    kind: &'static str,
    pattern: &'static str,
    /// The folder the dialog opens in, with the file to select there.
    start: Option<(std::path::PathBuf, Option<String>)>,
}

/// Windows' file dialog for one file, over the window `owner`. Returns `None` when the user
/// cancelled. The dialog runs on a thread of its own: worker threads may already use COM's other
/// model.
fn pick_file(owner: Option<isize>, picker: Picker) -> crate::Result<Option<std::path::PathBuf>> {
    std::thread::spawn(move || show_picker(owner.map(|h| HWND(h as _)), &picker).map_err(|e| e.to_string()))
        .join()
        .map_err(|_| "the file dialog failed")?
        .map_err(Into::into)
}

fn show_picker(owner: Option<HWND>, picker: &Picker) -> crate::Result<Option<std::path::PathBuf>> {
    use windows::Win32::Foundation::ERROR_CANCELLED;
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree};
    use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
    use windows::Win32::UI::Shell::{FileOpenDialog, IFileOpenDialog, IShellItem, SHCreateItemFromParsingName, SIGDN_FILESYSPATH};
    use windows::core::{HRESULT, HSTRING};
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
        dialog.SetTitle(&HSTRING::from(picker.title.as_str()))?;
        let (kind, pattern) = (HSTRING::from(picker.kind), HSTRING::from(picker.pattern));
        dialog.SetFileTypes(&[COMDLG_FILTERSPEC { pszName: windows::core::PCWSTR(kind.as_ptr()), pszSpec: windows::core::PCWSTR(pattern.as_ptr()) }])?;
        if let Some((folder, file)) = &picker.start {
            // A folder the dialog can't open leaves it where Windows would open it.
            let item: windows::core::Result<IShellItem> = SHCreateItemFromParsingName(&HSTRING::from(folder.as_os_str()), None);
            if let Ok(item) = item {
                let _ = dialog.SetFolder(&item);
            }
            if let Some(file) = file {
                let _ = dialog.SetFileName(&HSTRING::from(file.as_str()));
            }
        }
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
    /// The table of controllers: each the bridge knows, and each kind connected now or used with
    /// the current account before (`controller_rows`).
    controllers: Vec<ControllerRowView>,
    /// An installed game that Steam never sees uses the bridge (`Unseen::Always`), so a controller
    /// that isn't an Xbox controller reaches it only through the driver.
    bridge_games: bool,
    /// A connected controller that won't reach a game that uses the bridge; `None` when every
    /// controller is fine, or no installed game uses the bridge.
    controller_alert: Option<ControllerAlertView>,
    /// Every store, with what can be set for it.
    stores: Vec<StoreView>,
    /// Steam's overlay settings in the current account; `None` without Steam or an account.
    steam_overlay: Option<SteamOverlayView>,
    /// The text size Auto gives on the window's monitor, in percent.
    auto_text_size: u16,
    version: &'static str,
    /// steambro runs from an installed package (`crate::packaged`), so the user has it from the
    /// Store; the window then shows no way to give something back.
    packaged: bool,
    /// The languages steambro has, `(code, name)`, and the one Auto gives: Windows' display language.
    languages: &'static [(&'static str, &'static str)],
    auto_language: &'static str,
}

#[derive(Serialize)]
pub struct Strings {
    language: &'static str,
    strings: &'static serde_json::Value,
    /// English, for a text the language lacks; none when the language is English.
    fallback: Option<&'static serde_json::Value>,
}

/// The window's texts in the language in use (src/i18n.rs), read before the first render and
/// again when the language setting changes.
#[tauri::command]
pub fn strings() -> Strings {
    let language = crate::i18n::language();
    Strings { language, strings: crate::i18n::strings(language), fallback: (language != "en").then(|| crate::i18n::strings("en")) }
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
    /// When Steam can't see the store's games, so that steambro stands in for Steam.
    unseen: Unseen,
    /// How many of its games are installed.
    games: usize,
    /// How many of those are apps, like Netflix.
    apps: usize,
    /// The user's choices for the store.
    settings: Overrides,
    /// What its settings card shows: those choices over the defaults (plan.rs).
    values: Knobs,
    /// The store may miss a launch request, so a launch asks it again after the nudge time
    /// (`Store::asks_again`); its page shows that time.
    asks_again: bool,
    /// The store's app closes by itself once a game starts, as its own settings say
    /// (`Store::app_closes_itself`), so the hand-over changes nothing; the window says so.
    closes_itself: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DriverView {
    installed: bool,
    version: Option<String>,
    /// The bridge can reach the driver now.
    available: bool,
    /// Steam's controller blocklist has the bridge's virtual controllers (`bridge::PAD_ID`, one
    /// model for all of them); `None` without Steam, and while the driver isn't available: there
    /// are no virtual controllers for Steam to take then, so the window offers no switch.
    hidden_from_steam: Option<bool>,
    /// The virtual controllers' model, `vendor:product` in hex like the table's IDs.
    pad_model: String,
}

/// A row of the settings window's table of controllers (`controller_rows`): a controller the
/// bridge knows, or a kind of controller connected now or used with Steam before.
#[derive(Serialize, PartialEq, Eq, Debug)]
#[serde(rename_all = "camelCase")]
struct ControllerRowView {
    /// The bridge's name for it, like `28de:1302#0`, when it drives a virtual controller; else
    /// its kind.
    id: String,
    /// `vendor:product` in hex, like `28de:1302`, when known.
    model: Option<String>,
    kind: Kind,
    /// What the model is, when steambro knows the IDs.
    name: Option<&'static str>,
    /// A controller of its kind is connected now.
    connected: bool,
    /// The bridge copies it for Xbox games (`Kind::bridged`).
    bridged: bool,
    /// Steam Input is on for its kind; `None` for Steam's default, or without Steam.
    steam_input: Option<bool>,
    /// The number of the virtual controller it drives (`bridge::Numbers`), when it has one.
    number: Option<u32>,
    /// It drove the first virtual controller in the last launch, so its number waits for it.
    last: bool,
}

/// `vendor` and `product` from `28de:1302` or `28de/1302`, hex, with or without `0x`.
fn parse_model(model: &str) -> Option<(u16, u16)> {
    let hex = |s: &str| u16::from_str_radix(s.trim().trim_start_matches("0x").trim_start_matches("0X"), 16).ok();
    let (vendor, product) = model.split_once([':', '/'])?;
    Some((hex(vendor)?, hex(product)?))
}

/// The table's rows, in this order: the controllers the bridge knows, by number; then each kind
/// connected now or used with Steam before that none of them is.
fn controller_rows(known: Vec<crate::bridge::KnownController>, checks: &[controllers::Check]) -> Vec<ControllerRowView> {
    let check = |kind| checks.iter().find(|c| c.kind == kind);
    let mut rows: Vec<ControllerRowView> = known
        .into_iter()
        .map(|known| {
            let model = known.id.split('#').next().unwrap_or_default().to_string();
            let ids = parse_model(&model);
            let kind = ids.map_or(Kind::Other, |(vendor, _)| Kind::of_vendor(vendor));
            ControllerRowView {
                name: ids.and_then(|(vendor, product)| controller_name(vendor, product)),
                model: ids.map(|_| model),
                kind,
                connected: check(kind).is_some_and(|c| c.connected),
                bridged: kind.bridged(),
                steam_input: check(kind).and_then(|c| c.steam_input),
                number: Some(known.number),
                last: known.last,
                id: known.id,
            }
        })
        .collect();
    let known_kinds: Vec<Kind> = rows.iter().map(|r| r.kind).collect();
    for c in checks.iter().filter(|c| !known_kinds.contains(&c.kind)) {
        rows.push(ControllerRowView {
            id: format!("{:?}", c.kind).to_lowercase(),
            model: None,
            kind: c.kind,
            name: None,
            connected: c.connected,
            bridged: c.bridged,
            steam_input: c.steam_input,
            number: None,
            last: false,
        });
    }
    rows
}

/// Why a connected controller won't reach an installed game that uses the bridge, for the
/// controller alert above the library.
#[derive(Serialize, PartialEq, Eq, Debug)]
#[serde(rename_all = "camelCase")]
struct ControllerAlertView {
    kind: Kind,
    reason: AlertReason,
}

#[derive(Serialize, PartialEq, Eq, Debug)]
#[serde(rename_all = "camelCase")]
enum AlertReason {
    /// The ViGEmBus driver isn't installed.
    DriverMissing,
    /// The driver is installed but doesn't answer, as until Windows restarts.
    DriverAsleep,
    /// Steam Input is off for the controller's kind in Steam's settings.
    SteamInputOff,
}

/// The first connected controller that the bridge copies (`Kind::bridged`) and that won't reach a
/// game, when an installed game uses the bridge (`bridge_games`): without the driver, with a
/// driver that doesn't answer, or with Steam Input off for its kind. An Xbox controller is never
/// one: the game reads it itself.
fn controller_alert(bridge_games: bool, checks: &[controllers::Check], driver: &crate::driver::Status) -> Option<ControllerAlertView> {
    if !bridge_games {
        return None;
    }
    checks.iter().filter(|c| c.connected && c.bridged).find_map(|c| {
        let reason = if !driver.installed {
            AlertReason::DriverMissing
        } else if !driver.available {
            AlertReason::DriverAsleep
        } else if c.steam_input == Some(false) {
            AlertReason::SteamInputOff
        } else {
            return None;
        };
        Some(ControllerAlertView { kind: c.kind, reason })
    })
}

/// Names for the IDs Steam's blocklist most often has.
fn controller_name(vendor: u16, product: u16) -> Option<&'static str> {
    Some(match (vendor, product) {
        (0x045E, 0x028E) => "Xbox 360 controller",
        (0x045E, 0x02D1 | 0x02DD | 0x02E0 | 0x02EA | 0x02FD | 0x0B00 | 0x0B12 | 0x0B13 | 0x0B20 | 0x0B22) => "Xbox controller",
        (0x054C, 0x05C4 | 0x09CC | 0x0BA0) => "DualShock 4",
        (0x054C, 0x0CE6) => "DualSense",
        (0x054C, 0x0DF2) => "DualSense Edge",
        (0x057E, 0x2009) => "Switch Pro Controller",
        // The ID Steam gives the Steam Controller in its virtual gamepad file.
        (0x28DE, 0x1302) => "Steam Controller",
        (0x28DE, _) => "Valve controller",
        _ => return None,
    })
}

/// What the settings window shows: the settings and the checks on the Steam Input bridge.
#[tauri::command]
pub async fn settings_view(app: AppHandle) -> Result<SettingsView, String> {
    let auto_text_size = app.get_webview_window("main").map_or(100, |w| super::text_size(&w).1);
    blocking(app, "read the settings", move |shared| {
        let settings = Settings::load();
        let library = shared.library(false);
        let driver = crate::driver::status();
        let user = Steam::locate().ok().and_then(|s| s.current_user().ok().flatten());
        let input = user.as_ref().and_then(|u| u.input_settings().ok());
        let overlay = user.as_ref().and_then(|u| u.overlay_settings().ok());
        let checks = controllers::checks(input.as_ref());
        let bridge_games = library.games.iter().any(|g| g.store.unseen() == Unseen::Always && Behavior::of(&settings, g).knobs.bridge);
        let hidden_from_steam = Steam::locate().ok().map(|s| s.ignored_controllers().contains(&crate::bridge::PAD_ID));
        Ok(SettingsView {
            steam_overlay: overlay.map(|o| SteamOverlayView {
                enabled: o.enabled,
                key: crate::overlay::Hotkey::parse(&o.key).map(|k| k.display()),
            }),
            version: crate::VERSION,
            packaged: crate::packaged(),
            languages: crate::i18n::LANGUAGES,
            auto_language: crate::i18n::system_language(),
            auto_text_size,
            controller_alert: controller_alert(bridge_games, &checks, &driver),
            controllers: controller_rows(crate::bridge::known_controllers(), &checks),
            driver: DriverView {
                installed: driver.installed,
                version: driver.version,
                available: driver.available,
                hidden_from_steam: hidden_from_steam.filter(|_| driver.available),
                pad_model: format!("{:04x}:{:04x}", crate::bridge::PAD_ID.0, crate::bridge::PAD_ID.1),
            },
            bridge_games,
            stores: crate::library::stores()
                .iter()
                .map(|store| StoreView {
                    id: store.id(),
                    name: store.name(),
                    app: store.app_name(),
                    unseen: store.unseen(),
                    games: library.games.iter().filter(|g| g.store.id() == store.id()).count(),
                    apps: library.games.iter().filter(|g| g.store.id() == store.id() && g.store.is_app(g)).count(),
                    settings: settings.store(store.id()),
                    values: plan::store_values(&settings, *store),
                    asks_again: store.asks_again(),
                    closes_itself: store.app_closes_itself(),
                })
                .collect(),
            settings,
        })
    })
    .await
}

/// What the settings window saves: everything it shows, all at once.
#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SettingsChange {
    new_game_notices: bool,
    launch: LaunchSettings,
    /// The user's choices for each store, by store ID.
    stores: BTreeMap<String, Overrides>,
    /// In percent; none for Auto.
    text_size: Option<u16>,
    /// A code from `i18n::LANGUAGES`; none for Auto.
    #[serde(default)]
    language: Option<String>,
}

/// Saves the settings window's changes. Launches and the tray app read the settings each time they
/// need them, so a change applies without a restart; the window's text size applies at once, and
/// a new language relabels the tray menu (the window reads its texts again itself).
#[tauri::command]
pub async fn save_settings(app: AppHandle, change: SettingsChange) -> Result<(), String> {
    let window = app.get_webview_window("main");
    let handle = app.clone();
    blocking(app, "save the settings", move |_| {
        crate::log!("settings: {change:?}");
        let language = change.language.filter(|code| crate::i18n::LANGUAGES.iter().any(|(c, _)| c == code));
        let mut language_changed = false;
        Settings::update(|all| {
            all.new_game_notices = change.new_game_notices;
            all.launch = change.launch;
            all.text_size = change.text_size.map(|size| size.clamp(*settings::TEXT_SIZES.start(), *settings::TEXT_SIZES.end()));
            language_changed = all.language != language;
            all.language = language;
            for (id, choices) in change.stores {
                if crate::library::stores().iter().any(|s| s.id() == id) {
                    all.set_store(&id, choices);
                }
            }
        })?;
        if let Some(window) = window {
            super::apply_text_size(&window);
        }
        if language_changed {
            super::tray::relabel(&handle)?;
        }
        Ok(())
    })
    .await
}

/// Starts steambro again as on its first start, as if just installed. In Steam, in every account:
/// removes every shortcut that runs steambro, with its images (`sync::remove_all`), and takes the
/// bridge's virtual controllers out of Steam's controller blocklist; Steam closes for it and
/// starts again when it ran. Then it deletes steambro's settings, its record of the games and
/// their artwork, the artwork cache, the games seen, the waiting changes, and the controllers'
/// numbers, and restarts the tray app. The logs and the reports stay. The window clears its own
/// storage before it calls this.
#[tauri::command]
pub async fn start_fresh(app: AppHandle) -> Result<(), String> {
    blocking(app.clone(), "start fresh", |shared| {
        use std::sync::atomic::Ordering::SeqCst;
        // A look at the stores for new artwork or new games that runs now writes nothing any more,
        // and one that is writing finishes first.
        shared.starting_fresh.store(true, SeqCst);
        let state = shared.state.lock().unwrap();
        match fresh() {
            // The state stays locked until the tray app restarts, so that nothing waiting for it
            // writes steambro's old record back.
            Ok(()) => {
                std::mem::forget(state);
                Ok(())
            }
            Err(e) => {
                crate::log!("starting fresh stopped: {e}");
                shared.starting_fresh.store(false, SeqCst);
                Err(e)
            }
        }
    })
    .await?;
    app.restart();
}

/// Start fresh's work, while the state is locked: steambro's shortcuts out of Steam, then its files.
fn fresh() -> crate::Result<()> {
    {
        if let Ok(steam) = Steam::locate() {
            let running = Steam::is_running();
            if running {
                crate::log!("starting fresh: closing Steam");
                steam.shut_down()?;
            }
            for user in steam.users().unwrap_or_default() {
                match sync::remove_all(&user) {
                    Ok(keys) if !keys.is_empty() => crate::log!("starting fresh: removed from account {}: {}", user.id, keys.join(", ")),
                    Ok(_) => {}
                    Err(e) => crate::log!("starting fresh: couldn't remove steambro's shortcuts from account {}: {e}", user.id),
                }
            }
            if let Err(e) = steam.unignore_controllers(&[crate::bridge::PAD_ID]) {
                crate::log!("starting fresh: couldn't take steambro's virtual controllers out of Steam's blocklist: {e}");
            }
            if running {
                steam.start()?;
            }
        }
        let dir = crate::data_dir()?;
        crate::log!("starting fresh: deleting what's in {} but the logs and the reports", dir.display());
        for entry in std::fs::read_dir(&dir)?.flatten() {
            let name = entry.file_name().to_string_lossy().to_lowercase();
            if name.ends_with(".log") || name == "reports" {
                continue;
            }
            let path = entry.path();
            let removed = if path.is_dir() { std::fs::remove_dir_all(&path) } else { std::fs::remove_file(&path) };
            if let Err(e) = removed {
                crate::log!("couldn't delete {}: {e}", path.display());
            }
        }
        Ok(())
    }
}

/// Saves the user's choices for one game.
#[tauri::command]
pub async fn set_game_settings(app: AppHandle, key: String, settings: Overrides) -> Result<(), String> {
    blocking(app, "save the settings", move |_| {
        crate::log!("settings for {key}: {settings:?}");
        Settings::update(|all| all.set_game(&key, settings))
    })
    .await
}

/// Ignores the games, or stops ignoring them (`Settings::ignored`).
#[tauri::command]
pub async fn set_ignored(app: AppHandle, keys: Vec<String>, ignored: bool) -> Result<(), String> {
    blocking(app, "save the settings", move |_| {
        crate::log!("{}: {}", if ignored { "ignore" } else { "stop ignoring" }, keys.join(", "));
        Settings::update(|all| {
            for key in &keys {
                all.set_ignored(key, ignored);
            }
        })
    })
    .await
}

/// Opens steambro's license (`license`), its source (`source`), the page with the ViGEmBus
/// driver's official installer (`driver`), its page in the Microsoft Store (`store`), or Steam's
/// official download page (`steam`), for a PC without Steam. The license is a file beside the exe in a download; without it, as in a
/// development build, the copy on GitHub opens.
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
            "store" => "https://apps.microsoft.com/detail/9NQVZ9CVTG85".into(),
            "steam" => "https://store.steampowered.com/about/".into(),
            _ => return Err(format!("{what} isn't something steambro can open").into()),
        };
        std::process::Command::new("explorer.exe").arg(target).spawn()?;
        Ok(())
    })
    .await
}

/// Writes a report for a bug (src/report.rs) and shows it in Explorer.
#[tauri::command]
pub async fn save_report(app: AppHandle) -> Result<String, String> {
    blocking(app, "write the report", |_| {
        let path = crate::report::write()?;
        crate::report::show(&path)?;
        Ok(path.display().to_string())
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
                store.app_running = Some(if restart { settings::AppRunning::Restart } else { settings::AppRunning::Leave });
                all.set_store(&request.store, store);
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
        not_starting_fresh(shared)?;
        work(shared, &library, &mut state, &users)
    })
    .await
}

/// Start fresh is deleting steambro's files (`Shared::starting_fresh`): nothing may write them again.
fn not_starting_fresh(shared: &Shared) -> crate::Result<()> {
    if shared.starting_fresh.load(std::sync::atomic::Ordering::SeqCst) { Err("steambro is starting fresh".into()) } else { Ok(()) }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::KnownController;
    use crate::controllers::Check;
    use crate::driver::Status;

    #[test]
    fn a_steam_that_closed_late_starts_again() {
        let minutes = |n: u64| Some(std::time::Duration::from_secs(n * 60));
        assert!(closed_late(false, minutes(2)), "Steam closed after steambro stopped waiting");
        assert!(!closed_late(true, minutes(2)), "Steam still runs");
        assert!(!closed_late(false, minutes(11)), "the user closed Steam later");
        assert!(!closed_late(false, None), "steambro didn't ask Steam to close");
    }

    #[test]
    fn the_controller_alert_names_the_first_controller_that_wont_reach_the_game() {
        let check = |kind, connected, steam_input| Check { kind, connected, bridged: kind.bridged(), steam_input };
        let xbox = check(Kind::Xbox, true, Some(false));
        let steam = check(Kind::Steam, true, Some(true));
        let dual_sense = check(Kind::PlayStation, true, Some(false));
        let asleep = check(Kind::Nintendo, false, Some(false));
        let driver = |installed, available| Status { installed, version: None, available };
        let alert = |checks: &[Check], driver: &Status| controller_alert(true, checks, driver);
        assert_eq!(controller_alert(false, &[steam], &driver(false, false)), None, "no installed game uses the bridge");
        assert_eq!(alert(&[xbox], &driver(false, false)), None, "the game reads an Xbox controller itself");
        assert_eq!(alert(&[asleep], &driver(false, false)), None, "a controller used before but not connected");
        assert_eq!(alert(&[xbox, steam], &driver(false, false)), Some(ControllerAlertView { kind: Kind::Steam, reason: AlertReason::DriverMissing }));
        assert_eq!(alert(&[steam], &driver(true, false)), Some(ControllerAlertView { kind: Kind::Steam, reason: AlertReason::DriverAsleep }));
        assert_eq!(alert(&[steam, dual_sense], &driver(true, true)), Some(ControllerAlertView { kind: Kind::PlayStation, reason: AlertReason::SteamInputOff }));
        assert_eq!(alert(&[check(Kind::Other, true, None)], &driver(true, true)), None, "Steam's default for the kind isn't known to be off");
        assert_eq!(serde_json::to_string(&ControllerAlertView { kind: Kind::Steam, reason: AlertReason::DriverMissing }).unwrap(), r#"{"kind":"steam","reason":"driverMissing"}"#);
    }

    #[test]
    fn the_table_of_controllers_joins_the_bridges_and_steams() {
        let known = |id: &str, number, last| KnownController { id: id.into(), number, last };
        let check = |kind, connected, steam_input| Check { kind, connected, bridged: kind.bridged(), steam_input };
        let checks = [check(Kind::Steam, true, Some(true)), check(Kind::Xbox, true, Some(false)), check(Kind::PlayStation, false, None)];
        let rows = controller_rows(vec![known("28de:1302#0", 1, true), known("054c:0ce6#0", 2, false)], &checks);
        let short: Vec<_> = rows.iter().map(|r| (r.id.as_str(), r.model.as_deref(), r.kind, r.name, r.connected, r.steam_input, r.number)).collect();
        assert_eq!(
            short,
            [
                ("28de:1302#0", Some("28de:1302"), Kind::Steam, Some("Steam Controller"), true, Some(true), Some(1)),
                ("054c:0ce6#0", Some("054c:0ce6"), Kind::PlayStation, Some("DualSense"), false, None, Some(2)),
                ("xbox", None, Kind::Xbox, None, true, Some(false), None),
            ],
            "the bridge's by number, then a kind without one"
        );
        assert!(rows[0].last && rows[0].bridged && !rows[2].bridged, "the bridge copies all but Xbox controllers");
        let rows = controller_rows(vec![known("broken", 3, false)], &[]);
        assert_eq!((rows[0].model.as_deref(), rows[0].kind), (None, Kind::Other), "no model, so no name");
        assert_eq!(controller_rows(vec![known("28de:1205#0", 4, false)], &[])[0].name, Some("Valve controller"), "another product of Valve's");
        assert_eq!(
            serde_json::to_string(&controller_rows(vec![known("28de:1302#0", 1, true)], &checks)[0]).unwrap(),
            r#"{"id":"28de:1302#0","model":"28de:1302","kind":"steam","name":"Steam Controller","connected":true,"bridged":true,"steamInput":true,"number":1,"last":true}"#
        );
        assert_eq!((parse_model("0x45e/0x7EB0"), parse_model("28de:1302"), parse_model("xbox")), (Some((0x045E, 0x7EB0)), Some((0x28DE, 0x1302)), None));
    }
}

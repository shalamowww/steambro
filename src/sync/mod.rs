//! Puts games into Steam and keeps steambro's record of it: adds shortcuts, fills artwork slots
//! under the artwork rules, and applies the user's artwork edits.

mod pending;
mod state;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
pub use pending::Pending;
pub use state::{ArtChoice, Placement, State};
use state::TrackedShortcut;

use crate::art::cache::Cache;
use crate::art::{Anchor, Crop, Origin, Slot, logo, render};
use crate::library::{ArtOrigins, Game, Library};
use crate::steam::{NewShortcut, Shortcut, SteamUser};

/// Where a game stands in one Steam account.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    /// steambro added it; the shortcut has this app ID.
    Steambro(u32),
    /// A shortcut that steambro didn't make already starts the game.
    User(u32),
    NotAdded,
}

pub fn status(library: &Library, state: &State, user: u32, shortcuts: &[Shortcut], game: &Game) -> Status {
    if let Some(tracked) = state.find(user, &game.key())
        && shortcuts.iter().any(|s| s.app_id == tracked.app_id)
    {
        return Status::Steambro(tracked.app_id);
    }
    match shortcuts.iter().find(|s| library.is_launched_by(game, s)) {
        Some(shortcut) => Status::User(shortcut.app_id),
        None => Status::NotAdded,
    }
}

/// What `prepare` and `apply` report about a game while they work, for the window's progress view.
#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Step {
    /// Getting the slot's image, number `number` of `of`.
    Artwork { slot: Slot, number: usize, of: usize },
    /// Its artwork is ready; it waits until Steam is closed.
    Ready,
    /// Added to every chosen account that lacked it. `later` images couldn't be fetched; the next
    /// start tries them again.
    Added { later: usize },
    /// Its steambro shortcut started it the old way and now runs what steambro writes today.
    Updated,
    /// Its steambro shortcut is gone from every chosen account.
    Removed,
    /// Its artwork is ready, and the change waits for Steam to restart (`Pending`).
    Queued,
    /// Every chosen account has it already.
    AlreadyInSteam,
    /// No chosen account has a steambro shortcut for it.
    NotInSteam,
    Failed { error: String },
}

/// What the user asked steambro to do with some games, in the chosen accounts.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Request {
    /// Add the games to the accounts that lack them, and update their outdated steambro shortcuts.
    Add,
    /// Remove steambro's shortcuts of the games. Shortcuts the user made stay.
    Remove,
}

/// The first half of a change, done while Steam may still run: what to do in each account, with
/// the artwork of games to add fetched into the cache. Then `apply` keeps Steam closed only
/// while it writes files.
pub struct Prepared {
    /// What to do in which account, in the order of the request.
    todo: Vec<(u32, String, Change)>,
    fetched: HashMap<(String, Slot), Fetched>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Change {
    Add,
    /// The shortcut with this app ID is steambro's but outdated (see `is_outdated`).
    Update(u32),
    /// Delete steambro's shortcut with this app ID.
    Remove(u32),
}

/// What fetching one slot gave.
#[derive(Clone, Copy)]
enum Fetched {
    Image,
    Unavailable,
    Failed,
}

impl Prepared {
    pub fn is_empty(&self) -> bool {
        self.todo.is_empty()
    }

    /// The requests that need a change, per account, to queue them (`Pending`): updating an
    /// outdated shortcut is adding.
    pub fn requests(&self) -> Vec<(u32, &str, Request)> {
        self.todo
            .iter()
            .map(|(user, key, change)| (*user, key.as_str(), if matches!(change, Change::Remove(_)) { Request::Remove } else { Request::Add }))
            .collect()
    }

    /// Adds another account's preparation, like the changes that wait for Steam to restart. A
    /// change for the same game in the same account is already in `self`, the newer request.
    pub fn merge(&mut self, other: Prepared) {
        for (user, key, change) in other.todo {
            if !self.todo.iter().any(|(u, k, _)| *u == user && *k == key) {
                self.todo.push((user, key, change));
            }
        }
        for (slot, fetched) in other.fetched {
            self.fetched.entry(slot).or_insert(fetched);
        }
    }
}

/// Works out what each chosen account needs for each game, and fetches the artwork for the games
/// to add. Each game has its own request, so one change can add some games and remove others.
/// For `Request::Add`, an account lacks a game unless it has a shortcut for it. An outdated
/// steambro shortcut is updated in place. For `Request::Remove`, each steambro shortcut of the
/// game goes. Games that need nothing are reported and skipped.
pub fn prepare(
    library: &Library,
    cache: &Cache,
    state: &State,
    users: &[SteamUser],
    requests: &[(String, Request)],
    report: &mut dyn FnMut(&str, Step),
) -> crate::Result<Prepared> {
    let shortcuts = users.iter().map(SteamUser::shortcuts).collect::<crate::Result<Vec<_>>>()?;
    let steambro = crate::steambro_exe()?;
    let mut prepared = Prepared { todo: Vec::new(), fetched: HashMap::new() };
    for (key, request) in requests {
        // Removing needs only steambro's record, so it also works for a game that is gone from the PC.
        if *request == Request::Remove {
            let changes: Vec<(u32, Change)> = users
                .iter()
                .zip(&shortcuts)
                .filter_map(|(user, shortcuts)| {
                    let tracked = state.find(user.id, key)?;
                    shortcuts.iter().any(|s| s.app_id == tracked.app_id).then_some((user.id, Change::Remove(tracked.app_id)))
                })
                .collect();
            if changes.is_empty() {
                let theirs = library.find(key).is_some_and(|game| users.iter().zip(&shortcuts).any(|(user, shortcuts)| matches!(status(library, state, user.id, shortcuts, game), Status::User(_))));
                report(key, if theirs { Step::Failed { error: "you added it to Steam yourself, so steambro leaves that shortcut alone".into() } } else { Step::NotInSteam });
                continue;
            }
            prepared.todo.extend(changes.into_iter().map(|(id, change)| (id, key.clone(), change)));
            continue;
        }
        let Some(game) = library.find(key) else {
            report(key, Step::Failed { error: "the game isn't installed any more".into() });
            continue;
        };
        let statuses: Vec<(u32, Status, &[Shortcut])> =
            users.iter().zip(&shortcuts).map(|(user, shortcuts)| (user.id, status(library, state, user.id, shortcuts, game), shortcuts.as_slice())).collect();
        // A game that can't get a shortcut, such as one without a launch code, fails before any download.
        if let Err(e) = shortcut_for(game, &steambro) {
            report(key, Step::Failed { error: e.to_string() });
            continue;
        }
        let changes: Vec<(u32, Change)> = statuses
            .iter()
            .filter_map(|(id, status, shortcuts)| match status {
                Status::Steambro(app_id) => {
                    let shortcut = shortcuts.iter().find(|s| s.app_id == *app_id)?;
                    is_outdated(game, shortcut, &steambro).then_some((*id, Change::Update(*app_id)))
                }
                Status::User(_) => None,
                Status::NotAdded => Some((*id, Change::Add)),
            })
            .collect();
        if changes.is_empty() {
            report(key, Step::AlreadyInSteam);
            continue;
        }
        if changes.iter().any(|(_, change)| *change == Change::Add) {
            let mut origins = asker(library, game);
            for (number, slot) in (1..).zip(Slot::ALL) {
                report(key, Step::Artwork { slot, number, of: Slot::ALL.len() });
                let fetched = match source_with(cache, game, slot, &mut origins) {
                    Ok(Some(_)) => Fetched::Image,
                    Ok(None) => Fetched::Unavailable,
                    Err(_) => Fetched::Failed,
                };
                prepared.fetched.insert((key.clone(), slot), fetched);
            }
            report(key, Step::Ready);
        }
        prepared.todo.extend(changes.into_iter().map(|(id, change)| (id, key.clone(), change)));
    }
    Ok(prepared)
}

/// The second half of a change: writes the shortcuts, their artwork, the updates, and the
/// removals that `prepare` planned. Steam must be closed. It downloads nothing, so it is quick.
/// A removed shortcut is forgotten, with the artwork files steambro wrote for it. Returns the
/// names of the games changed.
pub fn apply(library: &Library, cache: &Cache, state: &mut State, users: &[SteamUser], prepared: &Prepared, report: &mut dyn FnMut(&str, Step)) -> crate::Result<Vec<String>> {
    let steambro = crate::steambro_exe()?;
    let mut errors: HashMap<&str, String> = HashMap::new();
    for user in users {
        let mut added: Vec<(&Game, NewShortcut)> = Vec::new();
        let mut updated: Vec<(u32, NewShortcut)> = Vec::new();
        let mut removed: Vec<(&str, u32)> = Vec::new();
        for (_, key, change) in prepared.todo.iter().filter(|(id, _, _)| *id == user.id) {
            if let Change::Remove(app_id) = change {
                removed.push((key, *app_id));
                continue;
            }
            let Some(game) = library.find(key) else { continue };
            let shortcut = shortcut_for(game, &steambro)?;
            match change {
                Change::Add => {
                    let shortcut = NewShortcut { name: steam_name(library, game), ..shortcut };
                    let icon = user.grid_dir().join(format!("{}.png", Slot::Icon.grid_stem(shortcut.app_id())));
                    added.push((game, NewShortcut { icon, ..shortcut }));
                }
                Change::Update(app_id) => updated.push((*app_id, shortcut)),
                Change::Remove(_) => {}
            }
        }
        if added.is_empty() && updated.is_empty() && removed.is_empty() {
            continue;
        }
        let new: Vec<&NewShortcut> = added.iter().map(|(_, s)| s).collect();
        let gone: Vec<u32> = removed.iter().map(|(_, app_id)| *app_id).collect();
        if let Err(e) = user.save_shortcuts(&new, &updated, &gone) {
            let error = if users.len() > 1 { format!("{}: {e}", user.name.as_deref().unwrap_or("an account")) } else { e.to_string() };
            for (_, key, _) in prepared.todo.iter().filter(|(id, _, _)| *id == user.id) {
                errors.insert(key, error.clone());
            }
            continue;
        }
        if !added.is_empty() {
            let app_ids: Vec<u32> = added.iter().map(|(_, s)| s.app_id()).collect();
            match user.restore_in_collections(&app_ids) {
                Ok(names) if !names.is_empty() => crate::log!("put the added games back into Steam's collections: {}", names.join(", ")),
                Ok(_) => {}
                Err(e) => crate::log!("couldn't put the added games back into Steam's collections: {e}"),
            }
        }
        for (game, shortcut) in &added {
            let key = game.key();
            // A record left from a shortcut the user deleted in Steam: its files are steambro's.
            forget(state, user, &key);
            let added = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_secs());
            state.shortcuts.push(TrackedShortcut { steam_user: user.id, game: key.clone(), app_id: shortcut.app_id(), art: BTreeMap::new(), added, fingerprints: BTreeMap::new() });
            fill_art(cache, state, user, game, &mut |slot| match prepared.fetched.get(&(key.clone(), slot)) {
                Some(Fetched::Image) => cache.get(&key, slot).map(Some).ok_or_else(|| "the image left the cache".into()),
                Some(Fetched::Unavailable) => Ok(None),
                Some(Fetched::Failed) | None => Err("not fetched".into()),
            });
        }
        for (key, _) in removed {
            forget(state, user, key);
        }
    }
    state.save()?;

    let mut done = Vec::new();
    let mut reported: Vec<&str> = Vec::new();
    for (_, key, _) in &prepared.todo {
        if reported.contains(&key.as_str()) {
            continue;
        }
        reported.push(key);
        if let Some(error) = errors.get(key.as_str()) {
            report(key, Step::Failed { error: error.clone() });
            continue;
        }
        let changes: Vec<Change> = prepared.todo.iter().filter(|(_, k, _)| k == key).map(|(_, _, change)| *change).collect();
        if changes.contains(&Change::Add) {
            let later = state
                .shortcuts
                .iter()
                .filter(|t| t.game == *key && users.iter().any(|u| u.id == t.steam_user))
                .flat_map(|t| t.art.values())
                .filter(|p| **p == Placement::Pending)
                .count();
            report(key, Step::Added { later });
        } else if changes.iter().any(|c| matches!(c, Change::Update(_))) {
            report(key, Step::Updated);
        } else {
            report(key, Step::Removed);
        }
        done.push(library.find(key).map_or_else(|| key.clone(), |g| g.name().to_string()));
    }
    Ok(done)
}

/// Whether a steambro shortcut runs something other than what steambro writes today: a way of
/// starting the game that steambro has since replaced, or steambro at a path it has moved from.
pub fn is_outdated(game: &Game, shortcut: &Shortcut, steambro: &Path) -> bool {
    let Ok(now) = shortcut_for(game, steambro) else { return false };
    let exe = shortcut.exe_path().to_string_lossy().to_lowercase();
    exe != now.exe.to_string_lossy().to_lowercase() || shortcut.launch_options != now.launch_options
}

/// Drops steambro's record of the game's shortcut in the account, and deletes the artwork files
/// steambro wrote for it. Images that Steam had before steambro (`Kept`) stay, and so does an image
/// the user set in Steam over steambro's.
fn forget(state: &mut State, user: &SteamUser, key: &str) {
    let Some(at) = state.shortcuts.iter().position(|t| t.steam_user == user.id && t.game == key) else { return };
    let tracked = state.shortcuts.remove(at);
    for (slot, placement) in tracked.art {
        if placement == Placement::Written
            && let Some(file) = existing_file(&user.grid_dir(), tracked.app_id, slot)
            && tracked.fingerprints.get(&slot).is_none_or(|recorded| std::fs::read(&file).is_ok_and(|bytes| fingerprint(&bytes) == *recorded))
        {
            let _ = std::fs::remove_file(file);
        }
    }
}

/// Forgets the account's steambro shortcuts that are gone from Steam, because the user deleted
/// them there, with the artwork steambro wrote for them. Run once per start for each account.
/// Returns the keys of the games forgotten.
pub fn forget_deleted(state: &mut State, user: &SteamUser) -> crate::Result<Vec<String>> {
    let shortcuts = user.shortcuts()?;
    let gone: Vec<String> = state
        .shortcuts
        .iter()
        .filter(|t| t.steam_user == user.id && !shortcuts.iter().any(|s| s.app_id == t.app_id))
        .map(|t| t.game.clone())
        .collect();
    for key in &gone {
        forget(state, user, key);
    }
    if !gone.is_empty() {
        state.save()?;
    }
    Ok(gone)
}

/// Takes back the account's shortcuts that run steambro for a game (`steambro.exe` with
/// `launch "<game key>"`) but have no record, as after "Start fresh" or a lost state file. Images
/// in their slots count as steambro's, with their fingerprints, so that a change the user makes
/// in Steam later is told apart. Run once per start for each account. Returns the keys.
pub fn adopt(state: &mut State, user: &SteamUser) -> crate::Result<Vec<String>> {
    let shortcuts = user.shortcuts()?;
    let mut adopted = Vec::new();
    for shortcut in &shortcuts {
        let Some(key) = launched_key(shortcut) else { continue };
        if state.find(user.id, &key).is_some() || state.shortcuts.iter().any(|t| t.steam_user == user.id && t.app_id == shortcut.app_id) {
            continue;
        }
        let grid = user.grid_dir();
        let mut art = BTreeMap::new();
        let mut fingerprints = BTreeMap::new();
        for slot in Slot::ALL {
            let present = existing_file(&grid, shortcut.app_id, slot).and_then(|file| std::fs::read(file).ok());
            art.insert(slot, if present.is_some() { Placement::Written } else { Placement::Pending });
            if let Some(bytes) = present {
                fingerprints.insert(slot, fingerprint(&bytes));
            }
        }
        state.shortcuts.push(TrackedShortcut { steam_user: user.id, game: key.clone(), app_id: shortcut.app_id, art, added: None, fingerprints });
        adopted.push(key);
    }
    if !adopted.is_empty() {
        state.save()?;
    }
    Ok(adopted)
}

/// Removes every shortcut in the account that runs steambro for a game, with its images, whether
/// steambro has a record of it or not, for Start fresh. Steam must be closed. Returns their keys.
pub fn remove_all(user: &SteamUser) -> crate::Result<Vec<String>> {
    let shortcuts = user.shortcuts()?;
    let ours: Vec<(&Shortcut, String)> = shortcuts.iter().filter_map(|s| launched_key(s).map(|key| (s, key))).collect();
    if ours.is_empty() {
        return Ok(Vec::new());
    }
    let grid = user.grid_dir();
    for (shortcut, _) in &ours {
        for slot in Slot::ALL {
            while let Some(file) = existing_file(&grid, shortcut.app_id, slot) {
                std::fs::remove_file(file)?;
            }
        }
    }
    let gone: Vec<u32> = ours.iter().map(|(s, _)| s.app_id).collect();
    user.save_shortcuts(&[], &[], &gone)?;
    Ok(ours.into_iter().map(|(_, key)| key).collect())
}

/// The game key of a shortcut that runs steambro for a game, from its launch options.
fn launched_key(shortcut: &Shortcut) -> Option<String> {
    if !crate::library::is_named(&shortcut.exe_path(), "steambro.exe") {
        return None;
    }
    let key = shortcut.launch_options.trim().strip_prefix("launch \"")?.strip_suffix('"')?;
    (!key.is_empty() && key.contains(':')).then(|| key.to_string())
}

/// Tries the pending artwork of the account's steambro shortcuts again. Run once per start for
/// each account. Returns whether anything changed.
pub fn retry_pending(library: &Library, cache: &Cache, state: &mut State, user: &SteamUser) -> crate::Result<bool> {
    let shortcuts = user.shortcuts()?;
    let mut changed = false;
    for game in &library.games {
        if !matches!(status(library, state, user.id, &shortcuts, game), Status::Steambro(_)) {
            continue;
        }
        let tracked = state.find(user.id, &game.key()).expect("status found it");
        if Slot::ALL.iter().all(|slot| tracked.art.get(slot).is_some_and(|p| *p != Placement::Pending)) {
            continue;
        }
        let mut origins = asker(library, game);
        fill_art(cache, state, user, game, &mut |slot| source_with(cache, game, slot, &mut origins));
        changed = true;
    }
    if changed {
        state.save()?;
    }
    Ok(changed)
}

/// Asks the store for a game's artwork at most once, and only when a slot needs it.
fn asker<'a>(library: &'a Library, game: &'a Game) -> impl FnMut() -> crate::Result<ArtOrigins> + 'a {
    let mut asked: Option<Result<ArtOrigins, String>> = None;
    move || asked.get_or_insert_with(|| library.art_origins(game).map_err(|e| e.to_string())).clone().map_err(Into::into)
}

/// The slot's source image: steambro's copy, or else fetched from the store and kept. `Ok(None)`
/// means the store has no image for the slot.
pub fn source(library: &Library, cache: &Cache, game: &Game, slot: Slot) -> crate::Result<Option<Vec<u8>>> {
    source_with(cache, game, slot, &mut || library.art_origins(game))
}

fn source_with(cache: &Cache, game: &Game, slot: Slot, origins: &mut dyn FnMut() -> crate::Result<ArtOrigins>) -> crate::Result<Option<Vec<u8>>> {
    let key = game.key();
    if let Some(bytes) = cache.get(&key, slot) {
        return Ok(Some(bytes));
    }
    let origins = origins()?;
    let Some((_, origin)) = origins.iter().find(|(s, _)| *s == slot) else { return Ok(None) };
    let from = from_id(slot, origin);
    // An origin that gave no image before, like a cut-out that failed its checks, isn't tried again.
    if known(cache, &key, slot, &from) == Known::Nothing {
        return Ok(None);
    }
    let bytes = fetch_source(slot, origin)?;
    // The slot may have got an image during the download, like the user's own: that one stays.
    match cache.put_if_absent(&key, slot, bytes.as_deref(), &record(&from, bytes.is_some()))? {
        Some(present) => Ok(Some(present)),
        None => Ok(bytes),
    }
}

/// The store's image for the slot as steambro keeps it: decoded once to check that it is an image
/// steambro can render, and for a logo, trimmed of its empty margins (`logo::trim`). `None` when
/// the origin turns out to have none.
fn fetch_source(slot: Slot, origin: &Origin) -> crate::Result<Option<Vec<u8>>> {
    let Some(bytes) = origin.fetch()? else { return Ok(None) };
    image::load_from_memory(&bytes).map_err(|e| format!("{origin:?} isn't an image steambro can read: {e}"))?;
    Ok(Some(if slot == Slot::Logo { logo::trim(bytes) } else { bytes }))
}

/// Names the store's image and how steambro prepares it, so that a change of either fetches the
/// image again (`refresh`).
fn from_id(slot: Slot, origin: &Origin) -> String {
    if slot == Slot::Logo { format!("{} (trimmed {})", origin.identity(), logo::VERSION) } else { origin.identity() }
}

/// What steambro's copy of a slot says about the store's image at an origin.
#[derive(PartialEq)]
enum Known {
    /// steambro has that image.
    Image,
    /// The origin had no image.
    Nothing,
    /// The origin is new to steambro, or its image is gone from the copy: fetch it.
    Unknown,
}

fn known(cache: &Cache, key: &str, slot: Slot, from: &str) -> Known {
    match cache.from(key, slot) {
        Some(recorded) if recorded == from && cache.file(key, slot).is_some() => Known::Image,
        Some(recorded) if recorded == record(from, false) => Known::Nothing,
        _ => Known::Unknown,
    }
}

/// What the cache records for an origin (`Cache::from`): the origin, or that it had no image.
fn record(from: &str, image: bool) -> String {
    if image { from.to_string() } else { format!("none {from}") }
}

/// A fingerprint of an image file steambro wrote: its length and an FNV-1a hash of its bytes.
pub fn fingerprint(bytes: &[u8]) -> String {
    let hash = bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3));
    format!("{}-{hash:016x}", bytes.len())
}

/// The image file Steam shows in the slot, if there is one.
fn steam_file(grid: &Path, app_id: u32, slot: Slot) -> Option<Vec<u8>> {
    std::fs::read(existing_file(grid, app_id, slot)?).ok()
}

/// Whether `file`, the image Steam shows in a slot that steambro wrote, is still steambro's, and
/// not one the user set in Steam since. A record with the fingerprint of steambro's file compares
/// that. A record from before fingerprints compares the file with `earlier`, the source steambro
/// wrote it from, rendered with the slot's choice, at either anchor for a default crop. Without
/// either, the file counts as steambro's, as the files of shortcuts steambro took back do (`adopt`).
fn is_ours(file: &[u8], slot: Slot, recorded: Option<&str>, earlier: Option<(&[u8], &ArtChoice, Anchor)>) -> bool {
    if let Some(recorded) = recorded {
        return fingerprint(file) == recorded;
    }
    let Some((source, choice, anchor)) = earlier else { return true };
    let mut anchors = vec![anchor];
    if choice.crop.is_none() && slot.size().is_some() {
        anchors.extend([Anchor::Center, Anchor::Top].into_iter().filter(|a| *a != anchor));
    }
    anchors.into_iter().any(|a| render::render(source, slot, choice.crop, a).is_ok_and(|r| r.bytes == file))
}

/// One slot that `refresh` looks at, as it was when it began.
struct Look<'a> {
    game: &'a Game,
    key: String,
    slot: Slot,
    choice: ArtChoice,
    /// The game's steambro shortcuts: the account's place in `refresh`'s list, the app ID, and the
    /// slot's placement and fingerprint.
    shortcuts: Vec<(usize, u32, Option<Placement>, Option<String>)>,
}

/// What `refresh` found for a slot, without the state's lock.
struct Found {
    /// What to record as the store's origin (`record`), when steambro asked for its image.
    from: Option<String>,
    /// steambro's copy changes to this image; `Some(None)`: it goes, the store has none now.
    new: Option<Option<Vec<u8>>>,
    /// The choice to keep: it loses its crop when the new image has another shape.
    choice: ArtChoice,
    /// The image to write into Steam, rendered.
    rendered: Option<render::Rendered>,
    /// For each shortcut in `Look::shortcuts` whose written slot changes: the fingerprint of the
    /// file Steam showed (`None`: no file), and whether that file was steambro's (`is_ours`).
    seen: Vec<Option<(Option<String>, bool)>>,
}

/// Keeps steambro's images up to date with the stores, for the slots that follow their store
/// (`ArtChoice::follows`), in the games that steambro added or has images of. It asks each store
/// where its images are now and fetches an image only when its origin changed (`Origin::identity`).
/// steambro's copy changes only when the image changed; when the store has no image any more,
/// steambro's copy goes. In every account where the game is a steambro shortcut, it writes the new
/// image where Steam still shows steambro's file (`is_ours`), takes steambro's file out where the
/// store has none any more, and fills each slot that is still empty. An image the user set in
/// Steam, over steambro's or in an empty slot, and a slot they cleared there, become `Kept`;
/// Steam's own images are never touched. steambro's copy changes only after Steam's files are
/// written, so a slot that couldn't be written is tried again next time.
///
/// Downloads, renders and the checks of Steam's files run without the state's lock; it is held
/// only to read the choices and to write. A slot that changed meanwhile, in steambro or in Steam,
/// is left to the next look, and nothing is written when `stop` says so (Start fresh). Runs when
/// the window opens. Returns the names of the games whose images changed.
pub fn refresh(library: &Library, cache: &Cache, state: &std::sync::Mutex<State>, users: &[SteamUser], stop: &dyn Fn() -> bool) -> Vec<String> {
    let accounts: Vec<(&SteamUser, Vec<Shortcut>)> = users.iter().filter_map(|u| Some((u, u.shortcuts().ok()?))).collect();
    let looks: Vec<Look> = {
        let state = state.lock().unwrap();
        let mut looks = Vec::new();
        for game in &library.games {
            let key = game.key();
            let added: Vec<(usize, u32)> = accounts
                .iter()
                .enumerate()
                .filter_map(|(i, (user, shortcuts))| match status(library, &state, user.id, shortcuts, game) {
                    Status::Steambro(app_id) => Some((i, app_id)),
                    _ => None,
                })
                .collect();
            if added.is_empty() && Slot::ALL.iter().all(|slot| cache.file(&key, *slot).is_none()) {
                continue;
            }
            for slot in Slot::ALL {
                let choice = state.choice(&key, slot);
                if !choice.follows() {
                    continue;
                }
                let shortcuts = added
                    .iter()
                    .map(|&(i, app_id)| {
                        let tracked = state.find(accounts[i].0.id, &key);
                        (i, app_id, tracked.and_then(|t| t.art.get(&slot).copied()), tracked.and_then(|t| t.fingerprints.get(&slot).cloned()))
                    })
                    .collect();
                looks.push(Look { game, key: key.clone(), slot, choice, shortcuts });
            }
        }
        looks
    };

    let mut asked: HashMap<String, Option<ArtOrigins>> = HashMap::new();
    let mut found = Vec::new();
    for look in &looks {
        // A store that can't be asked now (no connection) is asked again next time.
        let origins = asked.entry(look.key.clone()).or_insert_with(|| library.art_origins(look.game).ok());
        let old = cache.get(&look.key, look.slot);
        let mut f = Found { from: None, new: None, choice: look.choice.clone(), rendered: None, seen: Vec::new() };
        if let Some((_, origin)) = origins.as_ref().and_then(|o| o.iter().find(|(s, _)| *s == look.slot)) {
            let from = from_id(look.slot, origin);
            if known(cache, &look.key, look.slot, &from) == Known::Unknown {
                match fetch_source(look.slot, origin) {
                    Ok(Some(bytes)) => {
                        f.from = Some(record(&from, true));
                        if old.as_ref() != Some(&bytes) {
                            // A crop is kept for the same picture in another size, not for another shape.
                            if f.choice.crop.is_some() && !same_shape(old.as_deref(), &bytes) {
                                f.choice.crop = None;
                            }
                            f.new = Some(Some(bytes));
                        }
                    }
                    Ok(None) => {
                        f.from = Some(record(&from, false));
                        if old.is_some() {
                            f.new = Some(None);
                        }
                    }
                    Err(e) => crate::log!("couldn't get the new {} of {}: {e}", look.slot.label(), look.game.name()),
                }
            }
        }
        let changes = f.new.is_some();
        let anchor = anchor(look.game, &look.choice, look.slot);
        f.seen = look
            .shortcuts
            .iter()
            .map(|(i, app_id, placement, print)| {
                if !changes || *placement != Some(Placement::Written) {
                    return None;
                }
                let file = steam_file(&accounts[*i].0.grid_dir(), *app_id, look.slot);
                let ours = file.as_deref().is_some_and(|file| is_ours(file, look.slot, print.as_deref(), old.as_deref().map(|o| (o, &look.choice, anchor))));
                Some((file.as_deref().map(fingerprint), ours))
            })
            .collect();
        let needed = look.shortcuts.iter().zip(&f.seen).any(|((i, app_id, placement, _), seen)| match placement {
            Some(Placement::Kept) => false,
            Some(Placement::Written) => seen.as_ref().is_some_and(|(_, ours)| *ours),
            _ => existing_file(&accounts[*i].0.grid_dir(), *app_id, look.slot).is_none(),
        });
        let image = match &f.new {
            Some(new) => new.as_deref(),
            None => old.as_deref(),
        };
        if needed && let Some(image) = image {
            match render::render(image, look.slot, f.choice.crop, anchor) {
                Ok(rendered) => f.rendered = Some(rendered),
                Err(e) => crate::log!("couldn't render the new {} of {}: {e}", look.slot.label(), look.game.name()),
            }
        }
        found.push(f);
    }

    let mut state = state.lock().unwrap();
    if stop() {
        return Vec::new();
    }
    let mut changed: Vec<String> = Vec::new();
    for (look, f) in looks.iter().zip(found) {
        // The slot changed meanwhile: in steambro, as an edit, or in Steam. The next look does it
        // again, from what is there then.
        let moved = state.choice(&look.key, look.slot) != look.choice
            || look.shortcuts.iter().zip(&f.seen).any(|((i, app_id, placement, print), seen)| {
                let user = accounts[*i].0;
                let tracked = state.find(user.id, &look.key);
                let now = (tracked.and_then(|t| t.art.get(&look.slot).copied()), tracked.and_then(|t| t.fingerprints.get(&look.slot).cloned()));
                now != (*placement, print.clone())
                    || seen.as_ref().is_some_and(|(file, _)| steam_file(&user.grid_dir(), *app_id, look.slot).map(|b| fingerprint(&b)) != *file)
            });
        if moved {
            continue;
        }
        let mut did = false;
        let mut failed = false;
        for ((i, app_id, placement, _), seen) in look.shortcuts.iter().zip(&f.seen) {
            let user = accounts[*i].0;
            let grid = user.grid_dir();
            let ours = seen.as_ref().is_some_and(|(_, ours)| *ours);
            let outcome = match (placement, &f.new) {
                (Some(Placement::Kept), _) | (Some(Placement::Written), None) => continue,
                // The user's image in Steam, or the empty slot they left, stays.
                (Some(Placement::Written), Some(_)) if !ours => Some(Placement::Kept),
                (Some(Placement::Written), Some(None)) => {
                    while let Some(file) = existing_file(&grid, *app_id, look.slot) {
                        if let Err(e) = std::fs::remove_file(file) {
                            crate::log!("couldn't take out the {} of {}: {e}", look.slot.label(), look.game.name());
                            failed = true;
                            break;
                        }
                    }
                    did = true;
                    (!failed).then_some(Placement::Unavailable)
                }
                // An empty slot that has an image now got it from Steam, like one the user set there.
                (_, _) if *placement != Some(Placement::Written) && existing_file(&grid, *app_id, look.slot).is_some() => Some(Placement::Kept),
                (_, _) => match &f.rendered {
                    None => {
                        failed |= *placement == Some(Placement::Written);
                        None
                    }
                    Some(rendered) => match write_rendered(&grid, *app_id, look.slot, rendered) {
                        Ok(print) => {
                            mark_written(&mut state, user.id, &look.key, look.slot, print);
                            did = true;
                            None
                        }
                        Err(e) => {
                            crate::log!("couldn't write the new {} of {}: {e}", look.slot.label(), look.game.name());
                            failed = true;
                            // An emptied slot is filled at the next try.
                            existing_file(&grid, *app_id, look.slot).is_none().then_some(Placement::Pending)
                        }
                    },
                },
            };
            if let Some(placement) = outcome {
                mark(&mut state, user.id, &look.key, look.slot, placement);
            }
        }
        // steambro's copy follows only once Steam has the new image everywhere it should, so a
        // slot that failed keeps the old origin and is done again next time.
        if !failed {
            let kept = match &f.new {
                Some(Some(bytes)) => cache.put(&look.key, look.slot, bytes, f.from.as_deref()),
                Some(None) => cache.remove(&look.key, look.slot).and_then(|()| f.from.as_ref().map_or(Ok(()), |from| cache.set_from(&look.key, look.slot, from))),
                None => f.from.as_ref().map_or(Ok(()), |from| cache.set_from(&look.key, look.slot, from)),
            };
            match kept {
                Ok(()) => {
                    did |= f.new.is_some();
                    if f.choice != look.choice {
                        state.set_choice(&look.key, look.slot, f.choice.clone());
                    }
                }
                Err(e) => crate::log!("couldn't keep the new {} of {}: {e}", look.slot.label(), look.game.name()),
            }
        }
        if did && !changed.iter().any(|name| name == look.game.name()) {
            changed.push(look.game.name().to_string());
        }
    }
    if let Err(e) = state.save() {
        crate::log!("couldn't save steambro's record of the artwork: {e}");
    }
    changed
}

/// Records that steambro wrote the slot's file, with its fingerprint.
fn mark_written(state: &mut State, user: u32, key: &str, slot: Slot, print: String) {
    if let Some(tracked) = state.find_mut(user, key) {
        tracked.art.insert(slot, Placement::Written);
        tracked.fingerprints.insert(slot, print);
    }
}

/// Records the slot's placement when steambro has no file of its own there.
fn mark(state: &mut State, user: u32, key: &str, slot: Slot, placement: Placement) {
    if let Some(tracked) = state.find_mut(user, key) {
        tracked.art.insert(slot, placement);
        tracked.fingerprints.remove(&slot);
    }
}

/// Both images have the same shape, as the same picture in another size has.
fn same_shape(old: Option<&[u8]>, new: &[u8]) -> bool {
    let ratio = |bytes: &[u8]| {
        let (w, h) = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().ok()?.into_dimensions().ok()?;
        Some(f64::from(w) / f64::from(h.max(1)))
    };
    match (old.and_then(ratio), ratio(new)) {
        (Some(a), Some(b)) => (a / b - 1.0).abs() < 0.01,
        _ => false,
    }
}

/// Fills each slot of a steambro shortcut that has no final placement yet. A slot where Steam
/// already has an image is kept as it is, and steambro never replaces it by itself. `fetch` gives
/// the slot's source image.
fn fill_art(cache: &Cache, state: &mut State, user: &SteamUser, game: &Game, fetch: &mut dyn FnMut(Slot) -> crate::Result<Option<Vec<u8>>>) {
    let key = game.key();
    let Some(app_id) = state.find(user.id, &key).map(|t| t.app_id) else { return };
    let grid = user.grid_dir();
    for slot in Slot::ALL {
        let current = state.find(user.id, &key).and_then(|t| t.art.get(&slot).copied());
        if current.is_some_and(|p| p != Placement::Pending) {
            continue;
        }
        let placement = if existing_file(&grid, app_id, slot).is_some() {
            Placement::Kept
        } else {
            match fetch(slot) {
                Ok(None) => Placement::Unavailable,
                Err(_) => Placement::Pending,
                Ok(Some(bytes)) => {
                    let choice = state.choice(&key, slot);
                    match write_slot(&grid, app_id, slot, &bytes, &choice, anchor(game, &choice, slot)) {
                        Ok(print) => {
                            mark_written(state, user.id, &key, slot, print);
                            continue;
                        }
                        // A damaged copy is dropped so the next run fetches it again.
                        Err(_) => {
                            let _ = cache.remove(&key, slot);
                            Placement::Pending
                        }
                    }
                }
            }
        };
        mark(state, user.id, &key, slot, placement);
    }
}

/// The user's edits from the UI. Each one changes steambro's choice for the game and, in every
/// account where the game is a steambro shortcut, rewrites the slot in Steam, even one that Steam
/// had its own image in (`Kept`): the edit is the user's explicit request.
pub fn set_crop(library: &Library, cache: &Cache, state: &mut State, users: &[SteamUser], key: &str, slot: Slot, crop: Option<Crop>) -> crate::Result<()> {
    let choice = ArtChoice { crop, ..state.choice(key, slot) };
    state.set_choice(key, slot, choice);
    apply_edit(library, cache, state, users, key, slot)
}

/// Makes `bytes` the slot's source image, with the centered default crop. `file` is where the
/// image came from, when known. The user's own image never follows the store.
pub fn replace(library: &Library, cache: &Cache, state: &mut State, users: &[SteamUser], key: &str, slot: Slot, bytes: &[u8], file: Option<PathBuf>) -> crate::Result<()> {
    image::load_from_memory(bytes).map_err(|e| format!("that file isn't an image steambro can read: {e}"))?;
    let fixed = state.choice(key, slot).fixed;
    cache.put(key, slot, bytes, None)?;
    state.set_choice(key, slot, ArtChoice { crop: None, own: true, fixed, file });
    apply_edit(library, cache, state, users, key, slot)
}

/// Back to the store's image with the centered crop. A store image steambro already has isn't
/// fetched again. The user's own image is dropped for the store's, but only once the store's image
/// is in hand; when the store can't be asked, nothing changes. When the store has no image for the
/// slot, like for an exe's cover, the slot is cleared: the user's image goes from steambro, and
/// steambro's file from Steam. Whether the slot follows the store stays as the user set it
/// (`set_follow`).
pub fn reset(library: &Library, cache: &Cache, state: &mut State, users: &[SteamUser], key: &str, slot: Slot) -> crate::Result<()> {
    let choice = state.choice(key, slot);
    let new = ArtChoice { fixed: choice.fixed, ..ArtChoice::default() };
    if !choice.own {
        state.set_choice(key, slot, new);
        return apply_edit(library, cache, state, users, key, slot);
    }
    let game = library.find(key).ok_or_else(|| format!("{key} isn't installed"))?;
    let origins = library.art_origins(game)?;
    let mut nothing = None;
    let store = match origins.iter().find(|(s, _)| *s == slot) {
        None => None,
        Some((_, origin)) => {
            let from = from_id(slot, origin);
            let bytes = if known(cache, key, slot, &from) == Known::Nothing { None } else { fetch_source(slot, origin)? };
            if bytes.is_none() {
                nothing = Some(record(&from, false));
            }
            bytes.map(|bytes| (bytes, from))
        }
    };
    if let Some((bytes, from)) = store {
        cache.put(key, slot, &bytes, Some(&from))?;
        state.set_choice(key, slot, new);
        return apply_edit(library, cache, state, users, key, slot);
    }
    cache.remove(key, slot)?;
    if let Some(nothing) = nothing {
        cache.set_from(key, slot, &nothing)?;
    }
    state.set_choice(key, slot, new);
    for user in users {
        let Some(tracked) = state.find(user.id, key) else { continue };
        let (app_id, recorded) = (tracked.app_id, tracked.fingerprints.get(&slot).cloned());
        if tracked.art.get(&slot) != Some(&Placement::Written) {
            continue;
        }
        let grid = user.grid_dir();
        // An image the user set in Steam since stays.
        if steam_file(&grid, app_id, slot).is_some_and(|file| is_ours(&file, slot, recorded.as_deref(), None)) {
            while let Some(file) = existing_file(&grid, app_id, slot) {
                std::fs::remove_file(file)?;
            }
            mark(state, user.id, key, slot, Placement::Unavailable);
        }
    }
    state.save()
}

/// Turns the slot's updates from the store on or off. Off keeps the image as it is now. On goes
/// back to the store's image: the newest one, with the user's crop while the image is the store's,
/// written in every account like an edit. The user's own image is dropped for it, but only once the
/// store's image is in hand: when the store can't be asked, or has no image for the slot, nothing
/// changes and the error says why.
pub fn set_follow(library: &Library, cache: &Cache, state: &mut State, users: &[SteamUser], key: &str, slot: Slot, follow: bool) -> crate::Result<()> {
    let choice = state.choice(key, slot);
    if !follow {
        // Kept through the user's own image, so a reset to the store's image doesn't follow it.
        state.set_choice(key, slot, ArtChoice { fixed: true, ..choice });
        return state.save();
    }
    let game = library.find(key).ok_or_else(|| format!("{key} isn't installed"))?;
    let none = || format!("{} has no image for the {}", game.slot_source(slot), slot.label());
    let origins = library.art_origins(game)?;
    let (_, origin) = origins.iter().find(|(s, _)| *s == slot).ok_or_else(none)?;
    let from = from_id(slot, origin);
    let cached = cache.get(key, slot);
    let bytes = match known(cache, key, slot, &from) {
        Known::Image if !choice.own => cached.clone().ok_or_else(none)?,
        Known::Nothing => return Err(none().into()),
        _ => fetch_source(slot, origin)?.ok_or_else(none)?,
    };
    let mut new = ArtChoice { fixed: false, own: false, file: None, ..choice.clone() };
    if choice.own || (new.crop.is_some() && !same_shape(cached.as_deref(), &bytes)) {
        new.crop = None;
    }
    if choice.own || cached.as_ref() != Some(&bytes) {
        cache.put(key, slot, &bytes, Some(&from))?;
    } else {
        cache.set_from(key, slot, &from)?;
    }
    state.set_choice(key, slot, new);
    apply_edit(library, cache, state, users, key, slot)
}

/// Where the file dialog for the slot's image opens, with the file to select: beside the file of
/// the user's own image; else in steambro's folder of the store's images for the game, with the
/// slot's image selected when it has one; else in the game's folder. A game added by its exe has no
/// store, so its dialog opens in the game's folder.
pub fn picker_folder(library: &Library, cache: &Cache, state: &State, key: &str, slot: Slot) -> Option<(PathBuf, Option<String>)> {
    let choice = state.choice(key, slot);
    if choice.own
        && let Some(file) = &choice.file
        && let Some(folder) = file.parent().filter(|f| f.is_dir())
    {
        return Some((folder.to_path_buf(), file.file_name().map(|n| n.to_string_lossy().into_owned())));
    }
    let game = library.find(key)?;
    if game.has_store() && cache.folder(key).is_dir() {
        let file = cache.file(key, slot).and_then(|f| f.file_name().map(|n| n.to_string_lossy().into_owned()));
        return Some((cache.folder(key), file));
    }
    Some(game.install_dir().to_path_buf()).filter(|dir| dir.is_dir()).map(|dir| (dir, None))
}

fn apply_edit(library: &Library, cache: &Cache, state: &mut State, users: &[SteamUser], key: &str, slot: Slot) -> crate::Result<()> {
    let game = library.find(key).ok_or_else(|| format!("{key} isn't installed"))?;
    for user in users {
        let Some(app_id) = state.find(user.id, key).map(|t| t.app_id) else { continue };
        if !user.shortcuts()?.iter().any(|s| s.app_id == app_id) {
            continue;
        }
        let bytes = source(library, cache, game, slot)?.ok_or("there is no image for this slot")?;
        let choice = state.choice(key, slot);
        let print = write_slot(&user.grid_dir(), app_id, slot, &bytes, &choice, anchor(game, &choice, slot))?;
        mark_written(state, user.id, key, slot, print);
    }
    state.save()
}

/// Extensions Steam reads in the grid folder.
const GRID_EXTENSIONS: [&str; 4] = ["png", "jpg", "jpeg", "webp"];

/// The image Steam shows in the slot, if the grid folder has one.
pub fn existing_file(grid: &Path, app_id: u32, slot: Slot) -> Option<PathBuf> {
    let stem = slot.grid_stem(app_id);
    GRID_EXTENSIONS.iter().map(|ext| grid.join(format!("{stem}.{ext}"))).find(|p| p.is_file())
}

/// Where the default crop sits: the store's placement for its images, the center for the user's.
pub fn anchor(game: &Game, choice: &ArtChoice, slot: Slot) -> Anchor {
    if choice.own { Anchor::Center } else { game.anchor(slot) }
}

/// Renders the slot and writes it (`write_rendered`). Returns the file's fingerprint.
fn write_slot(grid: &Path, app_id: u32, slot: Slot, source: &[u8], choice: &ArtChoice, anchor: Anchor) -> crate::Result<String> {
    write_rendered(grid, app_id, slot, &render::render(source, slot, choice.crop, anchor)?)
}

/// Puts a rendered image into the slot in place of whatever file the slot had, whatever its
/// extension. Returns the file's fingerprint.
fn write_rendered(grid: &Path, app_id: u32, slot: Slot, rendered: &render::Rendered) -> crate::Result<String> {
    std::fs::create_dir_all(grid)?;
    while let Some(old) = existing_file(grid, app_id, slot) {
        std::fs::remove_file(old)?;
    }
    // The shortcut's icon path names a .png file, so the icon is always PNG (see `render`).
    std::fs::write(grid.join(format!("{}.{}", slot.grid_stem(app_id), rendered.ext)), &rendered.bytes)?;
    Ok(fingerprint(&rendered.bytes))
}

/// The game's name in Steam: its own, with its store after it when a game from another store has
/// the same name, like "Fortnite (Xbox)". So both can be in Steam and be told apart there, each
/// with its own app ID: Steam's ID comes from the exe and the name, and every shortcut runs steambro.
fn steam_name(library: &Library, game: &Game) -> String {
    let twin = library.games.iter().any(|g| g.store.id() != game.store.id() && g.name().eq_ignore_ascii_case(game.name()));
    if twin { format!("{} ({})", game.name(), game.source()) } else { game.name().to_string() }
}

/// The Steam shortcut for a game: `steambro launch "<game key>"`, when its store can add it
/// (`Store::can_add`). Adding names it with `steam_name`.
fn shortcut_for(game: &Game, steambro: &Path) -> crate::Result<NewShortcut> {
    game.store.can_add(game)?;
    Ok(NewShortcut {
        name: game.name().to_string(),
        start_dir: steambro.parent().map(Path::to_path_buf).unwrap_or_default(),
        exe: steambro.to_path_buf(),
        launch_options: format!("launch \"{}\"", game.key()),
        icon: PathBuf::new(),
        // Steam doesn't make a collection of a tag; the tag only names the store.
        tags: vec![game.source().to_string()],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_game_in_two_stores_gets_two_names_and_ids() {
        let store = |id: &str| *crate::library::stores().iter().find(|s| s.id() == id).unwrap();
        let mut library = Library::default();
        library.games = vec![
            Game::new(store("xbox"), "x".into(), "Fortnite".into(), PathBuf::new(), ()),
            Game::new(store("epic"), "Fortnite".into(), "FORTNITE".into(), PathBuf::new(), ()),
            Game::new(store("gog"), "1".into(), "Alien Nations".into(), PathBuf::new(), ()),
        ];
        let names: Vec<String> = library.games.iter().map(|g| steam_name(&library, g)).collect();
        assert_eq!(names, ["Fortnite (Xbox)", "FORTNITE (Epic Games)", "Alien Nations"]);
        let id = |name: &str| NewShortcut { name: name.into(), exe: PathBuf::from(r"C:\steambro.exe"), start_dir: PathBuf::new(), launch_options: String::new(), icon: PathBuf::new(), tags: Vec::new() }.app_id();
        assert_ne!(id(&names[0]), id(&names[1]));
    }

    fn change(library: &Library, cache: &Cache, state: &mut State, users: &[SteamUser], keys: &[String], request: Request) -> (Vec<String>, Vec<(String, &'static str)>) {
        let requests: Vec<(String, Request)> = keys.iter().map(|key| (key.clone(), request)).collect();
        changes(library, cache, state, users, &requests)
    }

    fn changes(library: &Library, cache: &Cache, state: &mut State, users: &[SteamUser], requests: &[(String, Request)]) -> (Vec<String>, Vec<(String, &'static str)>) {
        let mut steps = Vec::new();
        let mut report = |key: &str, step: Step| {
            let kind = match step {
                Step::Artwork { .. } => "artwork",
                Step::Ready => "ready",
                Step::Added { .. } => "added",
                Step::Updated => "updated",
                Step::Removed => "removed",
                Step::Queued => "queued",
                Step::AlreadyInSteam => "already",
                Step::NotInSteam => "not in steam",
                Step::Failed { .. } => "failed",
            };
            steps.push((key.to_string(), kind));
        };
        let prepared = prepare(library, cache, state, users, requests, &mut report).unwrap();
        let changed = apply(library, cache, state, users, &prepared, &mut report).unwrap();
        (changed, steps)
    }

    /// A real Xbox game and a real Battle.net game of this PC, where there is one.
    fn some_games(library: &Library) -> Vec<String> {
        ["xbox:", "battlenet:"]
            .iter()
            .filter_map(|prefix| library.games.iter().map(Game::key).find(|k| k.starts_with(prefix)))
            .collect()
    }

    /// Adds one real Xbox game and one real Battle.net game of this PC to two scratch account
    /// folders (Store artwork comes over the network) and checks the shortcuts, the grid files,
    /// and the reported steps.
    #[test]
    fn add_writes_shortcuts_and_art() {
        let root = std::env::temp_dir().join("steambro-test");
        let _ = std::fs::remove_dir_all(&root);
        let users = [SteamUser::at(1, root.join("userdata").join("1")), SteamUser::at(2, root.join("userdata").join("2"))];
        let library = Library::detect();
        let keys = some_games(&library);
        if keys.is_empty() {
            return;
        }
        let cache = Cache::open().unwrap();
        let mut state = State::default();

        // The first account only. A game that isn't installed fails without stopping the others.
        let missing = "xbox:Not.Installed_0000000000000!App".to_string();
        let request: Vec<String> = std::iter::once(missing.clone()).chain(keys.iter().cloned()).collect();
        let (added, steps) = change(&library, &cache, &mut state, &users[..1], &request, Request::Add);
        assert_eq!(steps[0], (missing, "failed"));
        assert_eq!(added.len(), keys.len());
        for key in &keys {
            let kinds: Vec<&str> = steps.iter().filter(|(k, _)| k == key).map(|(_, kind)| *kind).collect();
            assert_eq!(kinds, ["artwork", "artwork", "artwork", "artwork", "artwork", "ready", "added"], "{key}");
            let tracked = state.find(1, key).unwrap();
            assert!(users[0].shortcuts().unwrap().iter().any(|s| s.app_id == tracked.app_id), "{key} has no shortcut");
            for (slot, placement) in &tracked.art {
                let file = existing_file(&users[0].grid_dir(), tracked.app_id, *slot);
                assert_eq!(file.is_some(), *placement == Placement::Written, "{key} {slot:?} is {placement:?}");
            }
            assert!(tracked.art.values().filter(|p| **p == Placement::Written).count() >= 3, "{key}: {:?}", tracked.art);
        }
        assert!(users[1].shortcuts().unwrap().is_empty(), "the second account was not chosen");

        // A slot that follows its store takes the store's image again when steambro's copy came
        // from elsewhere, in steambro and in Steam. A slot the user fixed keeps its image, until
        // it follows the store again.
        let key = &keys[0];
        let name = library.find(key).unwrap().name().to_string();
        let slot = [Slot::Cover, Slot::Hero, Slot::Wide].into_iter().find(|s| state.find(1, key).unwrap().art.get(s) == Some(&Placement::Written)).expect("a written slot");
        let mut dot = Vec::new();
        image::DynamicImage::new_rgb8(3, 3).write_to(&mut std::io::Cursor::new(&mut dot), image::ImageFormat::Png).unwrap();
        let stale = |cache: &Cache| cache.put(key, slot, &dot, Some("an older origin")).unwrap();
        let refreshed = |state: &mut State| {
            let shared = std::sync::Mutex::new(std::mem::take(state));
            let changed = refresh(&library, &cache, &shared, &users[..1], &|| false);
            *state = shared.into_inner().unwrap();
            changed
        };
        stale(&cache);
        assert!(refreshed(&mut state).contains(&name));
        assert_ne!(cache.get(key, slot), Some(dot.clone()), "steambro's copy is the store's again");
        let file = existing_file(&users[0].grid_dir(), state.find(1, key).unwrap().app_id, slot).unwrap();
        assert!(image::image_dimensions(&file).unwrap().0 > 3, "and so is Steam's");
        // An image the user set in Steam over steambro's stays when the store's image changes.
        std::fs::write(&file, &dot).unwrap();
        stale(&cache);
        refreshed(&mut state);
        assert_eq!(std::fs::read(&file).unwrap(), dot, "the user's image in Steam stays");
        assert_eq!(state.find(1, key).unwrap().art.get(&slot), Some(&Placement::Kept));
        state.set_choice(key, slot, ArtChoice { fixed: true, ..state.choice(key, slot) });
        stale(&cache);
        assert!(!refreshed(&mut state).contains(&name));
        assert_eq!(cache.get(key, slot), Some(dot.clone()), "a fixed slot stays");
        // Following again takes the store's image and writes it over the kept one: the user asked.
        set_follow(&library, &cache, &mut state, &users[..1], key, slot, true).unwrap();
        assert!(state.choice(key, slot).follows());
        assert_ne!(cache.get(key, slot), Some(dot.clone()), "following again takes the store's image");
        assert_eq!(state.find(1, key).unwrap().art.get(&slot), Some(&Placement::Written));
        assert!(image::image_dimensions(&file).unwrap().0 > 3);

        // Both accounts: only the second one lacks the games, and gets the same shortcuts.
        let (added, _) = change(&library, &cache, &mut state, &users, &keys, Request::Add);
        assert_eq!(added.len(), keys.len());
        assert_eq!(users[0].shortcuts().unwrap().len(), keys.len());
        for key in &keys {
            assert_eq!(state.find(1, key).unwrap().app_id, state.find(2, key).unwrap().app_id);
        }

        // Adding again changes nothing, and art that is there stays.
        let (added, steps) = change(&library, &cache, &mut state, &users, &keys, Request::Add);
        assert!(added.is_empty());
        assert!(steps.iter().all(|(_, kind)| *kind == "already"), "{steps:?}");
        assert_eq!(users[1].shortcuts().unwrap().len(), keys.len());

        // A shortcut that runs steambro from an old path is outdated; adding updates it in place.
        let game = library.find(&keys[0]).unwrap();
        let app_id = state.find(1, &keys[0]).unwrap().app_id;
        let mut old = shortcut_for(game, Path::new(r"C:\old place\steambro.exe")).unwrap();
        old.launch_options = "--old".into();
        users[0].save_shortcuts(&[], &[(app_id, old)], &[]).unwrap();
        let steambro = crate::steambro_exe().unwrap();
        let shortcut = |user: &SteamUser| user.shortcuts().unwrap().into_iter().find(|s| s.app_id == app_id).unwrap();
        assert!(is_outdated(game, &shortcut(&users[0]), &steambro));
        let (updated, steps) = change(&library, &cache, &mut state, &users, &keys, Request::Add);
        assert_eq!(updated, [game.name().to_string()]);
        assert_eq!(steps.iter().find(|(k, _)| *k == keys[0]).unwrap().1, "updated", "{steps:?}");
        let now = shortcut(&users[0]);
        assert!(!is_outdated(game, &now, &steambro), "{} {}", now.exe, now.launch_options);
        assert_eq!(users[0].shortcuts().unwrap().len(), keys.len(), "updated in place, not added");

        // Removing takes steambro's shortcut and the art it wrote out of the second account only.
        let app_id = state.find(2, &keys[0]).unwrap().app_id;
        let written: Vec<Slot> = state.find(2, &keys[0]).unwrap().art.iter().filter(|(_, p)| **p == Placement::Written).map(|(s, _)| *s).collect();
        assert!(!written.is_empty());
        let (removed, steps) = change(&library, &cache, &mut state, &users[1..], &keys[..1], Request::Remove);
        assert_eq!(removed, [game.name().to_string()]);
        assert_eq!(steps, [(keys[0].clone(), "removed")]);
        assert!(users[1].shortcuts().unwrap().iter().all(|s| s.app_id != app_id));
        assert!(written.iter().all(|slot| existing_file(&users[1].grid_dir(), app_id, *slot).is_none()), "the art steambro wrote is gone");
        assert_eq!(status(&library, &state, 2, &users[1].shortcuts().unwrap(), game), Status::NotAdded);
        assert!(state.find(2, &keys[0]).is_none(), "the record is gone");
        assert_eq!(users[0].shortcuts().unwrap().len(), keys.len(), "the first account keeps it");

        // Removing again finds nothing; adding it back writes the artwork again, as steambro's. The
        // same change can remove another game.
        let (_, steps) = change(&library, &cache, &mut state, &users[1..], &keys[..1], Request::Remove);
        assert_eq!(steps, [(keys[0].clone(), "not in steam")]);
        let mut requests = vec![(keys[0].clone(), Request::Add)];
        requests.extend(keys.get(1).map(|other| (other.clone(), Request::Remove)));
        let (_, steps) = changes(&library, &cache, &mut state, &users[1..], &requests);
        assert!(steps.contains(&(keys[0].clone(), "added")), "{steps:?}");
        if let Some(other) = keys.get(1) {
            assert!(steps.contains(&(other.clone(), "removed")), "{steps:?}");
            assert!(state.find(2, other).is_none() && state.find(1, other).is_some(), "only the second account lost it");
        }
        let art = &state.find(2, &keys[0]).unwrap().art;
        assert!(written.iter().all(|slot| art.get(slot) == Some(&Placement::Written)), "{art:?}");

        // A shortcut deleted in Steam (here straight from the file) is forgotten at the next start,
        // with steambro's artwork, so adding it again doesn't take steambro's files for Steam's.
        users[1].save_shortcuts(&[], &[], &[app_id]).unwrap();
        assert_eq!(forget_deleted(&mut state, &users[1]).unwrap(), [keys[0].clone()]);
        assert!(written.iter().all(|slot| existing_file(&users[1].grid_dir(), app_id, *slot).is_none()));
        assert!(forget_deleted(&mut state, &users[0]).unwrap().is_empty(), "the first account still has its shortcuts");

        // A game gone from the PC can still be removed: its record names the shortcut.
        let gone_app = state.find(1, &keys[0]).unwrap().app_id;
        state.shortcuts.iter_mut().find(|t| t.steam_user == 1 && t.game == keys[0]).unwrap().game = "exe:gone".into();
        let gone = Library::default();
        assert!(gone.is_uninstalled("exe:gone"));
        let (removed, steps) = change(&gone, &cache, &mut state, &users[..1], &["exe:gone".to_string()], Request::Remove);
        assert_eq!((removed, steps), (vec!["exe:gone".to_string()], vec![("exe:gone".to_string(), "removed")]));
        assert!(users[0].shortcuts().unwrap().iter().all(|s| s.app_id != gone_app));
        assert!(state.find(1, "exe:gone").is_none());
    }
}


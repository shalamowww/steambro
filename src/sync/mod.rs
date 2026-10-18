//! Puts games into Steam and keeps steambro's record of it: adds shortcuts, fills artwork slots
//! under the artwork rules, and applies the user's artwork edits.

mod state;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
pub use state::{ArtChoice, Placement, State};
use state::TrackedShortcut;

use crate::art::cache::Cache;
use crate::art::{Anchor, Crop, Slot, render};
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

/// What `prepare` and `add` report about a game while they work, for the window's progress view.
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
    /// Every chosen account has it already.
    AlreadyInSteam,
    /// No chosen account has a steambro shortcut for it.
    NotInSteam,
    Failed { error: String },
}

/// What the user asked steambro to do with some games, in the chosen accounts.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Deserialize)]
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
}

/// Works out what each chosen account needs for each game, and fetches the artwork for the games
/// to add. For `Request::Add`, an account lacks a game unless it has a shortcut for it. An
/// outdated steambro shortcut is updated in place. For `Request::Remove`, each steambro shortcut
/// of the game goes. Games that need nothing are reported and skipped.
pub fn prepare(
    library: &Library,
    cache: &Cache,
    state: &State,
    users: &[SteamUser],
    keys: &[String],
    request: Request,
    report: &mut dyn FnMut(&str, Step),
) -> crate::Result<Prepared> {
    let shortcuts = users.iter().map(SteamUser::shortcuts).collect::<crate::Result<Vec<_>>>()?;
    let steambro = crate::steambro_exe()?;
    let mut prepared = Prepared { todo: Vec::new(), fetched: HashMap::new() };
    for key in keys {
        // Removing needs only steambro's record, so it also works for a game that is gone from the PC.
        if request == Request::Remove {
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
            state.shortcuts.push(TrackedShortcut { steam_user: user.id, game: key.clone(), app_id: shortcut.app_id(), art: BTreeMap::new() });
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
/// steambro wrote for it. Images that Steam had before steambro (`Kept`) stay.
fn forget(state: &mut State, user: &SteamUser, key: &str) {
    let Some(at) = state.shortcuts.iter().position(|t| t.steam_user == user.id && t.game == key) else { return };
    let tracked = state.shortcuts.remove(at);
    for (slot, placement) in tracked.art {
        if placement == Placement::Written
            && let Some(file) = existing_file(&user.grid_dir(), tracked.app_id, slot)
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
    let bytes = origin.fetch()?;
    image::guess_format(&bytes).map_err(|_| format!("{origin:?} isn't an image"))?;
    cache.put(&key, slot, &bytes)?;
    Ok(Some(bytes))
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
                        Ok(()) => Placement::Written,
                        // A damaged copy is dropped so the next run fetches it again.
                        Err(_) => {
                            let _ = cache.remove(&key, slot);
                            Placement::Pending
                        }
                    }
                }
            }
        };
        if let Some(tracked) = state.find_mut(user.id, &key) {
            tracked.art.insert(slot, placement);
        }
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

/// Makes `bytes` the slot's source image, with the centered default crop.
pub fn replace(library: &Library, cache: &Cache, state: &mut State, users: &[SteamUser], key: &str, slot: Slot, bytes: &[u8]) -> crate::Result<()> {
    image::load_from_memory(bytes).map_err(|e| format!("that file isn't an image steambro can read: {e}"))?;
    cache.put(key, slot, bytes)?;
    state.set_choice(key, slot, ArtChoice { crop: None, own: true });
    apply_edit(library, cache, state, users, key, slot)
}

/// Back to the store's image with the centered crop. A store image steambro already has isn't
/// fetched again; the user's own image is dropped and the store's is fetched.
pub fn reset(library: &Library, cache: &Cache, state: &mut State, users: &[SteamUser], key: &str, slot: Slot) -> crate::Result<()> {
    if state.choice(key, slot).own {
        cache.remove(key, slot)?;
    }
    state.set_choice(key, slot, ArtChoice::default());
    apply_edit(library, cache, state, users, key, slot)
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
        write_slot(&user.grid_dir(), app_id, slot, &bytes, &choice, anchor(game, &choice, slot))?;
        if let Some(tracked) = state.find_mut(user.id, key) {
            tracked.art.insert(slot, Placement::Written);
        }
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

/// Renders the slot and replaces whatever file the slot had, whatever its extension.
fn write_slot(grid: &Path, app_id: u32, slot: Slot, source: &[u8], choice: &ArtChoice, anchor: Anchor) -> crate::Result<()> {
    let rendered = render::render(source, slot, choice.crop, anchor)?;
    std::fs::create_dir_all(grid)?;
    while let Some(old) = existing_file(grid, app_id, slot) {
        std::fs::remove_file(old)?;
    }
    // The shortcut's icon path names a .png file, so the icon is always PNG (see `render`).
    std::fs::write(grid.join(format!("{}.{}", slot.grid_stem(app_id), rendered.ext)), rendered.bytes)?;
    Ok(())
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
        let mut steps = Vec::new();
        let mut report = |key: &str, step: Step| {
            let kind = match step {
                Step::Artwork { .. } => "artwork",
                Step::Ready => "ready",
                Step::Added { .. } => "added",
                Step::Updated => "updated",
                Step::Removed => "removed",
                Step::AlreadyInSteam => "already",
                Step::NotInSteam => "not in steam",
                Step::Failed { .. } => "failed",
            };
            steps.push((key.to_string(), kind));
        };
        let prepared = prepare(library, cache, state, users, keys, request, &mut report).unwrap();
        let changed = apply(library, cache, state, users, &prepared, &mut report).unwrap();
        (changed, steps)
    }

    /// Real games of this PC, one per store.
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

        // Removing again finds nothing; adding it back writes the artwork again, as steambro's.
        let (_, steps) = change(&library, &cache, &mut state, &users[1..], &keys[..1], Request::Remove);
        assert_eq!(steps, [(keys[0].clone(), "not in steam")]);
        change(&library, &cache, &mut state, &users[1..], &keys[..1], Request::Add);
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

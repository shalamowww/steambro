//! Every game steambro found on this PC, whichever store it came from. Each store is a module
//! that implements `Store`; everything about one store lives in its module. steambro reads each
//! store's own records and never scans the disk, so a leftover game folder doesn't show.

mod battlenet;
mod epic;
pub mod exe;
mod gog;
mod msstore;
mod xbox;

use std::any::Any;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::Serialize;

use crate::art::{Anchor, Origin, Slot};
use crate::settings::Overrides;
use crate::steam::Shortcut;

/// Every store steambro supports, in the order the window shows them. A new store is a module that
/// implements `Store`, added here.
static STORES: [&dyn Store; 5] = [&battlenet::BattleNet, &xbox::Xbox, &epic::Epic, &gog::Gog, &exe::Exe];

pub fn stores() -> &'static [&'static dyn Store] {
    &STORES
}

/// One source of games: how to find its games, where their artwork comes from, and how they start.
pub trait Store: Sync {
    /// The first part of its game keys, like `xbox`. Never changes: the state and Steam shortcuts
    /// hold game keys.
    fn id(&self) -> &'static str;
    /// The store's name for display, like `Xbox`. Also the tag on its Steam shortcuts.
    fn name(&self) -> &'static str;
    /// Where its artwork comes from, for display, like `Microsoft Store`.
    fn art_source(&self) -> &'static str;
    /// Where the image of one of the game's slots comes from, for display, when a store takes some
    /// images elsewhere.
    fn slot_source(&self, _game: &Game, _slot: Slot) -> &'static str {
        self.art_source()
    }
    /// The games installed for the current Windows user, from the store's own records; never from
    /// scanning the disk. Empty when the store isn't installed. A game that can't be read is
    /// skipped with a note in `warnings`; an error means the whole store couldn't be read.
    fn detect(&'static self, warnings: &mut Vec<String>) -> crate::Result<Vec<Game>>;
    /// Where each slot's image comes from. An error means the store couldn't be asked now (for
    /// example, no connection); an empty list means it has no artwork for the game.
    fn art_origins(&self, game: &Game) -> crate::Result<Vec<(Slot, Origin)>>;
    /// The slots the store can ever fill, without asking it. The window offers updates from the
    /// store only for these.
    fn art_slots(&self, _game: &Game) -> &'static [Slot] {
        &Slot::ALL
    }
    /// Where the default crop of the store's image for the game's slot sits.
    fn anchor(&self, _game: &Game, _slot: Slot) -> Anchor {
        Anchor::Center
    }
    /// Checks that the game can be added to Steam; an error gives the reason. Every shortcut runs
    /// `steambro launch "<game key>"`.
    fn can_add(&self, _game: &Game) -> crate::Result<()> {
        Ok(())
    }
    /// How the game starts, for display.
    fn launch_description(&self, game: &Game) -> String;
    /// Whether a shortcut that the user or another tool made starts the game. A shortcut to an
    /// exe inside the game's folder always counts; this adds the store's own ways.
    fn is_launched_by(&self, _game: &Game, _shortcut: &Shortcut) -> bool {
        false
    }
    /// Starts the game for `steambro launch` and says how to follow it. See launch.rs.
    fn start(&self, game: &Game) -> crate::Result<Started>;
    /// When Steam can't see the store's games while they play (plan.rs). Every store says it, so
    /// that the launch and the settings page read the same answer.
    fn unseen(&self) -> Unseen;
    /// The store's own defaults, between steambro's and the user's choices for the store
    /// (plan.rs).
    fn defaults(&self) -> Overrides {
        Overrides::default()
    }
    /// How long the store may take to start a game at least, when that's longer than the setting
    /// for every game (`LaunchSettings::start_timeout_s`); a game's own setting still wins.
    fn start_timeout(&self) -> Option<std::time::Duration> {
        None
    }
    /// The store may miss the first launch request, so a launch asks it again after the nudge
    /// time (`Started::nudge`, `LaunchSettings::nudge_s`). The settings window shows that time
    /// only for such a store.
    fn asks_again(&self) -> bool {
        false
    }
    /// An app rather than a game, like Netflix. Steam's desktop layout (mouse and keyboard) suits
    /// it, so by default it gets no bridge, keeps that layout, and starts in full screen (plan.rs).
    fn is_app(&self, _game: &Game) -> bool {
        false
    }
    /// The store's own app that starts its games, like Battle.net, when it has one. Steam sees the
    /// game only when steambro starts that app inside Steam's launch (launch.rs).
    fn app_name(&self) -> Option<&'static str> {
        None
    }
    /// Whether the store's app runs now.
    fn app_running(&self) -> bool {
        false
    }
    /// The file names of the app's main processes, like `EpicGamesLauncher.exe`, for an app whose
    /// starter may start it again, so that the launch can't follow it from its own processes
    /// (`process::Family::take_in`).
    fn app_exes(&self) -> &'static [&'static str] {
        &[]
    }
    /// The game may hide its windows on purpose and keep running, like a program in the tray, so
    /// a launch doesn't take it for hung on its way out (launch.rs, `Hung`). An app does, and an
    /// exe the user added may be a launcher that does.
    fn windows_may_hide(&self, game: &Game) -> bool {
        self.is_app(game)
    }
    /// The store's app closes by itself once a game starts, as its own settings say, so there is
    /// nothing to hand over (`hand_over_app` in launch.rs), and opening it again would go against
    /// the user's choice.
    fn app_closes_itself(&self) -> bool {
        false
    }
    /// Why closing the store's app now would interrupt it, like a download.
    fn app_busy(&self) -> Option<String> {
        None
    }
    /// Closes the store's app and waits until it has closed, so that `start` opens it again.
    fn close_app(&self, _game: &Game) -> crate::Result<()> {
        Ok(())
    }
    /// Opens the store's app outside Steam's launch (`process::start_outside_steam`), as if the
    /// user opened it, after steambro closed the one it opened for the game (`HandOver::Reopen`).
    /// Returns the app's process ID once it runs.
    fn reopen_app(&self, _game: &Game) -> crate::Result<u32> {
        Err("this store has no app to open".into())
    }
    /// What the store's app and its settings are now, as (label, value), for the log of a launch
    /// and the report for a bug (report.rs): its version, whether it runs, and those of its own
    /// settings that change what happens at a launch, with the game's own where there is a game.
    /// Never an account name or a sign-in.
    fn diagnostics(&self, _game: Option<&Game>) -> Vec<(String, String)> {
        Vec::new()
    }
    /// Extra lines for `steambro list -v`, as (label, value).
    fn details(&self, _game: &Game) -> Vec<(String, String)> {
        Vec::new()
    }
}

/// When Steam can't see a store's games while they play. Then steambro stands in for Steam with the
/// game's controller layout, the bridge, and the overlay, as the settings say (plan.rs).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Unseen {
    /// Always: Windows starts the game, as a packaged app (Xbox).
    Always,
    /// When the store's app already ran outside Steam's launch, so the game it starts does too
    /// (Battle.net, Epic Games).
    AppOutside,
    /// When the game runs as administrator: it starts through Windows' prompt, outside Steam's
    /// launch (GOG, added exes).
    Admin,
}

/// How `steambro launch` follows a game that its store started.
pub struct Started {
    /// The game is a process with its exe in one of these folders,
    pub folders: Vec<PathBuf>,
    /// or a process with the identity of one of these packages (by family name), wherever its exe is.
    pub packages: Vec<String>,
    /// The error when no game process appears.
    pub not_started: String,
    /// Asks the store once more if no game process has appeared after a while.
    pub nudge: Option<Box<dyn Fn() -> crate::Result<()>>>,
    /// Steam doesn't see the game: the store's app already ran outside Steam's launch, or the game
    /// runs as administrator.
    pub outside_steam: bool,
    /// Runs after the game has ended, for example to close a store app that steambro opened.
    pub after: Option<Box<dyn FnOnce()>>,
}

pub struct Game {
    pub store: &'static dyn Store,
    /// The store's own ID for the game. The game key is `<store id>:<id>`.
    id: String,
    name: String,
    /// The folder of the game's files. A process with its exe inside it is the game.
    install_dir: PathBuf,
    /// What only the store's module reads, such as Xbox's package folder or Battle.net's launch code.
    details: Box<dyn Any + Send + Sync>,
}

/// Where each slot's image comes from, as far as the game's store says.
pub type ArtOrigins = Arc<Vec<(Slot, Origin)>>;

#[derive(Default)]
pub struct Library {
    pub games: Vec<Game>,
    /// Problems with one store; the games of the other stores are still listed.
    pub warnings: Vec<String>,
    /// The stores that couldn't list their games, so their games' absence means nothing.
    pub failed: Vec<&'static str>,
    /// Store lookups already done in this run, by game key.
    origins: Mutex<HashMap<String, ArtOrigins>>,
}

impl Library {
    pub fn detect() -> Self {
        let mut games = Vec::new();
        let mut warnings = Vec::new();
        let mut failed = Vec::new();
        for store in STORES {
            match store.detect(&mut warnings) {
                Ok(found) => games.extend(found),
                Err(e) => {
                    warnings.push(format!("couldn't list {} games: {e}", store.name()));
                    failed.push(store.id());
                }
            }
        }
        games.sort_by_key(|g| g.name().to_lowercase());
        Self { games, warnings, failed, origins: Mutex::default() }
    }

    pub fn find(&self, key: &str) -> Option<&Game> {
        self.games.iter().find(|g| g.key() == key)
    }

    /// The game is gone from the PC: its store listed its games, and this one wasn't among them.
    pub fn is_uninstalled(&self, key: &str) -> bool {
        let store = key.split_once(':').map_or(key, |(store, _)| store);
        self.find(key).is_none() && STORES.iter().any(|s| s.id() == store) && !self.failed.contains(&store)
    }

    /// The game's artwork sources, asked once per run. See `Store::art_origins`.
    pub fn art_origins(&self, game: &Game) -> crate::Result<ArtOrigins> {
        let key = game.key();
        if let Some(found) = self.origins.lock().unwrap().get(&key) {
            return Ok(found.clone());
        }
        let found = Arc::new(game.store.art_origins(game)?);
        self.origins.lock().unwrap().insert(key, found.clone());
        Ok(found)
    }

    /// Whether a Steam shortcut made by the user or another tool already starts this game.
    pub fn is_launched_by(&self, game: &Game, shortcut: &Shortcut) -> bool {
        is_inside(&shortcut.exe_path(), game.install_dir()) || game.store.is_launched_by(game, shortcut)
    }
}

/// A game of the store with this ID, for other modules' tests. Only Xbox has apps.
#[cfg(test)]
pub fn test_game(store_id: &str, name: &str, app: bool) -> Game {
    if store_id == "xbox" {
        return xbox::test_game(name, app);
    }
    let store = *STORES.iter().find(|s| s.id() == store_id).expect("a known store");
    Game::new(store, name.into(), name.into(), PathBuf::new(), ())
}

/// The installed game with this key, asking only its store. For `steambro launch`, which needs one
/// game quickly.
pub fn find_installed(key: &str) -> crate::Result<Game> {
    let (store_id, _) = key.split_once(':').ok_or_else(|| format!("{key} isn't a game key"))?;
    let store = STORES.iter().find(|s| s.id() == store_id).ok_or_else(|| format!("steambro doesn't know the store of {key}"))?;
    let mut warnings = Vec::new();
    store
        .detect(&mut warnings)?
        .into_iter()
        .find(|g| g.key().eq_ignore_ascii_case(key))
        .ok_or_else(|| format!("{key} isn't installed any more").into())
}

impl Game {
    pub fn new(store: &'static dyn Store, id: String, name: String, install_dir: PathBuf, details: impl Any + Send + Sync) -> Self {
        Self { store, id, name, install_dir, details: Box::new(details) }
    }

    /// The store module's details for the game. Only that module calls this, with its own type.
    pub fn details<T: 'static>(&self) -> &T {
        self.details.downcast_ref().expect("a store reads only its own games")
    }

    /// Identity that stays stable across runs: `<store id>:<id>`.
    pub fn key(&self) -> String {
        format!("{}:{}", self.store.id(), self.id)
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn install_dir(&self) -> &Path {
        &self.install_dir
    }

    pub fn source(&self) -> &'static str {
        self.store.name()
    }

    pub fn art_source(&self) -> &'static str {
        self.store.art_source()
    }

    /// Where the image of the slot comes from, for display.
    pub fn slot_source(&self, slot: Slot) -> &'static str {
        self.store.slot_source(self, slot)
    }

    /// The store can fill the slot (`Store::art_slots`).
    pub fn store_fills(&self, slot: Slot) -> bool {
        self.store.art_slots(self).contains(&slot)
    }

    /// Where the default crop of the store's image for the slot sits.
    pub fn anchor(&self, slot: Slot) -> Anchor {
        self.store.anchor(self, slot)
    }

    /// The game comes from a store with its own artwork, not an exe the user added.
    pub fn has_store(&self) -> bool {
        self.store.id() != exe::Exe.id()
    }

    /// How the game starts, for display.
    pub fn launch_description(&self) -> String {
        self.store.launch_description(self)
    }
}

pub fn is_named(path: &Path, name: &str) -> bool {
    path.file_name().is_some_and(|f| f.eq_ignore_ascii_case(name))
}

/// Case-insensitive "is `path` somewhere under `dir`", matching how Windows treats paths.
pub fn is_inside(path: &Path, dir: &Path) -> bool {
    let path = path.to_string_lossy().to_lowercase();
    let dir = dir.to_string_lossy().to_lowercase();
    let dir = dir.trim_end_matches('\\');
    path.strip_prefix(dir).is_some_and(|rest| rest.starts_with('\\'))
}

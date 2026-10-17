//! The user's settings, in `%LOCALAPPDATA%\steambro\settings.json`: each store's behavior, each
//! game's overrides of it, and the timings of game launches. The tray app writes them; game
//! launches read them.

use std::collections::BTreeMap;
use std::ops::RangeInclusive;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    /// The tray app tells about a newly installed game that isn't in Steam yet.
    pub new_game_notices: bool,
    /// How game launches follow every game.
    pub launch: LaunchSettings,
    /// Each store's behavior, by store ID. A store without an entry has the defaults.
    pub stores: BTreeMap<String, StoreSettings>,
    /// Each game's overrides of its store's behavior, by game key.
    pub games: BTreeMap<String, GameSettings>,
}

impl Default for Settings {
    fn default() -> Self {
        Self { new_game_notices: true, launch: LaunchSettings::default(), stores: BTreeMap::new(), games: BTreeMap::new() }
    }
}

/// How `steambro launch` follows a game (launch.rs), for every game.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct LaunchSettings {
    /// How often steambro looks for the game's processes, in milliseconds.
    pub check_ms: u32,
    /// How long the game may take to appear after its store started it, in seconds. A store can
    /// need longer (`Started::start_timeout`); then its time applies.
    pub start_timeout_s: u32,
    /// Without a game process by then, steambro asks the store once more, in seconds.
    pub nudge_s: u32,
    /// The game counts as ended when none of its processes has run for this long, in seconds.
    /// Some games close and start again, like an updater or a launcher.
    pub end_grace_s: u32,
    /// How long a launch waits for the answer to its question (prompt.rs), in seconds.
    pub question_timeout_s: u32,
}

impl Default for LaunchSettings {
    fn default() -> Self {
        Self { check_ms: 1000, start_timeout_s: 90, nudge_s: 20, end_grace_s: 8, question_timeout_s: 60 }
    }
}

/// The values each timing may have; a file with another value gets the nearest one.
pub const CHECK_MS: RangeInclusive<u32> = 100..=10_000;
pub const START_TIMEOUT_S: RangeInclusive<u32> = 10..=3600;
pub const NUDGE_S: RangeInclusive<u32> = 5..=600;
pub const END_GRACE_S: RangeInclusive<u32> = 0..=600;
pub const QUESTION_TIMEOUT_S: RangeInclusive<u32> = 10..=600;

impl LaunchSettings {
    fn clamped(self) -> Self {
        let clamp = |v: u32, r: RangeInclusive<u32>| v.clamp(*r.start(), *r.end());
        Self {
            check_ms: clamp(self.check_ms, CHECK_MS),
            start_timeout_s: clamp(self.start_timeout_s, START_TIMEOUT_S),
            nudge_s: clamp(self.nudge_s, NUDGE_S),
            end_grace_s: clamp(self.end_grace_s, END_GRACE_S),
            question_timeout_s: clamp(self.question_timeout_s, QUESTION_TIMEOUT_S),
        }
    }
}

/// What steambro does for a store's games. Most of it matters only while Steam can't see the
/// game: always for Xbox games, and for a game whose store app already ran outside Steam.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct StoreSettings {
    /// Copy Steam's controllers to virtual Xbox 360 controllers (src/bridge.rs). Needs `layout`.
    pub bridge: bool,
    /// Make Steam use the game's Steam Input layout while it plays, whatever window is in front
    /// (`steam://forceinputappid`). Off, Steam keeps its desktop layout: mouse and keyboard.
    pub layout: bool,
    /// Show Steam's overlay over the game, in a window of steambro's (src/overlay.rs).
    pub overlay: bool,
    /// Open that overlay with Steam's overlay key, like Shift+Tab, while the game is in front.
    pub overlay_keys: bool,
    /// When Steam ends the game (Exit game, Stop), end all of the game's processes too. Steam
    /// ends `steambro launch`, which Windows then follows with the game (process.rs, `Job`).
    pub exit_ends_game: bool,
    /// What to do when the store's app (Battle.net, the Epic Games launcher) already runs outside
    /// Steam at a launch, so Steam wouldn't see the game it starts.
    pub app_running: AppRunning,
    /// Close the store's app after the game, when steambro opened it. Steam counts the app as part
    /// of the game, so it shows the game as running until the app closes.
    pub close_app: bool,
    /// Show an app (`Store::is_app`, like Netflix) in full screen when it starts (window.rs). Games
    /// keep their own window unless their settings say otherwise.
    pub full_screen: bool,
}

impl Default for StoreSettings {
    fn default() -> Self {
        Self {
            bridge: true,
            layout: true,
            overlay: true,
            overlay_keys: true,
            exit_ends_game: true,
            app_running: AppRunning::Ask,
            close_app: true,
            full_screen: true,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AppRunning {
    /// Ask in a prompt, which can make the answer the store's setting.
    Ask,
    /// Close the app and start it again inside Steam's launch.
    Restart,
    /// Leave it; the game runs outside Steam's view.
    Leave,
}

/// One game's overrides; `None` follows the store's setting, or for the timings, the setting for
/// every game.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct GameSettings {
    pub bridge: Option<bool>,
    pub layout: Option<bool>,
    pub overlay: Option<bool>,
    pub overlay_keys: Option<bool>,
    pub exit_ends_game: Option<bool>,
    pub app_running: Option<AppRunning>,
    pub close_app: Option<bool>,
    pub full_screen: Option<bool>,
    pub start_timeout_s: Option<u32>,
    pub end_grace_s: Option<u32>,
}

impl GameSettings {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    fn clamped(self) -> Self {
        Self {
            start_timeout_s: self.start_timeout_s.map(|v| v.clamp(*START_TIMEOUT_S.start(), *START_TIMEOUT_S.end())),
            end_grace_s: self.end_grace_s.map(|v| v.clamp(*END_GRACE_S.start(), *END_GRACE_S.end())),
            ..self
        }
    }
}

/// What a launch does for one game: its own settings over its store's, over the defaults.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Behavior {
    pub bridge: bool,
    pub layout: bool,
    pub overlay: bool,
    pub overlay_keys: bool,
    pub exit_ends_game: bool,
    pub app_running: AppRunning,
    pub close_app: bool,
    pub full_screen: bool,
    pub check: Duration,
    pub start_timeout: Duration,
    pub nudge: Duration,
    pub end_grace: Duration,
}

impl Settings {
    /// The saved settings, or the defaults when there are none or the file can't be read.
    pub fn load() -> Self {
        let Ok(path) = path() else { return Self::default() };
        let settings: Self = std::fs::read(&path).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default();
        settings.clamped()
    }

    /// Loads the settings, changes them, and saves them.
    pub fn update(change: impl FnOnce(&mut Self)) -> crate::Result<()> {
        let mut settings = Self::load();
        change(&mut settings);
        settings.save()
    }

    /// Values outside what the settings allow become the nearest allowed value.
    fn clamped(mut self) -> Self {
        self.launch = self.launch.clamped();
        for game in self.games.values_mut() {
            *game = game.clamped();
        }
        self
    }

    /// Writes a new file and then replaces the old one.
    pub fn save(&self) -> crate::Result<()> {
        let path = path()?;
        std::fs::create_dir_all(path.parent().expect("the settings path has a parent"))?;
        let temp = path.with_extension("json.new");
        std::fs::write(&temp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&temp, &path)?;
        Ok(())
    }

    pub fn store(&self, store_id: &str) -> StoreSettings {
        self.stores.get(store_id).copied().unwrap_or_default()
    }

    pub fn game(&self, game_key: &str) -> GameSettings {
        self.games.get(game_key).copied().unwrap_or_default()
    }

    /// What a launch does for the game. An app (`Store::is_app`, like Netflix) keeps Steam's
    /// desktop layout and gets no bridge unless its own settings say so; turning its bridge on
    /// brings the game's layout with it. Only apps start in full screen, unless a game's own
    /// settings say so. The bridge needs the game's layout: without it, Steam
    /// gives its virtual controllers nothing to copy. `store_start_timeout` is the store's own
    /// minimum time to start a game (`Started::start_timeout`).
    pub fn behavior(&self, game_key: &str, is_app: bool, store_start_timeout: Option<Duration>) -> Behavior {
        let store = self.store(store_of(game_key));
        let game = self.game(game_key);
        let bridge = game.bridge.unwrap_or(!is_app && store.bridge);
        let layout = game.layout.unwrap_or(if is_app { bridge } else { store.layout });
        let start = Duration::from_secs(self.launch.start_timeout_s.into()).max(store_start_timeout.unwrap_or_default());
        Behavior {
            bridge: bridge && layout,
            layout,
            overlay: game.overlay.unwrap_or(store.overlay),
            overlay_keys: game.overlay_keys.unwrap_or(store.overlay_keys),
            exit_ends_game: game.exit_ends_game.unwrap_or(store.exit_ends_game),
            app_running: game.app_running.unwrap_or(store.app_running),
            close_app: game.close_app.unwrap_or(store.close_app),
            full_screen: game.full_screen.unwrap_or(is_app && store.full_screen),
            check: Duration::from_millis(self.launch.check_ms.into()),
            start_timeout: game.start_timeout_s.map_or(start, |s| Duration::from_secs(s.into())),
            nudge: Duration::from_secs(self.launch.nudge_s.into()),
            end_grace: Duration::from_secs(game.end_grace_s.unwrap_or(self.launch.end_grace_s).into()),
        }
    }

    pub fn set_game(&mut self, game_key: &str, game: GameSettings) {
        if game.is_empty() {
            self.games.remove(game_key);
        } else {
            self.games.insert(game_key.to_string(), game);
        }
    }
}

/// The store ID in a game key: `epic` in `epic:Fortnite`.
fn store_of(game_key: &str) -> &str {
    game_key.split_once(':').map_or(game_key, |(store, _)| store)
}

fn path() -> crate::Result<PathBuf> {
    Ok(crate::data_dir()?.join("settings.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn games_follow_their_store_unless_overridden() {
        let mut settings = Settings::default();
        let fortnite = settings.behavior("epic:Fortnite", false, None);
        assert!(fortnite.bridge && fortnite.layout && fortnite.overlay && fortnite.overlay_keys && fortnite.exit_ends_game && fortnite.close_app);
        assert_eq!(fortnite.app_running, AppRunning::Ask);
        assert!(!fortnite.full_screen);
        let netflix = settings.behavior("xbox:Netflix", true, None);
        assert!(!netflix.bridge && !netflix.layout && netflix.overlay && netflix.full_screen);
        settings.stores.insert("xbox".into(), StoreSettings { full_screen: false, ..Default::default() });
        assert!(!settings.behavior("xbox:Netflix", true, None).full_screen);
        settings.stores.clear();

        settings.stores.insert("epic".into(), StoreSettings { bridge: false, app_running: AppRunning::Restart, overlay: false, ..Default::default() });
        let fortnite = settings.behavior("epic:Fortnite", false, None);
        assert!(!fortnite.bridge && fortnite.layout && !fortnite.overlay);
        assert_eq!(fortnite.app_running, AppRunning::Restart);
        assert!(settings.behavior("xbox:Game", false, None).bridge);

        settings.set_game("xbox:Netflix", GameSettings { bridge: Some(true), ..Default::default() });
        let netflix = settings.behavior("xbox:Netflix", true, None);
        assert!(netflix.bridge && netflix.layout);
        settings.set_game("epic:Fortnite", GameSettings { bridge: Some(true), app_running: Some(AppRunning::Leave), exit_ends_game: Some(false), ..Default::default() });
        let fortnite = settings.behavior("epic:Fortnite", false, None);
        assert!(fortnite.bridge && !fortnite.exit_ends_game);
        assert_eq!(fortnite.app_running, AppRunning::Leave);

        // The bridge needs the game's layout.
        settings.set_game("xbox:Game", GameSettings { layout: Some(false), ..Default::default() });
        let game = settings.behavior("xbox:Game", false, None);
        assert!(!game.layout && !game.bridge);

        // An app's own bridge brings the layout even when the store's layout is off.
        settings.stores.insert("xbox".into(), StoreSettings { layout: false, ..Default::default() });
        assert!(settings.behavior("xbox:Netflix", true, None).layout);
        settings.stores.clear();

        // A game starts in full screen only by its own setting.
        settings.set_game("gog:Game", GameSettings { full_screen: Some(true), ..Default::default() });
        assert!(settings.behavior("gog:Game", false, None).full_screen && !settings.behavior("gog:Other", false, None).full_screen);
        settings.set_game("gog:Game", GameSettings::default());

        settings.set_game("epic:Fortnite", GameSettings::default());
        settings.set_game("xbox:Netflix", GameSettings::default());
        settings.set_game("xbox:Game", GameSettings::default());
        assert!(settings.games.is_empty());
    }

    #[test]
    fn timings_come_from_the_settings_the_store_and_the_game() {
        let mut settings = Settings::default();
        let game = settings.behavior("xbox:Game", false, None);
        assert_eq!((game.check, game.start_timeout, game.nudge, game.end_grace), (Duration::from_secs(1), Duration::from_secs(90), Duration::from_secs(20), Duration::from_secs(8)));
        // A store that needs longer to start a game gets its time; a game's own time wins.
        assert_eq!(settings.behavior("epic:Fortnite", false, Some(Duration::from_secs(300))).start_timeout, Duration::from_secs(300));
        settings.set_game("epic:Fortnite", GameSettings { start_timeout_s: Some(30), end_grace_s: Some(0), ..Default::default() });
        let fortnite = settings.behavior("epic:Fortnite", false, Some(Duration::from_secs(300)));
        assert_eq!((fortnite.start_timeout, fortnite.end_grace), (Duration::from_secs(30), Duration::ZERO));
    }

    #[test]
    fn out_of_range_values_are_clamped() {
        let settings: Settings = serde_json::from_str(r#"{"launch": {"checkMs": 1, "endGraceS": 99999}, "games": {"xbox:A": {"startTimeoutS": 1}}}"#).unwrap();
        let settings = settings.clamped();
        assert_eq!(settings.launch.check_ms, 100);
        assert_eq!(settings.launch.end_grace_s, 600);
        assert_eq!(settings.launch.start_timeout_s, 90);
        assert_eq!(settings.game("xbox:A").start_timeout_s, Some(10));
    }
}

//! The user's settings, in `%LOCALAPPDATA%\steambro\settings.json`: the user's choices for each
//! store and each game, and the timings of game launches. The tray app writes them; game launches
//! read them. What a launch makes of them is plan.rs.

use std::collections::BTreeMap;
use std::ops::RangeInclusive;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    /// The tray app tells about a newly installed game that isn't in Steam yet.
    pub new_game_notices: bool,
    /// How game launches follow every game.
    pub launch: LaunchSettings,
    /// The user's choices for each store, by store ID.
    pub stores: BTreeMap<String, Overrides>,
    /// The user's choices for each game, by game key.
    pub games: BTreeMap<String, Overrides>,
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
    /// need longer (`Store::start_timeout`); then its time applies.
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

/// What steambro does for a game, as values: steambro's defaults (`Knobs::default`), or what
/// plan.rs resolved for a game. Most of it matters only while Steam can't see the game
/// (`library::Unseen`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Knobs {
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
    /// Show the game in full screen when it starts (window.rs). A store's choice is for its apps
    /// (`Store::is_app`, like Netflix); games keep their own window unless their own choice says
    /// otherwise.
    pub full_screen: bool,
}

impl Default for Knobs {
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

/// The user's choices for a store or a game, one per setting of `Knobs` and two timings; `None`
/// leaves a setting to the layer below (plan.rs). A store and a game have the same choices, so a
/// setting can be chosen for either, or for many games at once.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Overrides {
    pub bridge: Option<bool>,
    pub layout: Option<bool>,
    pub overlay: Option<bool>,
    pub overlay_keys: Option<bool>,
    pub exit_ends_game: Option<bool>,
    pub app_running: Option<AppRunning>,
    pub close_app: Option<bool>,
    pub full_screen: Option<bool>,
    /// How long the game may take to start, in seconds (`LaunchSettings::start_timeout_s`).
    pub start_timeout_s: Option<u32>,
    /// When the game counts as ended, in seconds (`LaunchSettings::end_grace_s`).
    pub end_grace_s: Option<u32>,
}

impl Overrides {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// These choices, with `base`'s where these have none.
    pub fn over(self, base: Self) -> Self {
        Self {
            bridge: self.bridge.or(base.bridge),
            layout: self.layout.or(base.layout),
            overlay: self.overlay.or(base.overlay),
            overlay_keys: self.overlay_keys.or(base.overlay_keys),
            exit_ends_game: self.exit_ends_game.or(base.exit_ends_game),
            app_running: self.app_running.or(base.app_running),
            close_app: self.close_app.or(base.close_app),
            full_screen: self.full_screen.or(base.full_screen),
            start_timeout_s: self.start_timeout_s.or(base.start_timeout_s),
            end_grace_s: self.end_grace_s.or(base.end_grace_s),
        }
    }

    fn clamped(self) -> Self {
        Self {
            start_timeout_s: self.start_timeout_s.map(|v| v.clamp(*START_TIMEOUT_S.start(), *START_TIMEOUT_S.end())),
            end_grace_s: self.end_grace_s.map(|v| v.clamp(*END_GRACE_S.start(), *END_GRACE_S.end())),
            ..self
        }
    }
}

impl Settings {
    /// The saved settings, or the defaults when there are none or the file can't be read.
    pub fn load() -> Self {
        let Ok(path) = path() else { return Self::default() };
        let settings: Self = std::fs::read(&path).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default();
        settings.clamped()
    }

    /// Loads the settings, changes them, and saves them, so that a change keeps what another
    /// window saved meanwhile.
    pub fn update(change: impl FnOnce(&mut Self)) -> crate::Result<()> {
        let mut settings = Self::load();
        change(&mut settings);
        settings.save()
    }

    /// Values outside what the settings allow become the nearest allowed value.
    fn clamped(mut self) -> Self {
        self.launch = self.launch.clamped();
        for choices in self.stores.values_mut().chain(self.games.values_mut()) {
            *choices = choices.clamped();
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

    pub fn store(&self, store_id: &str) -> Overrides {
        self.stores.get(store_id).copied().unwrap_or_default()
    }

    pub fn game(&self, game_key: &str) -> Overrides {
        self.games.get(game_key).copied().unwrap_or_default()
    }

    pub fn set_store(&mut self, store_id: &str, choices: Overrides) {
        set(&mut self.stores, store_id, choices);
    }

    pub fn set_game(&mut self, game_key: &str, choices: Overrides) {
        set(&mut self.games, game_key, choices);
    }
}

/// Keeps only choices that choose something.
fn set(all: &mut BTreeMap<String, Overrides>, key: &str, choices: Overrides) {
    if choices.is_empty() {
        all.remove(key);
    } else {
        all.insert(key.to_string(), choices);
    }
}

fn path() -> crate::Result<PathBuf> {
    Ok(crate::data_dir()?.join("settings.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn out_of_range_values_are_clamped() {
        let settings: Settings =
            serde_json::from_str(r#"{"launch": {"checkMs": 1, "endGraceS": 99999}, "stores": {"epic": {"endGraceS": 0}}, "games": {"xbox:A": {"startTimeoutS": 1}}}"#).unwrap();
        let settings = settings.clamped();
        assert_eq!(settings.launch.check_ms, 100);
        assert_eq!(settings.launch.end_grace_s, 600);
        assert_eq!(settings.launch.start_timeout_s, 90);
        assert_eq!(settings.game("xbox:A").start_timeout_s, Some(10));
        assert_eq!(settings.store("epic").end_grace_s, Some(0));
    }

    /// Version 0.0.1 saved a store's settings whole; each value reads as a choice.
    #[test]
    fn older_store_settings_read_as_choices() {
        let settings: Settings = serde_json::from_str(r#"{"stores": {"xbox": {"bridge": false, "layout": true, "appRunning": "restart"}}}"#).unwrap();
        let xbox = settings.store("xbox");
        assert_eq!((xbox.bridge, xbox.layout, xbox.overlay, xbox.app_running), (Some(false), Some(true), None, Some(AppRunning::Restart)));
    }

    #[test]
    fn empty_choices_are_not_kept() {
        let mut settings = Settings::default();
        settings.set_store("epic", Overrides { overlay: Some(false), ..Default::default() });
        settings.set_game("epic:Fortnite", Overrides { bridge: Some(true), ..Default::default() });
        assert_eq!(Overrides { bridge: Some(true), ..Default::default() }.over(settings.store("epic")).overlay, Some(false));
        settings.set_store("epic", Overrides::default());
        settings.set_game("epic:Fortnite", Overrides::default());
        assert!(settings.stores.is_empty() && settings.games.is_empty());
    }
}

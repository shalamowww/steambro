//! What a launch does for a game, and why. The settings (settings.rs) hold the user's choices for
//! each store and game; this module resolves them over the defaults into one `Behavior`, and says
//! whether steambro stands in for Steam, from what the game's store declares (`Store::unseen`) and
//! what happened when the game started. launch.rs follows it, and the settings page shows what
//! each game inherits from it, so the rules live only here.

use std::time::Duration;

use serde::Serialize;

use crate::library::{Game, Store, Unseen};
use crate::settings::{Knobs, Overrides, Settings};

/// What a launch does for one game.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Behavior {
    pub knobs: Knobs,
    /// How often the launch looks for the game's processes.
    pub check: Duration,
    /// How long the game may take to appear after its store started it.
    pub start_timeout: Duration,
    /// Without a game process by then, the launch asks the store once more.
    pub nudge: Duration,
    /// The game has ended when none of its processes has run for this long.
    pub end_grace: Duration,
}

impl Behavior {
    /// The game's behavior. Each layer is over the one before: steambro's defaults, the store's own
    /// (`Store::defaults`), the user's choices for the store, and the user's choices for the game.
    ///
    /// A store's bridge and layout are for its games: an app like Netflix gets no bridge and keeps
    /// Steam's desktop layout unless its own choices say otherwise, and turning its bridge on brings
    /// the game's layout with it. A store's full screen is for its apps: a game keeps its own window
    /// unless its own choice says otherwise. The bridge needs the game's layout: without it, Steam
    /// gives its virtual controllers nothing to copy. A store can need longer than the usual wait
    /// for a game to start (`Store::start_timeout`).
    pub fn of(settings: &Settings, game: &Game) -> Self {
        resolve(settings, game, settings.game(&game.key()))
    }
}

fn resolve(settings: &Settings, game: &Game, own: Overrides) -> Behavior {
    let store = store_choices(settings, game.store);
    let values = values(store);
    let is_app = game.store.is_app(game);
    let bridge = own.bridge.unwrap_or(!is_app && values.bridge);
    let layout = own.layout.unwrap_or(if is_app { bridge } else { values.layout });
    let launch = settings.launch;
    let start = secs(launch.start_timeout_s).max(game.store.start_timeout().unwrap_or_default());
    Behavior {
        knobs: Knobs {
            bridge: bridge && layout,
            layout,
            overlay: own.overlay.unwrap_or(values.overlay),
            overlay_keys: own.overlay_keys.unwrap_or(values.overlay_keys),
            exit_ends_game: own.exit_ends_game.unwrap_or(values.exit_ends_game),
            app_running: own.app_running.unwrap_or(values.app_running),
            close_app: own.close_app.unwrap_or(values.close_app),
            full_screen: own.full_screen.unwrap_or(is_app && values.full_screen),
        },
        check: Duration::from_millis(launch.check_ms.into()),
        start_timeout: own.start_timeout_s.or(store.start_timeout_s).map_or(start, secs),
        nudge: secs(launch.nudge_s),
        end_grace: secs(own.end_grace_s.or(store.end_grace_s).unwrap_or(launch.end_grace_s)),
    }
}

/// The store's settings as its settings card shows them: the user's choices for the store over the
/// store's and steambro's defaults.
pub fn store_values(settings: &Settings, store: &dyn Store) -> Knobs {
    values(store_choices(settings, store))
}

fn store_choices(settings: &Settings, store: &dyn Store) -> Overrides {
    settings.store(store.id()).over(store.defaults())
}

/// The choices, with steambro's defaults where there are none.
fn values(choices: Overrides) -> Knobs {
    let d = Knobs::default();
    Knobs {
        bridge: choices.bridge.unwrap_or(d.bridge),
        layout: choices.layout.unwrap_or(d.layout),
        overlay: choices.overlay.unwrap_or(d.overlay),
        overlay_keys: choices.overlay_keys.unwrap_or(d.overlay_keys),
        exit_ends_game: choices.exit_ends_game.unwrap_or(d.exit_ends_game),
        app_running: choices.app_running.unwrap_or(d.app_running),
        close_app: choices.close_app.unwrap_or(d.close_app),
        full_screen: choices.full_screen.unwrap_or(d.full_screen),
    }
}

/// What a game gets where it has no choice of its own, setting by setting: each value is what the
/// game would have without its own choice for that setting. The settings page shows these as the
/// defaults of a game's choices.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Inherited {
    #[serde(flatten)]
    pub knobs: Knobs,
    pub start_timeout_s: u64,
    pub end_grace_s: u64,
}

pub fn inherited(settings: &Settings, game: &Game) -> Inherited {
    let own = settings.game(&game.key());
    let without = |clear: fn(&mut Overrides)| {
        let mut own = own;
        clear(&mut own);
        resolve(settings, game, own)
    };
    Inherited {
        knobs: Knobs {
            bridge: without(|o| o.bridge = None).knobs.bridge,
            layout: without(|o| o.layout = None).knobs.layout,
            overlay: without(|o| o.overlay = None).knobs.overlay,
            overlay_keys: without(|o| o.overlay_keys = None).knobs.overlay_keys,
            exit_ends_game: without(|o| o.exit_ends_game = None).knobs.exit_ends_game,
            app_running: without(|o| o.app_running = None).knobs.app_running,
            close_app: without(|o| o.close_app = None).knobs.close_app,
            full_screen: without(|o| o.full_screen = None).knobs.full_screen,
        },
        start_timeout_s: without(|o| o.start_timeout_s = None).start_timeout.as_secs(),
        end_grace_s: without(|o| o.end_grace_s = None).end_grace.as_secs(),
    }
}

/// Why steambro stands in for Steam during the launch: the game's controller layout, the bridge,
/// and the overlay, as the game's behavior says. `None` when Steam sees the game. `outside_steam`
/// is what the store found when it started the game (`Started::outside_steam`).
pub fn stand_in(unseen: Unseen, outside_steam: bool) -> Option<&'static str> {
    match unseen {
        Unseen::Always => Some("Windows starts the game, so Steam can't see it"),
        Unseen::AppOutside if outside_steam => Some("the store's app runs outside Steam's launch, so Steam can't see the game"),
        Unseen::Admin if outside_steam => Some("the game runs as administrator, so Steam can't see it"),
        Unseen::AppOutside | Unseen::Admin => None,
    }
}

fn secs(s: u32) -> Duration {
    Duration::from_secs(s.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::test_game;
    use crate::settings::AppRunning;

    #[test]
    fn games_follow_their_store_unless_overridden() {
        let mut settings = Settings::default();
        let fortnite = test_game("epic", "Fortnite", false);
        let netflix = test_game("xbox", "Netflix", true);
        let xbox_game = test_game("xbox", "Game", false);
        let behavior = |settings: &Settings, game: &Game| Behavior::of(settings, game).knobs;

        let k = behavior(&settings, &fortnite);
        assert!(k.bridge && k.layout && k.overlay && k.overlay_keys && k.exit_ends_game && k.close_app);
        assert_eq!(k.app_running, AppRunning::Ask);
        assert!(!k.full_screen, "games keep their own window");
        let k = behavior(&settings, &netflix);
        assert!(!k.bridge && !k.layout && k.overlay && k.full_screen, "apps get the desktop layout and full screen");
        settings.set_store("xbox", Overrides { full_screen: Some(false), ..Default::default() });
        assert!(!behavior(&settings, &netflix).full_screen);
        settings.set_store("xbox", Overrides::default());

        settings.set_store("epic", Overrides { bridge: Some(false), app_running: Some(AppRunning::Restart), overlay: Some(false), ..Default::default() });
        let k = behavior(&settings, &fortnite);
        assert!(!k.bridge && k.layout && !k.overlay);
        assert_eq!(k.app_running, AppRunning::Restart);
        assert!(behavior(&settings, &xbox_game).bridge, "another store's choices don't count");

        settings.set_game(&netflix.key(), Overrides { bridge: Some(true), ..Default::default() });
        let k = behavior(&settings, &netflix);
        assert!(k.bridge && k.layout, "an app's own bridge brings the layout");
        settings.set_game(&fortnite.key(), Overrides { bridge: Some(true), app_running: Some(AppRunning::Leave), exit_ends_game: Some(false), ..Default::default() });
        let k = behavior(&settings, &fortnite);
        assert!(k.bridge && !k.exit_ends_game);
        assert_eq!(k.app_running, AppRunning::Leave);

        settings.set_game(&xbox_game.key(), Overrides { layout: Some(false), ..Default::default() });
        let k = behavior(&settings, &xbox_game);
        assert!(!k.layout && !k.bridge, "the bridge needs the game's layout");

        settings.set_store("xbox", Overrides { layout: Some(false), ..Default::default() });
        assert!(behavior(&settings, &netflix).layout, "an app's own bridge brings the layout even when the store's is off");

        let gog = test_game("gog", "Game", false);
        settings.set_game(&gog.key(), Overrides { full_screen: Some(true), ..Default::default() });
        assert!(behavior(&settings, &gog).full_screen && !behavior(&settings, &test_game("gog", "Other", false)).full_screen);
    }

    #[test]
    fn timings_come_from_the_settings_the_store_and_the_game() {
        let mut settings = Settings::default();
        let game = Behavior::of(&settings, &test_game("xbox", "Game", false));
        assert_eq!((game.check, game.start_timeout, game.nudge, game.end_grace), (secs(1), secs(90), secs(20), secs(8)));
        // The Epic Games launcher may update itself first, so Epic's games may take 5 minutes.
        let fortnite = test_game("epic", "Fortnite", false);
        assert_eq!(Behavior::of(&settings, &fortnite).start_timeout, secs(300));
        settings.set_store("epic", Overrides { end_grace_s: Some(30), ..Default::default() });
        assert_eq!(Behavior::of(&settings, &fortnite).end_grace, secs(30));
        settings.set_game(&fortnite.key(), Overrides { start_timeout_s: Some(30), end_grace_s: Some(0), ..Default::default() });
        let fortnite = Behavior::of(&settings, &fortnite);
        assert_eq!((fortnite.start_timeout, fortnite.end_grace), (secs(30), Duration::ZERO));
    }

    #[test]
    fn a_game_inherits_what_it_would_get_without_its_own_choice() {
        let mut settings = Settings::default();
        let netflix = test_game("xbox", "Netflix", true);
        settings.set_game(&netflix.key(), Overrides { bridge: Some(true), ..Default::default() });
        let app = inherited(&settings, &netflix);
        assert!(!app.knobs.bridge, "apps get no bridge from their store");
        assert!(app.knobs.layout, "the app's own bridge brings the layout");
        assert!(app.knobs.full_screen);

        let fortnite = test_game("epic", "Fortnite", false);
        settings.set_store("epic", Overrides { close_app: Some(false), ..Default::default() });
        settings.set_game(&fortnite.key(), Overrides { overlay: Some(false), close_app: Some(true), ..Default::default() });
        let game = inherited(&settings, &fortnite);
        assert!(game.knobs.overlay && !game.knobs.close_app && !game.knobs.full_screen);
        assert_eq!((game.start_timeout_s, game.end_grace_s), (300, 8));
        assert!(!store_values(&settings, fortnite.store).close_app);
    }

    /// Each store's case of Steam not seeing its game, as the launch and the settings page see it.
    #[test]
    fn steambro_stands_in_where_steam_cant_see_the_game() {
        let cases = [
            ("xbox", true, false, true),
            ("xbox", false, false, true),
            ("battlenet", false, false, false),
            ("battlenet", false, true, true),
            ("epic", false, false, false),
            ("epic", false, true, true),
            ("gog", false, false, false),
            ("gog", false, true, true),
            ("exe", false, false, false),
            ("exe", false, true, true),
        ];
        for (store, app, outside_steam, expected) in cases {
            let game = test_game(store, "Game", app);
            assert_eq!(stand_in(game.store.unseen(), outside_steam).is_some(), expected, "{store}, app {app}, outside Steam {outside_steam}");
        }
    }
}

//! Epic Games: games from the Epic Games launcher's install manifests, artwork from its catalog
//! cache, started through the launcher.

mod install;
mod launcher;

use std::path::PathBuf;
use std::time::Duration;

use super::{Game, Started, Store, Unseen, is_named};
use crate::art::{Origin, Slot};
use crate::settings::{HandOver, Overrides};
use crate::steam::Shortcut;

pub struct Epic;

/// The launcher may sign in, update itself, or update the game before the game starts.
const START_TIMEOUT: Duration = Duration::from_secs(300);

/// What the Epic module knows about a game beyond the common fields. The game's ID is its Epic
/// app name.
struct App {
    namespace: String,
    item_id: String,
    /// Official artwork URLs from the launcher's catalog cache, by the Steam slot they fill.
    art: Vec<(Slot, String)>,
    /// The exe the launcher starts.
    launch_exe: PathBuf,
    /// The launcher uploads the game's saves to Epic's cloud after the game ends.
    cloud_saves: bool,
}

/// How long the launcher that stayed for a game with Epic cloud saves has after the game, to
/// upload them, before steambro closes it. Steam shows the game as running that long.
const CLOUD_SAVE_WAIT: Duration = Duration::from_secs(15);

impl Store for Epic {
    fn id(&self) -> &'static str {
        "epic"
    }

    fn name(&self) -> &'static str {
        "Epic Games"
    }

    fn art_source(&self) -> &'static str {
        "Epic Games Store"
    }

    fn detect(&'static self, _warnings: &mut Vec<String>) -> crate::Result<Vec<Game>> {
        Ok(install::find()?
            .into_iter()
            .map(|g| {
                let details = App { namespace: g.namespace, item_id: g.item_id, art: g.art, launch_exe: g.launch_exe, cloud_saves: g.cloud_saves };
                Game::new(self, g.app_name, g.name, g.install_dir, details)
            })
            .collect())
    }

    /// The catalog's artwork. Many games have no icon there, like Rocket League; the icon of the exe
    /// that the launcher starts stands in.
    fn art_origins(&self, game: &Game) -> crate::Result<Vec<(Slot, Origin)>> {
        let app = game.details::<App>();
        let mut origins: Vec<(Slot, Origin)> = app.art.iter().map(|(slot, url)| (*slot, Origin::Url(url.clone()))).collect();
        if !origins.iter().any(|(slot, _)| *slot == Slot::Icon) && app.launch_exe.is_file() {
            origins.push((Slot::Icon, Origin::ExeIcon(app.launch_exe.clone())));
        }
        Ok(origins)
    }

    /// steambro starts Epic games: the launcher starts the game and lets the shortcut end at once,
    /// so Steam would lose the game.
    fn can_add(&self, _game: &Game) -> crate::Result<()> {
        launcher::exe().ok_or("the Epic Games launcher isn't registered for its links; reinstall it")?;
        Ok(())
    }

    /// The Epic Games launcher started outside Steam starts its games outside Steam too.
    fn unseen(&self) -> Unseen {
        Unseen::AppOutside
    }

    /// The launcher that steambro opened closes once the game runs. Inside Steam's launch it gets
    /// Steam's overlay too, and Steam then runs two overlays for the game: it sent the Steam button
    /// to the launcher's, which nobody sees (Rocket League, 2026-11-10). With the launcher closed,
    /// Steam keeps only the game's. A closed launcher doesn't upload a game's Epic cloud saves when
    /// the game ends; the user can keep it for such a game (`HandOver::Keep`).
    fn defaults(&self) -> Overrides {
        Overrides { hand_over: Some(HandOver::Close), ..Default::default() }
    }

    fn start_timeout(&self) -> Option<Duration> {
        Some(START_TIMEOUT)
    }

    fn launch_description(&self, game: &Game) -> String {
        uri(game)
    }

    /// The launcher's link to the game, as on the desktop shortcuts it creates, in the shortcut's
    /// target or its launch options.
    fn is_launched_by(&self, game: &Game, shortcut: &Shortcut) -> bool {
        let exe = shortcut.exe_path();
        let text = format!("{} {}", exe.display(), shortcut.launch_options);
        let named = launcher::uri_parts(&text).last().is_some_and(|app| app.eq_ignore_ascii_case(game.id()));
        named && (is_named(&exe, "EpicGamesLauncher.exe") || text.to_lowercase().contains("com.epicgames.launcher://"))
    }

    /// Hands the game's link to the launcher. A launcher that steambro starts here runs inside
    /// Steam's launch, so Steam's overlay and controller reach the game. Steam counts it as part
    /// of the game, so steambro closes it again: once the game runs (the handover, launch.rs), or
    /// after the game (`Started::after`) when it stayed until then. Then a game with Epic cloud
    /// saves first gives it `CLOUD_SAVE_WAIT` to upload them.
    fn start(&self, game: &Game) -> crate::Result<Started> {
        let exe: PathBuf = launcher::exe().ok_or("the Epic Games launcher isn't registered for its links; reinstall it")?;
        let opened = !launcher::is_running();
        if opened {
            crate::log!("the Epic Games launcher isn't running; it starts with the game's link");
        } else {
            // Started outside this launch, it may sit outside Steam's view of the game.
            crate::log!("the Epic Games launcher is already running");
        }
        launcher::launch(&exe, &uri(game))?;
        let cloud_saves = game.details::<App>().cloud_saves;
        if cloud_saves {
            crate::log!("the game keeps its saves in Epic's cloud; the launcher uploads them when the game ends, if it is still open then");
        }
        Ok(Started {
            folders: vec![game.install_dir().to_path_buf()],
            packages: Vec::new(),
            not_started: "the Epic Games launcher didn't start the game. It may need an update or a sign-in; have a look in the launcher.".into(),
            nudge: None,
            outside_steam: !opened,
            after: opened.then(|| {
                Box::new(move || {
                    if cloud_saves {
                        crate::log!("giving the Epic Games launcher {}s to upload the game's cloud saves", CLOUD_SAVE_WAIT.as_secs());
                        std::thread::sleep(CLOUD_SAVE_WAIT);
                    }
                    crate::log!("closing the Epic Games launcher, which steambro opened");
                    let closed = install::data_dir().ok_or_else(|| "its data folder is gone".into()).and_then(|data| launcher::close(&data));
                    match closed {
                        Ok(()) => crate::log!("the Epic Games launcher has closed"),
                        Err(e) => crate::log!("left the Epic Games launcher open ({e}); Steam shows the game as running until it closes"),
                    }
                }) as Box<dyn FnOnce()>
            }),
        })
    }

    fn app_name(&self) -> Option<&'static str> {
        Some("Epic Games Launcher")
    }

    /// The launcher's link starts `EOSBootStrapper.exe`, which starts the launcher that stays, so
    /// the launch can't follow it from its own processes.
    fn app_exes(&self) -> &'static [&'static str] {
        &[launcher::EXE]
    }

    fn app_running(&self) -> bool {
        launcher::is_running()
    }

    fn app_busy(&self) -> Option<String> {
        install::data_dir().and_then(|data| launcher::busy(&data)).map(String::from)
    }

    fn close_app(&self, _game: &Game) -> crate::Result<()> {
        launcher::close(&install::data_dir().ok_or("the Epic Games launcher's data folder is gone")?)
    }

    /// For `HandOver::Reopen`, which the user can choose; Epic's default closes the launcher
    /// (`defaults`).
    fn reopen_app(&self, _game: &Game) -> crate::Result<u32> {
        launcher::reopen(&launcher::exe().ok_or("the Epic Games launcher isn't registered for its links")?)
    }

    fn diagnostics(&self, _game: Option<&Game>) -> Vec<(String, String)> {
        let mut lines = vec![
            ("Epic launcher".to_string(), launcher::exe().map_or("not registered".into(), |p| p.display().to_string())),
            ("Epic launcher running".to_string(), launcher::is_running().to_string()),
            ("Epic launcher busy".to_string(), self.app_busy().unwrap_or_else(|| "no".into())),
        ];
        lines.extend(launcher::settings());
        lines
    }

    fn details(&self, game: &Game) -> Vec<(String, String)> {
        let app = game.details::<App>();
        let mut lines = vec![("catalog item".to_string(), format!("{}:{}", app.namespace, app.item_id)), ("Epic cloud saves".to_string(), app.cloud_saves.to_string())];
        lines.extend(app.art.iter().map(|(slot, url)| (slot.label().to_string(), url.clone())));
        lines
    }
}

fn uri(game: &Game) -> String {
    let app = game.details::<App>();
    launcher::launch_uri(&app.namespace, &app.item_id, game.id())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::Behavior;
    use crate::settings::Settings;

    #[test]
    fn a_game_with_cloud_saves_closes_the_launcher_too_unless_the_user_keeps_it() {
        let details = App { namespace: "ns".into(), item_id: "item".into(), art: Vec::new(), launch_exe: PathBuf::new(), cloud_saves: true };
        let game = Game::new(&Epic, "Game".into(), "Game".into(), PathBuf::new(), details);
        let mut settings = Settings::default();
        assert_eq!(Behavior::of(&settings, &game).knobs.hand_over, HandOver::Close, "Epic cloud saves don't keep the launcher");
        assert!(Epic.details(&game).contains(&("Epic cloud saves".to_string(), "true".to_string())), "`steambro list -v` doesn't say it");
        assert_eq!(crate::plan::store_values(&settings, &Epic).hand_over, HandOver::Close);
        settings.set_game(&game.key(), Overrides { hand_over: Some(HandOver::Keep), ..Default::default() });
        assert_eq!(Behavior::of(&settings, &game).knobs.hand_over, HandOver::Keep);
        assert_eq!(Epic.app_exes(), ["EpicGamesLauncher.exe"]);
        assert!(!Epic.windows_may_hide(&game));
    }
}

//! Epic Games: games from the Epic Games launcher's install manifests, artwork from its catalog
//! cache, started through the launcher.

mod install;
mod launcher;

use std::path::PathBuf;
use std::time::Duration;

use super::{Game, Started, Store, Unseen, is_named};
use crate::art::{Origin, Slot};
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
}

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
                let details = App { namespace: g.namespace, item_id: g.item_id, art: g.art };
                Game::new(self, g.app_name, g.name, g.install_dir, details)
            })
            .collect())
    }

    fn art_origins(&self, game: &Game) -> crate::Result<Vec<(Slot, Origin)>> {
        Ok(game.details::<App>().art.iter().map(|(slot, url)| (*slot, Origin::Url(url.clone()))).collect())
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
    /// Steam's launch, so Steam's overlay and controller reach the game; steambro closes it again
    /// after the game, because Steam counts it as part of the game.
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
        Ok(Started {
            folders: vec![game.install_dir().to_path_buf()],
            packages: Vec::new(),
            not_started: "the Epic Games launcher didn't start the game. It may need an update or a sign-in; have a look in the launcher.".into(),
            nudge: None,
            outside_steam: !opened,
            after: opened.then(|| {
                Box::new(|| {
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

    fn app_running(&self) -> bool {
        launcher::is_running()
    }

    fn app_busy(&self) -> Option<String> {
        install::data_dir().and_then(|data| launcher::busy(&data)).map(String::from)
    }

    fn close_app(&self, _game: &Game) -> crate::Result<()> {
        launcher::close(&install::data_dir().ok_or("the Epic Games launcher's data folder is gone")?)
    }

    fn details(&self, game: &Game) -> Vec<(String, String)> {
        let app = game.details::<App>();
        let mut lines = vec![("catalog item".to_string(), format!("{}:{}", app.namespace, app.item_id))];
        lines.extend(app.art.iter().map(|(slot, url)| (slot.label().to_string(), url.clone())));
        lines
    }
}

fn uri(game: &Game) -> String {
    let app = game.details::<App>();
    launcher::launch_uri(&app.namespace, &app.item_id, game.id())
}

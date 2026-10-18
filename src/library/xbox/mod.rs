//! The Xbox app and Game Pass: PC games installed as Windows packages, artwork from the Microsoft
//! Store, started the way the Start menu starts them. Also the Windows apps the user added to the
//! Xbox app's library, like Netflix (added.rs).
//!
//! Windows' package broker creates every packaged process, so an Xbox game runs outside Steam's
//! launch, whoever asks for it: Steam tracks only steambro, and its overlay and Steam Input layout
//! don't reach the game. Other ways fail too: Steam has no permission to start the game's exe,
//! `GameLaunchHelper.exe` started directly asks Windows to start the game again, and Windows
//! refuses `CreateProcess` with the package's name. So steambro stands in for Steam (`Unseen`).

mod activate;
mod added;
mod catalog;
mod packages;

use std::path::PathBuf;

use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};

use super::{Game, Started, Store, Unseen, is_named};
use crate::art::{Anchor, Origin, Slot};
use crate::steam::Shortcut;

pub struct Xbox;

/// What the Xbox module knows about a game beyond the common fields. The game's ID is its AUMID.
struct Package {
    /// Package family name; the Microsoft Store catalog finds the game's artwork by it.
    family: String,
    /// The package folder as Windows reports it. A running game's path can start with it or with
    /// the install folder.
    package_dir: PathBuf,
    /// The game's own executable, as declared in MicrosoftGame.config. `None` for an app the
    /// user added in the Xbox app, like Netflix.
    exe: Option<PathBuf>,
}

/// An Xbox game, or an app like Netflix, for other modules' tests.
#[cfg(test)]
pub fn test_game(name: &str, app: bool) -> Game {
    let package = Package { family: format!("Test.{name}_test"), package_dir: PathBuf::new(), exe: (!app).then(|| PathBuf::from("Game.exe")) };
    Game::new(&Xbox, name.into(), name.into(), PathBuf::new(), package)
}

impl Store for Xbox {
    fn id(&self) -> &'static str {
        "xbox"
    }

    fn name(&self) -> &'static str {
        "Xbox"
    }

    fn art_source(&self) -> &'static str {
        "Microsoft Store"
    }

    /// The Xbox games, and the Windows apps the user added to the Xbox app's library that aren't
    /// games already.
    fn detect(&'static self, warnings: &mut Vec<String>) -> crate::Result<Vec<Game>> {
        let mut found = packages::installed_games(warnings)?;
        match packages::installed_apps(&added::list()) {
            Ok(apps) => found.extend(apps.into_iter().filter(|app| !found.iter().any(|g| g.aumid.eq_ignore_ascii_case(&app.aumid))).collect::<Vec<_>>()),
            Err(e) => warnings.push(format!("couldn't read the apps added in the Xbox app: {e}")),
        }
        Ok(found
            .into_iter()
            .map(|g| {
                let exe = g.executable.map(|exe| g.install_dir.join(exe));
                let details = Package { family: g.family, package_dir: g.package_dir, exe };
                Game::new(self, g.aumid, g.name, g.install_dir, details)
            })
            .collect())
    }

    fn art_origins(&self, game: &Game) -> crate::Result<Vec<(Slot, Origin)>> {
        catalog::art(&game.details::<Package>().family)
    }

    fn anchor(&self, slot: Slot) -> Anchor {
        catalog::anchor(slot)
    }

    fn launch_description(&self, game: &Game) -> String {
        format!("Windows app {}", game.id())
    }

    /// UWP Hook's launch options are "<AUMID> <game exe to wait for>".
    fn is_launched_by(&self, game: &Game, shortcut: &Shortcut) -> bool {
        is_named(&shortcut.exe_path(), "UWPHook.exe")
            && shortcut.launch_options.split_whitespace().next().is_some_and(|id| id.eq_ignore_ascii_case(game.id()))
    }

    /// Starts the app through Windows' activation manager, as the Start menu does. Windows runs the
    /// package's entry point, usually GameLaunchHelper.exe, which starts the game. Windows creates
    /// the game, so Steam sees only steambro.
    fn start(&self, game: &Game) -> crate::Result<Started> {
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok()? };
        activate::through_windows(game.id())?;
        crate::log!("Windows started the app");
        let package = game.details::<Package>();
        Ok(Started {
            folders: vec![game.install_dir().to_path_buf(), package.package_dir.clone()],
            // A web app (Netflix, Prime Video) runs as Edge's pwahelper.exe, with its package's identity.
            packages: vec![package.family.clone()],
            not_started: if package.exe.is_some() {
                "the game didn't start. Try starting it from the Xbox app once; it may need an update or a sign-in.".into()
            } else {
                format!("{} didn't start. Try starting it from the Start menu once.", game.name())
            },
            nudge: None,
            outside_steam: false,
            after: None,
        })
    }

    /// Windows starts packaged games outside Steam's launch.
    fn unseen(&self) -> Unseen {
        Unseen::Always
    }

    /// An app added in the Xbox app, like Netflix, isn't a game.
    fn is_app(&self, game: &Game) -> bool {
        game.details::<Package>().exe.is_none()
    }

    fn details(&self, game: &Game) -> Vec<(String, String)> {
        match &game.details::<Package>().exe {
            Some(exe) => vec![("game".into(), exe.display().to_string())],
            None => vec![("app".into(), "added in the Xbox app".into())],
        }
    }
}

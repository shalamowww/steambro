//! Battle.net: games from Battle.net's install database, names, launch codes and artwork from the
//! Battle.net app's catalog cache, started through the Battle.net app.

mod client;
mod install;

use std::path::PathBuf;

use super::{Game, Started, Store, Unseen, is_named};
use crate::art::{Origin, Slot};
use crate::steam::Shortcut;

pub struct BattleNet;

/// What the Battle.net module knows about a game beyond the common fields. The game's ID is its
/// install ID from product.db, like `s2` or `wow_classic_beta`.
struct Product {
    /// Battle.net.exe.
    client: PathBuf,
    /// Launch code, like `S2` or `WoWF`. Unknown when Battle.net hasn't cached its catalog.
    code: Option<String>,
    /// Official artwork in Battle.net's cache, by the Steam slot it fills.
    art: Vec<(Slot, PathBuf)>,
}

impl Store for BattleNet {
    fn id(&self) -> &'static str {
        "battlenet"
    }

    fn name(&self) -> &'static str {
        "Battle.net"
    }

    fn art_source(&self) -> &'static str {
        "Battle.net"
    }

    fn detect(&'static self, _warnings: &mut Vec<String>) -> crate::Result<Vec<Game>> {
        let Some(found) = install::find()? else { return Ok(Vec::new()) };
        Ok(found
            .games
            .into_iter()
            .map(|g| {
                let details = Product { client: found.exe.clone(), code: g.product_id, art: g.art };
                Game::new(self, g.uid, g.name, g.install_dir, details)
            })
            .collect())
    }

    fn art_origins(&self, game: &Game) -> crate::Result<Vec<(Slot, Origin)>> {
        Ok(game.details::<Product>().art.iter().map(|(slot, path)| (*slot, Origin::File(path.clone()))).collect())
    }

    /// steambro starts Battle.net games: a Battle.net that isn't running only opens the game's
    /// page for a launch command, and one that runs outside Steam hides the game from Steam.
    fn can_add(&self, game: &Game) -> crate::Result<()> {
        code(game)?;
        Ok(())
    }

    /// Battle.net started outside Steam starts its games outside Steam too.
    fn unseen(&self) -> Unseen {
        Unseen::AppOutside
    }

    fn launch_description(&self, game: &Game) -> String {
        let product = game.details::<Product>();
        match &product.code {
            Some(code) => format!("\"{}\" --exec=\"launch {code}\"", product.client.display()),
            None => "unknown: Battle.net hasn't cached this game's launch code".to_string(),
        }
    }

    /// Battle.net with the game's launch code, as people add Blizzard games to Steam by hand.
    fn is_launched_by(&self, game: &Game, shortcut: &Shortcut) -> bool {
        let exe = shortcut.exe_path();
        let via_battle_net = is_named(&exe, "Battle.net.exe") || is_named(&exe, "Battle.net Launcher.exe");
        let wanted = game.details::<Product>().code.as_ref().map(|code| format!("launch {}", code.to_lowercase()));
        via_battle_net && wanted.is_some_and(|w| shortcut.launch_options.to_lowercase().contains(&w))
    }

    /// Makes sure the Battle.net app runs and has signed in, then sends the launch command. A
    /// Battle.net that steambro starts here runs inside Steam's launch, so Steam's overlay and
    /// controller reach the game. steambro closes it again after the game: Steam counts Battle.net
    /// as part of the game and shows the game as running until Battle.net has closed.
    fn start(&self, game: &Game) -> crate::Result<Started> {
        let code = code(game)?;
        let client = game.details::<Product>().client.clone();
        let opened = !client::is_running();
        if opened {
            crate::log!("starting Battle.net and waiting until it has signed in");
            client::start(&client)?;
            crate::log!("Battle.net has signed in");
        } else {
            // Started outside this launch, it may sit outside Steam's view of the game.
            crate::log!("Battle.net is already running");
        }
        crate::log!("asking Battle.net to launch {code}");
        client::command(&client, &format!("launch {code}"))?;
        let again = (client.clone(), code);
        Ok(Started {
            folders: vec![game.install_dir().to_path_buf()],
            packages: Vec::new(),
            not_started: "Battle.net didn't start the game. It may need an update; have a look in Battle.net.".into(),
            nudge: Some(Box::new(move || {
                crate::log!("no game yet; asking Battle.net again");
                client::command(&again.0, &format!("launch {}", again.1))
            })),
            outside_steam: !opened,
            after: opened.then(|| {
                Box::new(move || {
                    crate::log!("closing Battle.net, which steambro opened");
                    if let Err(e) = client::command(&client, "shutdown") {
                        crate::log!("couldn't close Battle.net: {e}");
                    }
                }) as Box<dyn FnOnce()>
            }),
        })
    }

    fn app_name(&self) -> Option<&'static str> {
        Some("Battle.net")
    }

    fn app_running(&self) -> bool {
        client::is_running()
    }

    fn close_app(&self, game: &Game) -> crate::Result<()> {
        client::close(&game.details::<Product>().client)
    }

    fn details(&self, game: &Game) -> Vec<(String, String)> {
        let product = game.details::<Product>();
        let mut lines = vec![("launch code".to_string(), product.code.clone().unwrap_or_else(|| "unknown".into()))];
        lines.extend(product.art.iter().map(|(slot, path)| (slot.label().to_string(), path.display().to_string())));
        lines
    }
}

fn code(game: &Game) -> crate::Result<String> {
    game.details::<Product>().code.clone().ok_or_else(|| format!("Battle.net hasn't cached the launch code of {}", game.name()).into())
}

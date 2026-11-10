//! Battle.net: games from Battle.net's install database, names, launch codes and artwork from the
//! Battle.net app's catalog cache, started through the Battle.net app.

mod client;
mod install;

use std::path::PathBuf;

use super::{Game, Started, Store, Unseen, is_named, msstore};
use crate::art::{Anchor, Origin, Slot};
use crate::settings::{HandOver, Overrides};
use crate::steam::Shortcut;

pub struct BattleNet;

/// What the Battle.net module knows about a game beyond the common fields. The game's ID is its
/// install ID from product.db, like `s2` or `wowf`.
struct Product {
    /// Battle.net.exe.
    client: PathBuf,
    /// Launch code, like `S2` or `WoWF`. Unknown when Battle.net hasn't cached its catalog.
    code: Option<String>,
    /// Official artwork from Battle.net's catalog, and the game's Microsoft Store ID.
    art: install::Art,
}

/// The slots the Microsoft Store fills for a Blizzard game it sells: its poster, its 4K hero art and
/// its titled art are far bigger than Battle.net's. Battle.net's catalog keeps the transparent logo
/// and the icon, which the Store lacks.
const STORE_SLOTS: [Slot; 3] = [Slot::Cover, Slot::Hero, Slot::Wide];

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

    fn slot_source(&self, game: &Game, slot: Slot) -> &'static str {
        if game.details::<Product>().art.store_id.is_some() && STORE_SLOTS.contains(&slot) { "Microsoft Store" } else { "Battle.net" }
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

    /// Battle.net's catalog, with the Microsoft Store's images in `STORE_SLOTS` for a game the Store
    /// sells. If the Store can't be asked now, that is the error, as for any store: Battle.net's
    /// smaller images don't stand in, so steambro's Store images aren't swapped for them and back,
    /// and the slots are tried again later (`sync::retry_pending`, `sync::refresh`). A Store that
    /// doesn't know the game leaves Battle.net's images in place.
    fn art_origins(&self, game: &Game) -> crate::Result<Vec<(Slot, Origin)>> {
        let art = &game.details::<Product>().art;
        let mut origins = art.slots.clone();
        if let Some(id) = &art.store_id {
            let store = msstore::by_id(id).map_err(|e| format!("couldn't ask the Microsoft Store for the artwork of {}: {e}", game.name()))?;
            for (slot, origin) in store.into_iter().filter(|(slot, _)| STORE_SLOTS.contains(slot)) {
                origins.retain(|(s, _)| *s != slot);
                origins.push((slot, origin));
            }
        }
        Ok(origins)
    }

    /// The Store's titled art keeps its title at the top, and so does Battle.net's install
    /// background when it stands in for the wide cover.
    fn anchor(&self, game: &Game, slot: Slot) -> Anchor {
        let art = &game.details::<Product>().art;
        if art.store_id.is_some() {
            msstore::anchor(slot)
        } else if slot == Slot::Wide && art.wide_titled_top {
            Anchor::Top
        } else {
            Anchor::Center
        }
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

    /// Battle.net takes about 25 seconds to close after a game, and it takes longer while one of
    /// its windows is open, like a chat: Steam showed a game as running for 5 minutes after it
    /// ended. Handed over while the game runs, it closes during the game, and the one steambro
    /// opens again stays for chat and friends.
    fn defaults(&self) -> Overrides {
        Overrides { hand_over: Some(HandOver::Reopen), ..Default::default() }
    }

    /// Battle.net sometimes misses the first launch command while it starts (`Started::nudge` in
    /// `start`).
    fn asks_again(&self) -> bool {
        true
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
                    // A Battle.net set to exit when a game starts closed itself during the game. A
                    // Battle.net that runs now was opened later, by the user, so it stays; and one
                    // that is still closing doesn't answer, so a command would start a whole new one.
                    if client::exits_with_game() {
                        crate::log!("Battle.net closes by itself once a game starts, as its settings say, so steambro leaves Battle.net alone");
                        return;
                    }
                    // A command to a Battle.net that no longer runs would start a whole new one too.
                    if !client::is_running() {
                        crate::log!("Battle.net, which steambro opened, has closed already");
                        return;
                    }
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

    /// Battle.net's "When I launch a game: Exit Battle.net completely".
    fn app_closes_itself(&self) -> bool {
        client::exits_with_game()
    }

    fn close_app(&self, game: &Game) -> crate::Result<()> {
        client::close(&game.details::<Product>().client)
    }

    fn reopen_app(&self, game: &Game) -> crate::Result<u32> {
        client::reopen(&game.details::<Product>().client)
    }

    fn diagnostics(&self, game: Option<&Game>) -> Vec<(String, String)> {
        let mut lines = vec![
            ("Battle.net running".to_string(), client::is_running().to_string()),
            ("Battle.net exits when a game starts".to_string(), client::exits_with_game().to_string()),
        ];
        lines.extend(client::settings(game.map(|g| g.id())));
        lines
    }

    fn details(&self, game: &Game) -> Vec<(String, String)> {
        let product = game.details::<Product>();
        let mut lines = vec![("launch code".to_string(), product.code.clone().unwrap_or_else(|| "unknown".into()))];
        lines.extend(product.art.store_id.iter().map(|id| ("Microsoft Store ID".to_string(), id.clone())));
        lines.extend(product.art.slots.iter().map(|(slot, origin)| (slot.label().to_string(), origin.identity())));
        lines
    }
}

fn code(game: &Game) -> crate::Result<String> {
    game.details::<Product>().code.clone().ok_or_else(|| format!("Battle.net hasn't cached the launch code of {}", game.name()).into())
}

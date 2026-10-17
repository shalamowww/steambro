//! `steambro list`: the library as a table in the terminal.

use crate::Result;
use crate::art::Slot;
use crate::library::{Game, Library};
use crate::steam::{Shortcut, Steam};
use crate::sync::{self, Placement, State, Status};

pub fn list(verbose: bool) -> Result<()> {
    let library = Library::detect();
    for warning in &library.warnings {
        eprintln!("warning: {warning}");
    }
    let state = State::load()?;
    let accounts: Vec<(u32, Vec<Shortcut>)> = match Steam::locate() {
        Ok(steam) => steam.users()?.into_iter().map(|u| Ok((u.id, u.shortcuts()?))).collect::<Result<_>>()?,
        Err(e) => {
            eprintln!("warning: {e}");
            Vec::new()
        }
    };

    println!("{} games found", library.games.len());
    if accounts.is_empty() {
        print_table(&library, &state, None, verbose);
    }
    for account in &accounts {
        if accounts.len() > 1 {
            println!("\nSteam account {}", account.0);
        }
        print_table(&library, &state, Some(account), verbose);
    }
    Ok(())
}

fn print_table(library: &Library, state: &State, account: Option<&(u32, Vec<Shortcut>)>, verbose: bool) {
    let mut rows = vec![["GAME".to_string(), "SOURCE".to_string(), "IN STEAM".to_string(), "ARTWORK".to_string()]];
    for game in &library.games {
        let (in_steam, artwork) = describe(library, state, game, account);
        rows.push([game.name().to_string(), game.source().to_string(), in_steam, artwork]);
    }
    let widths: Vec<usize> = (0..3).map(|col| rows.iter().map(|r| r[col].chars().count()).max().unwrap_or(0)).collect();
    println!();
    for (i, row) in rows.iter().enumerate() {
        println!("  {:w0$}  {:w1$}  {:w2$}  {}", row[0], row[1], row[2], row[3], w0 = widths[0], w1 = widths[1], w2 = widths[2]);
        if verbose && i > 0 {
            print_details(&library.games[i - 1]);
        }
    }
}

/// Whether the game is in this Steam account and, if steambro put it there, what each artwork slot holds.
fn describe(library: &Library, state: &State, game: &Game, account: Option<&(u32, Vec<Shortcut>)>) -> (String, String) {
    let Some((user, shortcuts)) = account else { return ("-".to_string(), planned_art(game)) };
    match sync::status(library, state, *user, shortcuts, game) {
        Status::Steambro(app_id) => {
            let tracked = state.find(*user, &game.key()).expect("status found it");
            let outdated = match (shortcuts.iter().find(|s| s.app_id == app_id), crate::steambro_exe()) {
                (Some(shortcut), Ok(steambro)) => sync::is_outdated(game, shortcut, &steambro),
                _ => false,
            };
            let slots: Vec<String> = Slot::ALL
                .iter()
                .filter_map(|slot| {
                    let label = slot.label();
                    let own = if state.choice(&game.key(), *slot).own { " (yours)" } else { "" };
                    match tracked.art.get(slot) {
                        Some(Placement::Written) => Some(format!("{label}{own}")),
                        Some(Placement::Kept) => Some(format!("{label} (Steam's)")),
                        Some(Placement::Pending) | None => Some(format!("{label} (will retry)")),
                        Some(Placement::Unavailable) => None,
                    }
                })
                .collect();
            let added = if outdated { "added by steambro (old shortcut)" } else { "added by steambro" };
            (added.to_string(), slots.join(", "))
        }
        Status::User(_) => ("added by you".to_string(), "-".to_string()),
        Status::NotAdded => ("not yet".to_string(), planned_art(game)),
    }
}

/// Known only after asking the store, which `list` doesn't do for the Microsoft Store.
fn planned_art(game: &Game) -> String {
    format!("from {}", game.art_source())
}

fn print_details(game: &Game) {
    println!("      key:     {}", game.key());
    println!("      folder:  {}", game.install_dir().display());
    println!("      launch:  {}", game.launch_description());
    for (label, value) in game.store.details(game) {
        println!("      {:12} {value}", format!("{label}:"));
    }
}

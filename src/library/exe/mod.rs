//! Games the user added by their exe, in `%LOCALAPPDATA%\steambro\exes.json`. They start directly,
//! like GOG games: Steam sees them as games it started. The artwork is the exe's own icon; the other
//! slots take the user's images.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{Game, Started, Store, Unseen};
use crate::art::{Origin, Slot};

pub struct Exe;

/// A game the user added. The game's ID is a hash of the exe's path.
#[derive(Clone, Serialize, Deserialize)]
struct Added {
    id: String,
    name: String,
    exe: PathBuf,
    #[serde(default)]
    args: String,
}

impl Store for Exe {
    fn id(&self) -> &'static str {
        "exe"
    }

    fn name(&self) -> &'static str {
        "Other"
    }

    fn art_source(&self) -> &'static str {
        "the exe"
    }

    /// The added games whose exe is still there.
    fn detect(&'static self, _warnings: &mut Vec<String>) -> crate::Result<Vec<Game>> {
        Ok(list()?
            .into_iter()
            .filter(|added| added.exe.is_file())
            .map(|added| {
                let dir = added.exe.parent().map(Path::to_path_buf).unwrap_or_default();
                Game::new(self, added.id.clone(), added.name.clone(), dir, added)
            })
            .collect())
    }

    fn art_origins(&self, game: &Game) -> crate::Result<Vec<(Slot, Origin)>> {
        Ok(vec![(Slot::Icon, Origin::ExeIcon(game.details::<Added>().exe.clone()))])
    }

    /// An exe that runs as administrator starts through Windows' prompt, outside Steam.
    fn unseen(&self) -> Unseen {
        Unseen::Admin
    }

    fn launch_description(&self, game: &Game) -> String {
        let added = game.details::<Added>();
        format!("\"{}\" {}", added.exe.display(), added.args).trim_end().to_string()
    }

    fn start(&self, game: &Game) -> crate::Result<Started> {
        let added = game.details::<Added>();
        let elevated = crate::process::start(&added.exe, &added.args, game.install_dir())?;
        Ok(Started {
            folders: vec![game.install_dir().to_path_buf()],
            packages: Vec::new(),
            not_started: format!("{} didn't start", added.exe.display()),
            nudge: None,
            outside_steam: elevated,
            after: None,
        })
    }
}

/// Adds a game by its exe and returns its game key. An exe added before keeps its entry.
pub fn add(exe: &Path) -> crate::Result<String> {
    let id = id_for(exe);
    let mut all = list()?;
    if !all.iter().any(|a| a.id == id) {
        let name = name_for(exe);
        crate::log!("added {} as {name}", exe.display());
        all.push(Added { id: id.clone(), name, exe: exe.to_path_buf(), args: String::new() });
        save(&all)?;
    }
    Ok(format!("exe:{id}"))
}

/// Takes a game out of the list. Its Steam shortcut, if any, then counts as uninstalled.
pub fn forget(id: &str) -> crate::Result<()> {
    let mut all = list()?;
    all.retain(|a| a.id != id);
    save(&all)
}

fn list() -> crate::Result<Vec<Added>> {
    match std::fs::read(path()?) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e.into()),
    }
}

fn save(all: &[Added]) -> crate::Result<()> {
    let path = path()?;
    std::fs::create_dir_all(path.parent().expect("the list's path has a parent"))?;
    std::fs::write(path, serde_json::to_vec_pretty(all)?)?;
    Ok(())
}

fn path() -> crate::Result<PathBuf> {
    Ok(crate::data_dir()?.join("exes.json"))
}

/// A stable ID for the exe: FNV-1a of its path, ignoring case as Windows does.
fn id_for(exe: &Path) -> String {
    let hash = exe.to_string_lossy().to_lowercase().bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3));
    format!("{hash:016x}")
}

/// The game's name: the nearest folder above the exe with a telling name, else the exe's name.
fn name_for(exe: &Path) -> String {
    const GENERIC: [&str; 12] = ["bin", "bin64", "binaries", "win64", "win32", "x64", "x86", "game", "games", "program files", "program files (x86)", "windowsnoeditor"];
    exe.ancestors()
        .skip(1)
        .filter_map(|dir| dir.file_name()?.to_str())
        .find(|name| !GENERIC.contains(&name.to_lowercase().as_str()))
        .map(String::from)
        .or_else(|| exe.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "Game".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_ids() {
        assert_eq!(name_for(Path::new(r"C:\Games\Hollow Knight\hollow_knight.exe")), "Hollow Knight");
        assert_eq!(name_for(Path::new(r"D:\Doom\Bin\Win64\doom.exe")), "Doom");
        assert_eq!(name_for(Path::new(r"C:\Games\tool.exe")), "tool");
        assert_eq!(id_for(Path::new(r"C:\A\b.exe")), id_for(Path::new(r"c:\a\B.EXE")));
    }
}

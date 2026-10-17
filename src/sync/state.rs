//! What steambro has done, in `%LOCALAPPDATA%\steambro\state.json`: the shortcuts it added, what
//! happened to each artwork slot, and the artwork changes the user made in steambro.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::art::{Crop, Slot};

#[derive(Default, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub shortcuts: Vec<TrackedShortcut>,
    /// By game key. Applies to every account, and to games not added yet.
    #[serde(default)]
    pub art: BTreeMap<String, BTreeMap<Slot, ArtChoice>>,
    /// The game keys the tray app has seen installed, so that it tells about each new game once.
    /// `None` until its first look.
    #[serde(default)]
    pub seen: Option<BTreeSet<String>>,
}

#[derive(Serialize, Deserialize)]
pub struct TrackedShortcut {
    pub steam_user: u32,
    /// The game's key, e.g. `xbox:<AUMID>` or `battlenet:s2`.
    pub game: String,
    /// Steam's ID for the shortcut. It stays the same when the user renames it in Steam.
    pub app_id: u32,
    /// A slot without an entry hasn't been tried yet.
    #[serde(default)]
    pub art: BTreeMap<Slot, Placement>,
}

/// What happened to one artwork slot of a steambro shortcut. Only `Pending` is tried again.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Placement {
    /// steambro wrote the file. It changes again only when the user edits it in steambro.
    Written,
    /// Steam already had an image there; steambro leaves it alone.
    Kept,
    /// The image couldn't be fetched (for example, no connection). Tried again on the next run.
    Pending,
    /// The store has no image for this slot.
    Unavailable,
}

#[derive(Default, Clone, Serialize, Deserialize)]
pub struct ArtChoice {
    /// `None` means the centered default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crop: Option<Crop>,
    /// The cached source is the user's own image, not the store's.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub own: bool,
}

impl State {
    pub fn load() -> crate::Result<Self> {
        let path = Self::path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        serde_json::from_slice(&std::fs::read(&path)?).map_err(|e| format!("{} is damaged: {e}", path.display()).into())
    }

    /// Writes a new file and then replaces the old one, so a crash can't leave half a file.
    pub fn save(&self) -> crate::Result<()> {
        let path = Self::path()?;
        std::fs::create_dir_all(path.parent().expect("state path has a parent"))?;
        let temp = path.with_extension("json.new");
        std::fs::write(&temp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&temp, &path)?;
        Ok(())
    }

    pub fn find(&self, steam_user: u32, game: &str) -> Option<&TrackedShortcut> {
        self.shortcuts.iter().find(|s| s.steam_user == steam_user && s.game == game)
    }

    pub fn find_mut(&mut self, steam_user: u32, game: &str) -> Option<&mut TrackedShortcut> {
        self.shortcuts.iter_mut().find(|s| s.steam_user == steam_user && s.game == game)
    }

    pub fn choice(&self, game: &str, slot: Slot) -> ArtChoice {
        self.art.get(game).and_then(|slots| slots.get(&slot)).cloned().unwrap_or_default()
    }

    pub fn set_choice(&mut self, game: &str, slot: Slot, choice: ArtChoice) {
        let slots = self.art.entry(game.to_string()).or_default();
        if choice.crop.is_none() && !choice.own {
            slots.remove(&slot);
        } else {
            slots.insert(slot, choice);
        }
        if slots.is_empty() {
            self.art.remove(game);
        }
    }

    fn path() -> crate::Result<PathBuf> {
        Ok(crate::data_dir()?.join("state.json"))
    }
}

//! steambro's own copy of each game's source images, in `%LOCALAPPDATA%\steambro\art`. A slot is
//! fetched once; after that the copy is the source, so crops can be changed offline and a store
//! that changes its images doesn't change the user's library.

use std::path::PathBuf;

use super::Slot;

pub struct Cache {
    dir: PathBuf,
}

impl Cache {
    pub fn open() -> crate::Result<Self> {
        Ok(Self { dir: crate::data_dir()?.join("art") })
    }

    /// The source image for the slot, if steambro has one.
    pub fn get(&self, game: &str, slot: Slot) -> Option<Vec<u8>> {
        std::fs::read(self.path(game, slot)).ok()
    }

    pub fn put(&self, game: &str, slot: Slot, bytes: &[u8]) -> crate::Result<()> {
        let path = self.path(game, slot);
        std::fs::create_dir_all(path.parent().expect("cache paths have a parent"))?;
        Ok(std::fs::write(path, bytes)?)
    }

    pub fn remove(&self, game: &str, slot: Slot) -> crate::Result<()> {
        match std::fs::remove_file(self.path(game, slot)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }

    /// `<cache>\<game key with unsafe characters replaced>\<slot>`. The file has no extension;
    /// readers detect the format from the content.
    fn path(&self, game: &str, slot: Slot) -> PathBuf {
        let folder: String = game.chars().map(|c| if c.is_ascii_alphanumeric() || "._-".contains(c) { c } else { '_' }).collect();
        self.dir.join(folder).join(format!("{slot:?}").to_lowercase())
    }
}

//! steambro's own copy of each game's source images, in `%LOCALAPPDATA%\steambro\art`: one folder
//! per game with one image per slot, like `cover.jpg`. The copy is the source: crops can change
//! offline. Beside a store's image, `<slot>.from` names where it came from (`Origin::identity`), so
//! that steambro fetches it again only when the store moved it (`sync::refresh`).

use std::path::PathBuf;
use std::sync::Mutex;

use super::Slot;

pub struct Cache {
    dir: PathBuf,
    /// Held while a slot's files change, so that a slow download can't replace an image that came
    /// in meanwhile (`put_if_absent`).
    writing: Mutex<()>,
}

/// The extensions of the images in the cache. Copies from before steambro kept extensions have
/// none; readers detect the format from the content either way.
const EXTENSIONS: [&str; 6] = ["png", "jpg", "webp", "ico", "bmp", "img"];

impl Cache {
    pub fn open() -> crate::Result<Self> {
        Ok(Self { dir: crate::data_dir()?.join("art"), writing: Mutex::new(()) })
    }

    /// The source image for the slot, if steambro has one.
    pub fn get(&self, game: &str, slot: Slot) -> Option<Vec<u8>> {
        std::fs::read(self.file(game, slot)?).ok()
    }

    /// The file of the slot's source image, whatever its extension.
    pub fn file(&self, game: &str, slot: Slot) -> Option<PathBuf> {
        let base = self.base(game, slot);
        EXTENSIONS.iter().map(|ext| base.with_extension(ext)).chain([base.clone()]).find(|p| p.is_file())
    }

    /// Keeps `bytes` as the slot's source image, in place of any other. `from` names the store's
    /// origin of the image; `None` for the user's own image.
    pub fn put(&self, game: &str, slot: Slot, bytes: &[u8], from: Option<&str>) -> crate::Result<()> {
        let _writing = self.writing.lock().unwrap();
        self.write(game, slot, Some(bytes), from)
    }

    /// Keeps the store's image (`None`: the origin had none) with its origin `from`, unless the slot
    /// got an image meanwhile, like the user's own. Returns that image then.
    pub fn put_if_absent(&self, game: &str, slot: Slot, bytes: Option<&[u8]>, from: &str) -> crate::Result<Option<Vec<u8>>> {
        let _writing = self.writing.lock().unwrap();
        if let Some(present) = self.get(game, slot) {
            return Ok(Some(present));
        }
        self.write(game, slot, bytes, Some(from))?;
        Ok(None)
    }

    /// Where the slot's image came from (`put`), or the origin that had no image for it.
    pub fn from(&self, game: &str, slot: Slot) -> Option<String> {
        std::fs::read_to_string(self.base(game, slot).with_extension("from")).ok()
    }

    /// Records the store's origin for the slot without changing its image: the same image came
    /// again, or the origin has none.
    pub fn set_from(&self, game: &str, slot: Slot, from: &str) -> crate::Result<()> {
        let _writing = self.writing.lock().unwrap();
        self.write_from(game, slot, from)
    }

    /// Drops the slot's image and its origin.
    pub fn remove(&self, game: &str, slot: Slot) -> crate::Result<()> {
        let _writing = self.writing.lock().unwrap();
        self.delete(game, slot)
    }

    fn write(&self, game: &str, slot: Slot, bytes: Option<&[u8]>, from: Option<&str>) -> crate::Result<()> {
        self.delete(game, slot)?;
        let base = self.base(game, slot);
        std::fs::create_dir_all(base.parent().expect("cache paths have a parent"))?;
        if let Some(bytes) = bytes {
            let ext = match image::guess_format(bytes) {
                Ok(image::ImageFormat::Png) => "png",
                Ok(image::ImageFormat::Jpeg) => "jpg",
                Ok(image::ImageFormat::WebP) => "webp",
                Ok(image::ImageFormat::Ico) => "ico",
                Ok(image::ImageFormat::Bmp) => "bmp",
                _ => "img",
            };
            std::fs::write(base.with_extension(ext), bytes)?;
        }
        if let Some(from) = from {
            self.write_from(game, slot, from)?;
        }
        Ok(())
    }

    fn write_from(&self, game: &str, slot: Slot, from: &str) -> crate::Result<()> {
        let path = self.base(game, slot).with_extension("from");
        std::fs::create_dir_all(path.parent().expect("cache paths have a parent"))?;
        Ok(std::fs::write(path, from)?)
    }

    fn delete(&self, game: &str, slot: Slot) -> crate::Result<()> {
        let base = self.base(game, slot);
        for path in EXTENSIONS.iter().chain(&["from"]).map(|ext| base.with_extension(ext)).chain([base.clone()]) {
            match std::fs::remove_file(path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
                _ => {}
            }
        }
        Ok(())
    }

    /// The game's folder: `<cache>\<game key with unsafe characters replaced>`.
    pub fn folder(&self, game: &str) -> PathBuf {
        let folder: String = game.chars().map(|c| if c.is_ascii_alphanumeric() || "._-".contains(c) { c } else { '_' }).collect();
        self.dir.join(folder)
    }

    /// The slot's file without extension, like `<game folder>\cover`.
    fn base(&self, game: &str, slot: Slot) -> PathBuf {
        self.folder(game).join(format!("{slot:?}").to_lowercase())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn images_keep_their_extension_and_origin() {
        let dir = std::env::temp_dir().join("steambro-test-cache");
        let _ = std::fs::remove_dir_all(&dir);
        let cache = Cache { dir: dir.clone(), writing: Mutex::new(()) };
        let mut png = Vec::new();
        image::DynamicImage::new_rgb8(2, 2).write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        // A copy from before extensions is still read, and a new copy replaces it.
        let old = cache.folder("xbox:A!b").join("cover");
        std::fs::create_dir_all(old.parent().unwrap()).unwrap();
        std::fs::write(&old, &png).unwrap();
        assert_eq!(cache.get("xbox:A!b", Slot::Cover), Some(png.clone()));
        cache.put("xbox:A!b", Slot::Cover, &png, Some("https://store/a.png")).unwrap();
        assert_eq!(cache.file("xbox:A!b", Slot::Cover), Some(cache.folder("xbox:A!b").join("cover.png")));
        assert!(!old.exists());
        assert_eq!(cache.from("xbox:A!b", Slot::Cover).as_deref(), Some("https://store/a.png"));
        // The user's own image has no origin, and a download that ends later doesn't replace it.
        cache.put("xbox:A!b", Slot::Cover, &png, None).unwrap();
        assert_eq!(cache.from("xbox:A!b", Slot::Cover), None);
        assert_eq!(cache.put_if_absent("xbox:A!b", Slot::Cover, Some(b"store"), "https://store/b.png").unwrap(), Some(png.clone()));
        assert_eq!(cache.from("xbox:A!b", Slot::Cover), None);
        cache.remove("xbox:A!b", Slot::Cover).unwrap();
        assert_eq!(cache.get("xbox:A!b", Slot::Cover), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

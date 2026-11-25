//! Finds Battle.net games. Battle.net's install database (product.db) says what is installed and
//! where; the catalog the Battle.net app caches locally supplies display names, launch codes and
//! the official artwork it shows in its own UI.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::Value;

use crate::art::{Origin, Slot};

pub struct Installed {
    /// Battle.net.exe, which starts games with `--exec="launch <product id>"`.
    pub exe: PathBuf,
    pub games: Vec<InstalledGame>,
}

pub struct InstalledGame {
    /// Install ID from product.db, like `s2` or `wowf`.
    pub uid: String,
    pub name: String,
    /// Launch code, like `S2` or `WoWF`. Unknown when Battle.net hasn't cached its catalog.
    pub product_id: Option<String>,
    pub install_dir: PathBuf,
    /// Official artwork from Battle.net's catalog, by the Steam slot it fills.
    pub art: Art,
}

/// A game's artwork in Battle.net's catalog.
#[derive(Default)]
pub struct Art {
    pub slots: Vec<(Slot, Origin)>,
    /// The game's ID in the Microsoft Store, like `9PB7921QZ087`, when the Store sells it too.
    pub store_id: Option<String>,
    /// The wide cover is the install background, whose title is at the top, as Battle.net has no
    /// wide picture with the title for the game.
    pub wide_titled_top: bool,
}

/// Blizzard's server for the catalog's files, by their content hash: the same files as Battle.net's
/// cache (`Cache\ab\cd\<hash>`), for those it hasn't downloaded. It needs no sign-in.
const CATALOG_SERVER: &str = "https://us.cdn.blizzard.com/tpr/catalogs/data";

/// Returns `None` when Battle.net isn't installed.
pub fn find() -> crate::Result<Option<Installed>> {
    let Some(program_data) = std::env::var_os("ProgramData") else { return Ok(None) };
    let db_path = Path::new(&program_data).join(r"Battle.net\Agent\product.db");
    if !db_path.is_file() {
        return Ok(None);
    }
    let installs = read_product_db(&std::fs::read(&db_path)?)?;
    // Without the cache, the games still show, with their folder names and without launch codes.
    let catalog = std::env::var_os("LOCALAPPDATA")
        .and_then(|dir| Catalog::load(&Path::new(&dir).join(r"Battle.net\Cache")));

    let mut exe = None;
    let mut games = Vec::new();
    for install in installs {
        match install.uid.as_str() {
            "agent" => {}
            "battle.net" => exe = Some(install.dir.join("Battle.net.exe")),
            uid => {
                let product = catalog.as_ref().and_then(|c| c.product_for(uid));
                let fallback_name = install.dir.file_name().map(|n| n.to_string_lossy().into_owned());
                let (name, product_id, art) = match product {
                    Some(p) => (p.name.or(fallback_name), p.product_id, p.art),
                    None => (fallback_name, None, Art::default()),
                };
                games.push(InstalledGame {
                    uid: uid.to_string(),
                    name: name.unwrap_or_else(|| uid.to_string()),
                    product_id,
                    install_dir: install.dir,
                    art,
                });
            }
        }
    }
    let Some(exe) = exe.filter(|e| e.is_file()) else { return Ok(None) };
    Ok(Some(Installed { exe, games }))
}

struct Install {
    uid: String,
    dir: PathBuf,
}

/// product.db is a protobuf message. steambro reads only what it needs: repeated field 1 holds
/// installs as `{ 1: uid, 3: settings { 1: install path, 13: flavor subfolder } }`.
fn read_product_db(bytes: &[u8]) -> crate::Result<Vec<Install>> {
    let mut installs = Vec::new();
    for (field, value) in proto::fields(bytes)? {
        let (1, proto::Value::Bytes(install)) = (field, value) else { continue };
        let mut uid = String::new();
        let mut dir = PathBuf::new();
        for (field, value) in proto::fields(install)? {
            match (field, value) {
                (1, proto::Value::Bytes(b)) => uid = String::from_utf8_lossy(b).into_owned(),
                (3, proto::Value::Bytes(settings)) => dir = install_dir(settings)?,
                _ => {}
            }
        }
        if !uid.is_empty() {
            installs.push(Install { uid, dir });
        }
    }
    Ok(installs)
}

fn install_dir(settings: &[u8]) -> crate::Result<PathBuf> {
    let mut dir = PathBuf::new();
    let mut flavor = String::new();
    for (field, value) in proto::fields(settings)? {
        match (field, value) {
            (1, proto::Value::Bytes(b)) => dir = PathBuf::from(String::from_utf8_lossy(b).replace('/', "\\")),
            (13, proto::Value::Bytes(b)) => flavor = String::from_utf8_lossy(b).into_owned(),
            _ => {}
        }
    }
    // WoW installs each flavor (retail, classic, beta...) into its own subfolder like `_classic_beta_`.
    if !flavor.is_empty() && dir.join(&flavor).is_dir() {
        dir.push(flavor);
    }
    Ok(dir)
}

/// The part of the protobuf wire format that product.db needs.
mod proto {
    pub enum Value<'a> {
        Bytes(&'a [u8]),
        Other,
    }

    pub fn fields(mut buf: &[u8]) -> crate::Result<Vec<(u64, Value<'_>)>> {
        let mut fields = Vec::new();
        while !buf.is_empty() {
            let key = varint(&mut buf)?;
            let value = match key & 7 {
                0 => {
                    varint(&mut buf)?;
                    Value::Other
                }
                1 => {
                    take(&mut buf, 8)?;
                    Value::Other
                }
                2 => {
                    let len = usize::try_from(varint(&mut buf)?)?;
                    Value::Bytes(take(&mut buf, len)?)
                }
                5 => {
                    take(&mut buf, 4)?;
                    Value::Other
                }
                other => return Err(format!("unsupported protobuf wire type {other}").into()),
            };
            fields.push((key >> 3, value));
        }
        Ok(fields)
    }

    fn varint(buf: &mut &[u8]) -> crate::Result<u64> {
        let mut value = 0u64;
        for shift in (0..64).step_by(7) {
            let byte = take(buf, 1)?[0];
            value |= u64::from(byte & 0x7f) << shift;
            if byte < 0x80 {
                return Ok(value);
            }
        }
        Err("varint too long".into())
    }

    fn take<'a>(buf: &mut &'a [u8], n: usize) -> crate::Result<&'a [u8]> {
        if buf.len() < n {
            return Err("product.db ends unexpectedly".into());
        }
        let (head, rest) = buf.split_at(n);
        *buf = rest;
        Ok(head)
    }
}

/// Battle.net's catalog as its app caches it: a main index whose `fragments` point to one JSON
/// file per game family. Every cached file is stored under its hash, as `Cache\ab\cd\abcd...`.
struct Catalog {
    cache: PathBuf,
    /// By fragment name, in the main index's order; the main index itself is `default`.
    fragments: Vec<(String, Value)>,
}

struct Product {
    name: Option<String>,
    product_id: Option<String>,
    art: Art,
}

/// Stands in for missing JSON objects so lookups can chain.
static NULL: Value = Value::Null;

impl Catalog {
    fn load(cache: &Path) -> Option<Self> {
        let main = find_main_index(cache)?;
        let mut fragments = Vec::new();
        for fragment in main["fragments"].as_array()? {
            // Entries with requirements are regional variants (like China); the base one is enough.
            if fragment.get("requires").is_some() {
                continue;
            }
            let (Some(name), Some(hash)) = (fragment["name"].as_str(), fragment["hash"].as_str()) else { continue };
            if let Some(json) = read_json(&cached_file(cache, hash)) {
                fragments.push((name.to_string(), json));
            }
        }
        fragments.push(("default".to_string(), main));
        Some(Self { cache: cache.to_path_buf(), fragments })
    }

    fn fragment(&self, name: &str) -> Option<&Value> {
        self.fragments.iter().find(|(n, _)| n == name).map(|(_, json)| json)
    }

    /// Products list their install flavors under `base.types`, each with the install's uid. A
    /// product's main install can carry the uid on the product itself instead, as retail WoW's
    /// `wow` does, with an empty `retail` flavor.
    fn product_for(&self, uid: &str) -> Option<Product> {
        for (fragment, product) in self.products() {
            for flavor in product["base"]["types"].as_object().into_iter().flat_map(|t| t.values()) {
                if flavor["uid"].as_str() == Some(uid) {
                    return Some(self.describe(fragment, product, flavor));
                }
            }
        }
        let (fragment, product) = self.products().find(|(_, p)| p["base"]["uid"].as_str() == Some(uid))?;
        Some(self.describe(fragment, product, &product["base"]["types"]["retail"]))
    }

    /// The game of `flavor` of `product`, which is in the catalog fragment named `fragment`.
    fn describe(&self, fragment: &str, product: &Value, flavor: &Value) -> Product {
        // A flavor can present itself as a product of its own: WoW's `wowf` is "WoW Forever".
        let product_id = flavor["override_product_id"].as_str().or(product["id"].as_str());
        let alias = product_id.and_then(|id| self.products().find(|(_, p)| p["id"].as_str() == Some(id)));
        let alias_base = alias.map_or(&NULL, |(_, a)| &a["base"]);
        let field = |key: &str| [flavor, alias_base, &product["base"]].into_iter().find_map(|v| v[key].as_str());
        let file = |key: &str| field(key).and_then(|k| self.file(k));

        // Battle.net shows a game's own icon from its presence resources, found by the game's
        // program ID. The `icon_medium` fields of a flavor's product are its family's: WoW: Forever
        // would get retail WoW's icon. Only an icon in the game's own fragment counts: for games
        // that share a program, like Call of Duty's, or that have none of their own, like Diablo
        // IV, the presence icon is Battle.net's generic one from the main index.
        let program = flavor["override_product_id"].as_str().or(product["base"]["program_id"].as_str()).or(product["id"].as_str());
        let own = |key: &&str| key.split('#').next() == Some(fragment) && fragment != "default";
        let presence = program.and_then(|p| self.presence(p));
        let icon = presence
            .and_then(|r| ["icon_275", "icon_56"].into_iter().find_map(|k| r[k].as_str().filter(own).and_then(|key| self.file(key))))
            .or_else(|| file("icon_medium"));
        let (slots, wide_titled_top) = slots(file, icon);
        let store_id = flavor["xbox_uid"].as_str().filter(|id| !id.is_empty()).map(String::from);
        Product { name: field("name").and_then(|key| self.string(key)), product_id: product_id.map(String::from), art: Art { slots, store_id, wide_titled_top } }
    }

    /// The presence resources (status icons) of the program with this ID, like `WoWF`.
    fn presence(&self, program: &str) -> Option<&Value> {
        self.fragments.iter().flat_map(|(_, f)| f["presence_resources"].as_array().into_iter().flatten()).find(|r| r["program_id"].as_str() == Some(program))
    }

    /// Every product, with the name of the fragment it is in.
    fn products(&self) -> impl Iterator<Item = (&str, &Value)> {
        self.fragments.iter().flat_map(|(name, f)| f["products"].as_array().into_iter().flatten().map(move |p| (name.as_str(), p)))
    }

    /// Resolves a `fragment#KEY` reference to a localized string.
    fn string(&self, key: &str) -> Option<String> {
        let fragment = self.fragment(key.split('#').next()?)?;
        fragment["strings"]["default"][key].as_str().map(String::from)
    }

    /// Resolves a `fragment#KEY` reference to an image: Battle.net's cached copy, or else the same
    /// file on Blizzard's catalog server, by its content hash.
    fn file(&self, key: &str) -> Option<Origin> {
        let fragment = self.fragment(key.split('#').next()?)?;
        let hash = fragment["files"]["default"][key]["hash"].as_str().filter(|h| h.len() > 4 && h.chars().all(|c| c.is_ascii_hexdigit()))?;
        let path = cached_file(&self.cache, hash);
        Some(if path.is_file() { Origin::File(path) } else { Origin::Url(format!("{CATALOG_SERVER}/{}/{}/{hash}", &hash[..2], &hash[2..4])) })
    }
}

/// Picks the catalog image for each Steam slot, and says whether the wide cover is an image with
/// the title at its top. `file` resolves a catalog field to an image.
fn slots(file: impl Fn(&str) -> Option<Origin>, icon: Option<Origin>) -> (Vec<(Slot, Origin)>, bool) {
    let cover = file("key_art");
    let background = file("install_background");
    // Battle.net dims its `background` images to sit behind its own UI, so they make a poor
    // header. `key_art` is the widest painted image for most games (600 pixels against 544), and
    // the choice is fixed, so that it doesn't change with what Battle.net has downloaded. The
    // user can move the crop.
    let header = cover.clone().or_else(|| background.clone());
    // The play-selector thumbnail is a wide image with the title on it, like Steam's wide cover;
    // else the install background, which has the title at its top.
    let thumbnail = file("play_selector_thumbnail");
    let wide_titled_top = thumbnail.is_none() && background.is_some();
    let wide = thumbnail.or(background).or_else(|| cover.clone());
    let slots = [(Slot::Cover, cover), (Slot::Hero, header), (Slot::Logo, file("logo")), (Slot::Wide, wide), (Slot::Icon, icon)]
        .into_iter()
        .filter_map(|(slot, origin)| Some((slot, origin?)))
        .collect();
    (slots, wide_titled_top)
}

fn cached_file(cache: &Path, hash: &str) -> PathBuf {
    cache.join(hash.get(..2).unwrap_or_default()).join(hash.get(2..4).unwrap_or_default()).join(hash)
}

/// The cache keeps every old copy of the main index, all with the same `version` (it's the format
/// version), so the most recently written file is the current one.
fn find_main_index(cache: &Path) -> Option<Value> {
    let mut newest: Option<(SystemTime, Value)> = None;
    for outer in std::fs::read_dir(cache).ok()?.flatten() {
        for inner in std::fs::read_dir(outer.path()).into_iter().flatten().flatten() {
            for file in std::fs::read_dir(inner.path()).into_iter().flatten().flatten() {
                let Ok(modified) = file.metadata().and_then(|m| m.modified()) else { continue };
                if newest.as_ref().is_some_and(|(time, _)| modified <= *time) {
                    continue;
                }
                let Some(json) = read_json(&file.path()) else { continue };
                if json["fragment_id"].as_str() == Some("default") && json["fragments"].is_array() {
                    newest = Some((modified, json));
                }
            }
        }
    }
    newest.map(|(_, json)| json)
}

/// Most cached files are images; only peek at the first byte before parsing.
fn read_json(path: &Path) -> Option<Value> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut first = [0u8; 1];
    file.read_exact(&mut first).ok()?;
    if first[0] != b'{' {
        return None;
    }
    let mut text = String::from("{");
    file.read_to_string(&mut text).ok()?;
    serde_json::from_str(&text).ok()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// A catalog like Battle.net's, in short: WoW with its WoW: Forever flavor, Diablo IV, and the
    /// main index with Battle.net's generic icon. No file is in the cache, so images are links.
    fn catalog() -> Catalog {
        let wow = json!({
            "products": [
                { "id": "WoW", "base": {
                    "uid": "wow", "name": "world_of_warcraft#WOW_NAME", "icon_medium": "world_of_warcraft#WOW_ICON", "key_art": "world_of_warcraft#WOW_KEY",
                    "types": { "retail": {}, "wowf": { "uid": "wowf", "override_product_id": "WoWF" } } } },
                { "id": "WoWF", "base": { "name": "world_of_warcraft#WOWF_NAME" } }
            ],
            "presence_resources": [{ "program_id": "WoWF", "icon_275": "world_of_warcraft#WOWF_ICON" }],
            "strings": { "default": { "world_of_warcraft#WOW_NAME": "World of Warcraft", "world_of_warcraft#WOWF_NAME": "WoW: Forever" } },
            "files": { "default": {
                "world_of_warcraft#WOW_ICON": { "hash": "aaaa0000" }, "world_of_warcraft#WOWF_ICON": { "hash": "bbbb0000" },
                "world_of_warcraft#WOW_KEY": { "hash": "cccc0000" } } }
        });
        let fenris = json!({
            "products": [{ "id": "Fen", "base": { "icon_medium": "fenris#FEN_ICON", "types": { "retail": { "uid": "fenris", "xbox_uid": "9N0H62KZ3BXV" } } } }],
            "files": { "default": { "fenris#FEN_ICON": { "hash": "dddd0000" } } }
        });
        let main = json!({
            "presence_resources": [{ "program_id": "Fen", "icon_275": "default#APP_ICON" }],
            "files": { "default": { "default#APP_ICON": { "hash": "eeee0000" } } }
        });
        let cache = std::env::temp_dir().join("steambro-test-no-battle-net-cache");
        Catalog { cache, fragments: vec![("world_of_warcraft".into(), wow), ("fenris".into(), fenris), ("default".into(), main)] }
    }

    fn icon(product: &Product) -> String {
        product.art.slots.iter().find(|(slot, _)| *slot == Slot::Icon).map(|(_, origin)| origin.identity()).unwrap_or_default()
    }

    #[test]
    fn each_game_gets_its_own_icon_and_retail_wow_is_found() {
        let catalog = catalog();
        let forever = catalog.product_for("wowf").unwrap();
        assert_eq!((forever.name.as_deref(), forever.product_id.as_deref()), (Some("WoW: Forever"), Some("WoWF")));
        assert_eq!(icon(&forever), format!("{CATALOG_SERVER}/bb/bb/bbbb0000"), "its own presence icon, not retail WoW's");
        let retail = catalog.product_for("wow").unwrap();
        assert_eq!((retail.name.as_deref(), retail.product_id.as_deref()), (Some("World of Warcraft"), Some("WoW")));
        assert!(icon(&retail).ends_with("aaaa0000"));
        let diablo = catalog.product_for("fenris").unwrap();
        assert!(icon(&diablo).ends_with("dddd0000"), "its own icon, not Battle.net's generic one");
        assert_eq!(diablo.art.store_id.as_deref(), Some("9N0H62KZ3BXV"));
        assert!(catalog.product_for("none").is_none());
    }
}

//! Finds Battle.net games. Battle.net's install database (product.db) says what is installed and
//! where; the catalog the Battle.net app caches locally supplies display names, launch codes and
//! the official artwork it shows in its own UI.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::Value;

use crate::art::Slot;

pub struct Installed {
    /// Battle.net.exe, which starts games with `--exec="launch <product id>"`.
    pub exe: PathBuf,
    pub games: Vec<InstalledGame>,
}

pub struct InstalledGame {
    /// Install ID from product.db, e.g. `s2` or `wow_classic_beta`.
    pub uid: String,
    pub name: String,
    /// Launch code, e.g. `S2` or `WoWF`. Unknown when Battle.net hasn't cached its catalog.
    pub product_id: Option<String>,
    pub install_dir: PathBuf,
    /// Official artwork found in Battle.net's cache, by the Steam slot it fills.
    pub art: Vec<(Slot, PathBuf)>,
}

/// Returns `None` when Battle.net isn't installed.
pub fn find() -> crate::Result<Option<Installed>> {
    let Some(program_data) = std::env::var_os("ProgramData") else { return Ok(None) };
    let db_path = Path::new(&program_data).join(r"Battle.net\Agent\product.db");
    if !db_path.is_file() {
        return Ok(None);
    }
    let installs = read_product_db(&std::fs::read(&db_path)?)?;
    // Without the cache we still know what's installed, just not the nice names or launch codes.
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
                    None => (fallback_name, None, Vec::new()),
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

/// Just enough of the protobuf wire format to walk product.db.
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
    art: Vec<(Slot, PathBuf)>,
}

/// Stands in for missing JSON objects so lookups can chain.
static NULL: Value = Value::Null;

impl Catalog {
    fn load(cache: &Path) -> Option<Self> {
        let main = find_main_index(cache)?;
        let mut fragments = Vec::new();
        for fragment in main["fragments"].as_array()? {
            // Entries with requirements are regional variants (e.g. China); the base one is enough.
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

    /// Products list their install flavors under `base.types`, each with the install's uid.
    fn product_for(&self, uid: &str) -> Option<Product> {
        for product in self.products() {
            for flavor in product["base"]["types"].as_object().into_iter().flat_map(|t| t.values()) {
                if flavor["uid"].as_str() == Some(uid) {
                    return Some(self.describe(product, flavor));
                }
            }
        }
        None
    }

    fn describe(&self, product: &Value, flavor: &Value) -> Product {
        // A flavor can present itself as a product of its own: WoW's `wow_classic_beta` is "WoW Forever".
        let product_id = flavor["override_product_id"].as_str().or(product["id"].as_str());
        let alias = product_id.and_then(|id| self.products().find(|p| p["id"].as_str() == Some(id)));
        let alias_base = alias.map_or(&NULL, |a| &a["base"]);
        let field = |key: &str| [flavor, alias_base, &product["base"]].into_iter().find_map(|v| v[key].as_str());

        let file = |key: &str| field(key).and_then(|k| self.file(k));
        Product { name: field("name").and_then(|key| self.string(key)), product_id: product_id.map(String::from), art: slots(file) }
    }

    fn products(&self) -> impl Iterator<Item = &Value> {
        self.fragments.iter().flat_map(|(_, f)| f["products"].as_array().into_iter().flatten())
    }

    /// Resolves a `fragment#KEY` reference to a localized string.
    fn string(&self, key: &str) -> Option<String> {
        let fragment = self.fragment(key.split('#').next()?)?;
        fragment["strings"]["default"][key].as_str().map(String::from)
    }

    /// Resolves a `fragment#KEY` reference to a cached file, if Battle.net has downloaded it.
    fn file(&self, key: &str) -> Option<PathBuf> {
        let fragment = self.fragment(key.split('#').next()?)?;
        let path = cached_file(&self.cache, fragment["files"]["default"][key]["hash"].as_str()?);
        path.is_file().then_some(path)
    }
}

/// Picks the catalog image for each Steam slot. `file` resolves a catalog field to a cached file.
fn slots(file: impl Fn(&str) -> Option<PathBuf>) -> Vec<(Slot, PathBuf)> {
    let cover = file("key_art");
    // Battle.net dims its `background` images to sit behind its own UI, so they make a poor
    // header. The biggest of the painted images is used instead; the user can move the crop.
    let header = ["install_background", "key_art"].into_iter().filter_map(&file).max_by_key(|p| pixel_count(p));
    // The play-selector thumbnail is a wide image with the title on it, like Steam's wide cover.
    let wide = file("play_selector_thumbnail").or_else(|| header.clone());
    [
        (Slot::Cover, cover),
        (Slot::Hero, header),
        (Slot::Logo, file("logo")),
        (Slot::Wide, wide),
        (Slot::Icon, file("icon_medium")),
    ]
    .into_iter()
    .filter_map(|(slot, path)| Some((slot, path?)))
    .collect()
}

fn pixel_count(path: &Path) -> u64 {
    let size = image::ImageReader::open(path).and_then(|r| r.with_guessed_format()).ok().and_then(|r| r.into_dimensions().ok());
    size.map_or(0, |(w, h)| u64::from(w) * u64::from(h))
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

//! Finds Epic games. The Epic Games launcher keeps one manifest per installed app in
//! `%ProgramData%\Epic\EpicGamesLauncher\Data\Manifests` (`*.item`, JSON), and a copy of its store
//! catalog in `Data\Catalog\catcache.bin` (base64 of JSON), with the artwork it shows.

use std::path::{Path, PathBuf};

use base64::Engine;
use serde_json::Value;

use crate::art::Slot;

pub struct InstalledGame {
    /// Epic's app name, like `Fortnite`, or a hex ID for most games. Stable across reinstalls.
    pub app_name: String,
    pub name: String,
    pub install_dir: PathBuf,
    /// The catalog namespace and item ID, which the launch URI names with the app name.
    pub namespace: String,
    pub item_id: String,
    /// Official artwork URLs from the catalog cache, by the Steam slot they fill.
    pub art: Vec<(Slot, String)>,
}

/// The installed games; empty when the Epic Games launcher isn't installed.
pub fn find() -> crate::Result<Vec<InstalledGame>> {
    let Some(data) = data_dir() else { return Ok(Vec::new()) };
    let Ok(entries) = std::fs::read_dir(data.join("Manifests")) else { return Ok(Vec::new()) };
    let catalog = read_catalog(&data.join(r"Catalog\catcache.bin"));
    let mut games = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.extension().is_some_and(|e| e.eq_ignore_ascii_case("item")) {
            continue;
        }
        let Some(manifest) = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()) else { continue };
        if let Some(mut game) = game_from(&manifest)
            && game.install_dir.is_dir()
        {
            if let Some(item) = catalog.iter().find(|i| str_of(i, "id") == Some(&game.item_id) && str_of(i, "namespace") == Some(&game.namespace)) {
                game.art = art(item);
            }
            games.push(game);
        }
    }
    Ok(games)
}

/// `%ProgramData%\Epic\EpicGamesLauncher\Data`, when the launcher is installed.
pub fn data_dir() -> Option<PathBuf> {
    let program_data = std::env::var_os("ProgramData")?;
    let dir = Path::new(&program_data).join(r"Epic\EpicGamesLauncher\Data");
    dir.is_dir().then_some(dir)
}

/// A game from its manifest. Unfinished installs, add-ons (they name a main game or a host
/// install), and apps that aren't games are left out.
fn game_from(manifest: &Value) -> Option<InstalledGame> {
    let flag = |key: &str| manifest.get(key).and_then(Value::as_bool).unwrap_or(false);
    let text = |key: &str| str_of(manifest, key).filter(|s| !s.is_empty());
    if flag("bIsIncompleteInstall") || text("MainGameAppName").is_some() || text("HostInstallationGuid").is_some() {
        return None;
    }
    let categories = manifest.get("AppCategories").and_then(Value::as_array);
    if !categories.is_some_and(|c| c.iter().any(|c| c.as_str() == Some("games"))) || text("LaunchExecutable").is_none() {
        return None;
    }
    let app_name = text("AppName")?.to_string();
    Some(InstalledGame {
        name: text("DisplayName").unwrap_or(&app_name).to_string(),
        install_dir: PathBuf::from(text("InstallLocation")?),
        namespace: text("CatalogNamespace")?.to_string(),
        item_id: text("CatalogItemId")?.to_string(),
        art: Vec::new(),
        app_name,
    })
}

/// The catalog items in the launcher's cache; empty when it can't be read.
fn read_catalog(path: &Path) -> Vec<Value> {
    let Ok(text) = std::fs::read(path) else { return Vec::new() };
    let Ok(json) = base64::engine::general_purpose::STANDARD.decode(text.trim_ascii()) else { return Vec::new() };
    match serde_json::from_slice(&json) {
        Ok(Value::Array(items)) => items,
        _ => Vec::new(),
    }
}

/// The catalog's `keyImages` for each slot, in order of preference.
fn art(item: &Value) -> Vec<(Slot, String)> {
    let images = item.get("keyImages").and_then(Value::as_array).cloned().unwrap_or_default();
    let url = |types: &[&str]| {
        types.iter().find_map(|wanted| {
            images.iter().find(|i| str_of(i, "type") == Some(wanted)).and_then(|i| str_of(i, "url")).map(String::from)
        })
    };
    let wide = ["DieselGameBox", "OfferImageWide", "DieselStoreFrontWide"];
    [
        (Slot::Cover, url(&["DieselGameBoxTall", "OfferImageTall", "DieselStoreFrontTall"])),
        (Slot::Hero, url(&wide)),
        (Slot::Logo, url(&["DieselGameBoxLogo"])),
        (Slot::Wide, url(&wide)),
        (Slot::Icon, url(&["AndroidIcon"])),
    ]
    .into_iter()
    .filter_map(|(slot, url)| Some((slot, url?)))
    .collect()
}

fn str_of<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_of_a_game() {
        let manifest: Value = serde_json::from_str(
            r#"{"bIsIncompleteInstall": false, "LaunchExecutable": "FortniteGame/Binaries/Win64/FortniteBootstrapper.exe",
                "DisplayName": "Fortnite", "InstallLocation": "C:\\Program Files\\Epic Games\\Fortnite",
                "AppCategories": ["games", "applications"], "CatalogNamespace": "fn",
                "CatalogItemId": "4fe75bbc5a674f4f9b356b5c90567da5", "AppName": "Fortnite",
                "MainGameAppName": "", "HostInstallationGuid": ""}"#,
        )
        .unwrap();
        let game = game_from(&manifest).unwrap();
        assert_eq!((game.app_name.as_str(), game.name.as_str(), game.namespace.as_str()), ("Fortnite", "Fortnite", "fn"));

        let mut addon = manifest.clone();
        addon["MainGameAppName"] = "Fortnite".into();
        assert!(game_from(&addon).is_none());
        let mut unfinished = manifest.clone();
        unfinished["bIsIncompleteInstall"] = true.into();
        assert!(game_from(&unfinished).is_none());
        let mut tool = manifest;
        tool["AppCategories"] = serde_json::json!(["applications"]);
        assert!(game_from(&tool).is_none());
    }

    #[test]
    fn catalog_images_fill_slots() {
        let item = serde_json::json!({"keyImages": [
            {"type": "DieselGameBox", "url": "https://x/wide.jpg"},
            {"type": "DieselGameBoxTall", "url": "https://x/tall.jpg"},
            {"type": "Featured", "url": "https://x/featured.jpg"}
        ]});
        let slots = art(&item);
        assert!(slots.contains(&(Slot::Cover, "https://x/tall.jpg".into())));
        assert!(slots.contains(&(Slot::Hero, "https://x/wide.jpg".into())));
        assert!(slots.contains(&(Slot::Wide, "https://x/wide.jpg".into())));
        assert_eq!(slots.len(), 3);
    }
}

//! Apps the user added to the Xbox app's library ("Add to library"). The Xbox app keeps them in
//! its `CustomLibraryManagement.manifest`; steambro offers the Windows apps among them, like
//! Netflix or Apple TV.

use std::path::Path;

use serde_json::Value;

pub struct AddedApp {
    /// `<package family>!<app id>`.
    pub aumid: String,
    /// The name the Xbox app shows.
    pub title: String,
}

/// The Windows apps in the Xbox app's library; empty without the Xbox app or the file.
pub fn list() -> Vec<AddedApp> {
    let Some(local) = std::env::var_os("LOCALAPPDATA") else { return Vec::new() };
    let path = Path::new(&local).join(
        r"Packages\Microsoft.GamingApp_8wekyb3d8bbwe\LocalState\ThirdPartyLibraries\CustomLibraryManagement\CustomLibraryManagement.manifest",
    );
    std::fs::read(path).map(|bytes| parse(&bytes)).unwrap_or_default()
}

/// Entries under `gameCache`. A Windows app's `id` is its AUMID and it has no folder; a program
/// has a folder and an exe, and is left out: Steam can start a program itself, and one inside a
/// game's folder (like FortniteLauncher.exe) is that game again.
fn parse(bytes: &[u8]) -> Vec<AddedApp> {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let Ok(json) = serde_json::from_slice::<Value>(bytes) else { return Vec::new() };
    let Some(entries) = json.get("gameCache").and_then(Value::as_object) else { return Vec::new() };
    entries
        .values()
        .filter_map(|entry| {
            let id = entry.get("id")?.as_str()?;
            let folder = entry.get("installLocation").and_then(Value::as_str).unwrap_or_default();
            if !folder.is_empty() || !id.contains('!') {
                return None;
            }
            let title = entry.get("title").and_then(Value::as_str).unwrap_or(id);
            Some(AddedApp { aumid: id.to_string(), title: title.to_string() })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_apps_only() {
        let manifest = br#"{"version":1,"provider":{"version":1,"enabled":true},"gameCache":{"version":1,
            "4DF9E0F8.Netflix_mcm4njqhnhss8!Netflix.App":{"id":"4DF9E0F8.Netflix_mcm4njqhnhss8!Netflix.App","title":"Netflix","installLocation":"","executableName":"4DF9E0F8.Netflix_mcm4njqhnhss8!Netflix.App"},
            "ee33b139":{"id":"ee33b139","title":"FortniteLauncher","installLocation":"C:\\Program Files\\Epic Games\\Fortnite\\FortniteGame\\Binaries\\Win64\\","executableName":"FortniteLauncher.exe"}}}"#;
        let apps = parse(manifest);
        assert_eq!(apps.len(), 1);
        assert_eq!((apps[0].aumid.as_str(), apps[0].title.as_str()), ("4DF9E0F8.Netflix_mcm4njqhnhss8!Netflix.App", "Netflix"));
        assert!(parse(b"not json").is_empty());
    }
}

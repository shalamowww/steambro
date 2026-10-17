//! GOG: games that GOG Galaxy or a GOG installer recorded in the registry, artwork from GOG's
//! public catalog, started directly. GOG games have no DRM, so `steambro launch` starts the game's
//! exe as its own child, and Steam sees it like a game it started itself.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::{Game, Started, Store};
use crate::art::{Origin, Slot};

pub struct Gog;

/// One subkey for each installed game or DLC, named by its GOG product ID.
const GAMES_KEY: &str = r"SOFTWARE\WOW6432Node\GOG.com\Games";

/// How the game starts: its primary play task. The game's ID is its GOG product ID.
struct Task {
    exe: PathBuf,
    args: String,
    dir: PathBuf,
}

impl Store for Gog {
    fn id(&self) -> &'static str {
        "gog"
    }

    fn name(&self) -> &'static str {
        "GOG"
    }

    fn art_source(&self) -> &'static str {
        "GOG"
    }

    fn detect(&'static self, warnings: &mut Vec<String>) -> crate::Result<Vec<Game>> {
        let Ok(games) = windows_registry::LOCAL_MACHINE.open(GAMES_KEY) else { return Ok(Vec::new()) };
        let mut found = Vec::new();
        for id in games.keys()? {
            let Ok(key) = games.open(&id) else { continue };
            let value = |name: &str| key.get_string(name).unwrap_or_default();
            let dir = PathBuf::from(value("path"));
            // A DLC names its game in `dependsOn`, and in its info file as the root game.
            let info = read_info(&dir, &id);
            if !value("dependsOn").is_empty() || info.as_ref().is_some_and(Info::is_dlc) || !dir.is_dir() {
                continue;
            }
            let name = Some(value("gameName")).filter(|n| !n.is_empty()).or_else(|| info.as_ref().map(|i| i.name.clone())).unwrap_or_else(|| id.clone());
            let task = info.as_ref().and_then(|i| i.primary_task(&dir)).or_else(|| {
                let exe = PathBuf::from(value("exe"));
                let dir = Some(PathBuf::from(value("workingDir"))).filter(|d| d.is_dir()).or_else(|| exe.parent().map(Path::to_path_buf))?;
                exe.is_file().then(|| Task { exe, args: value("launchParam"), dir })
            });
            match task {
                Some(task) => found.push(Game::new(self, id, name, dir, task)),
                None => warnings.push(format!("GOG's records for {name} name no exe to start")),
            }
        }
        Ok(found)
    }

    /// GOG's catalog for the game; the icon comes from the game's folder.
    fn art_origins(&self, game: &Game) -> crate::Result<Vec<(Slot, Origin)>> {
        let body = crate::art::http::get(&format!("https://api.gog.com/v2/games/{}", game.id()))?;
        let product: serde_json::Value = serde_json::from_slice(&body)?;
        let link = |path: &str| product.pointer(path).and_then(|v| v.as_str()).map(String::from);
        let mut origins = Vec::new();
        let mut add = |slot, url: Option<String>| origins.extend(url.map(|url| (slot, Origin::Url(url))));
        add(Slot::Cover, link("/_links/boxArtImage/href"));
        add(Slot::Hero, link("/_links/galaxyBackgroundImage/href").or_else(|| link("/_links/backgroundImage/href")));
        add(Slot::Logo, link("/_links/logo/href"));
        // The product card comes in sizes; 1600 pixels wide is close to the wide cover's shape.
        add(Slot::Wide, link("/_embedded/product/_links/image/href").map(|url| url.replace("{formatter}", "1600")));
        let icon = game.install_dir().join(format!("goggame-{}.ico", game.id()));
        if icon.is_file() {
            origins.push((Slot::Icon, Origin::File(icon)));
        } else {
            add(Slot::Icon, link("/_links/icon/href"));
        }
        Ok(origins)
    }

    fn launch_description(&self, game: &Game) -> String {
        let task = game.details::<Task>();
        format!("\"{}\" {}", task.exe.display(), task.args).trim_end().to_string()
    }

    /// Starts the exe directly. A game that Windows runs as administrator, as GOG sets up some old
    /// games, starts through Windows' prompt instead, outside Steam's view.
    fn start(&self, game: &Game) -> crate::Result<Started> {
        let task = game.details::<Task>();
        let elevated = crate::process::start(&task.exe, &task.args, &task.dir)?;
        Ok(Started {
            folders: vec![game.install_dir().to_path_buf()],
            packages: Vec::new(),
            not_started: format!("{} didn't start. Try starting it from its folder.", task.exe.display()),
            nudge: None,
            start_timeout: None,
            outside_steam: elevated,
            after: None,
        })
    }

    fn details(&self, game: &Game) -> Vec<(String, String)> {
        let task = game.details::<Task>();
        vec![("exe".into(), task.exe.display().to_string()), ("working folder".into(), task.dir.display().to_string())]
    }
}

/// `goggame-<id>.info` in the game's folder.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Info {
    game_id: String,
    root_game_id: Option<String>,
    #[serde(default)]
    name: String,
    #[serde(default)]
    play_tasks: Vec<PlayTask>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlayTask {
    #[serde(default)]
    is_primary: bool,
    category: Option<String>,
    path: Option<String>,
    arguments: Option<String>,
    working_dir: Option<String>,
}

impl Info {
    fn is_dlc(&self) -> bool {
        self.root_game_id.as_ref().is_some_and(|root| *root != self.game_id)
    }

    /// The game's primary play task; documents and links are tasks too.
    fn primary_task(&self, dir: &Path) -> Option<Task> {
        let task = self.play_tasks.iter().find(|t| t.is_primary && t.category.as_deref().is_none_or(|c| c == "game"))?;
        let exe = dir.join(task.path.as_ref()?.replace('/', "\\"));
        let working = task.working_dir.as_ref().map(|d| dir.join(d.replace('/', "\\"))).or_else(|| exe.parent().map(Path::to_path_buf))?;
        Some(Task { exe, args: task.arguments.clone().unwrap_or_default(), dir: working })
    }
}

fn read_info(dir: &Path, id: &str) -> Option<Info> {
    serde_json::from_slice(&std::fs::read(dir.join(format!("goggame-{id}.info"))).ok()?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primary_task_and_dlc() {
        let info: Info = serde_json::from_str(
            r#"{"gameId": "1", "rootGameId": "1", "name": "Game", "playTasks": [
                {"category": "document", "isPrimary": false, "path": "manual.pdf", "type": "FileTask"},
                {"category": "game", "isPrimary": true, "path": "Bin/AN.exe", "workingDir": "Bin", "type": "FileTask"}]}"#,
        )
        .unwrap();
        let task = info.primary_task(Path::new(r"C:\Games\Game")).unwrap();
        assert_eq!(task.exe, Path::new(r"C:\Games\Game\Bin\AN.exe"));
        assert_eq!(task.dir, Path::new(r"C:\Games\Game\Bin"));
        assert!(!info.is_dlc());
        let dlc: Info = serde_json::from_str(r#"{"gameId": "2", "rootGameId": "1"}"#).unwrap();
        assert!(dlc.is_dlc() && dlc.primary_task(Path::new(r"C:\x")).is_none());
    }
}

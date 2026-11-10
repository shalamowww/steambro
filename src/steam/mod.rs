//! Steam's own files and process: where Steam is installed, which accounts use it, their non-Steam
//! shortcuts, collections, settings, and artwork folder, and closing and starting Steam. It reads
//! and writes; sync decides what to change. Steam keeps shortcuts and collections in memory and
//! writes them back when it exits, so steambro changes those files only while Steam is closed.

mod vdf;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use vdf::Value;

pub struct Steam {
    root: PathBuf,
}

impl Steam {
    pub fn locate() -> crate::Result<Self> {
        let path = windows_registry::CURRENT_USER
            .open(r"Software\Valve\Steam")
            .and_then(|key| key.get_string("SteamPath"))
            .map_err(|_| "Steam doesn't seem to be installed (no SteamPath in the registry)")?;
        // Steam stores this with forward slashes.
        Ok(Self { root: PathBuf::from(path.replace('/', "\\")) })
    }

    /// Steam's folder, like `C:\Program Files (x86)\Steam`.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Steam's version, from the manifest of its client package.
    pub fn version(&self) -> Option<String> {
        let text = std::fs::read_to_string(self.root.join(r"package\steam_client_win64.manifest")).ok()?;
        let line = text.lines().find(|l| l.trim_start().starts_with("\"version\""))?;
        Some(line.split('"').nth(3)?.to_string())
    }

    /// Accounts that have signed in to Steam on this PC.
    pub fn users(&self) -> crate::Result<Vec<SteamUser>> {
        let names = self.account_names();
        let mut users = Vec::new();
        for entry in std::fs::read_dir(self.root.join("userdata"))? {
            let entry = entry?;
            // One folder per account, named by its numeric account ID; "0" is a placeholder.
            let Ok(id) = entry.file_name().to_string_lossy().parse::<u32>() else { continue };
            if id != 0 {
                let name = names.iter().find(|(account, _, _)| *account == id).map(|(_, name, _)| name.clone());
                users.push(SteamUser { id, name, dir: entry.path() });
            }
        }
        users.sort_by_key(|u| u.id);
        Ok(users)
    }

    /// The account signed in now, or else the one that signed in last.
    pub fn current_user(&self) -> crate::Result<Option<SteamUser>> {
        let mut users = self.users()?;
        let active = windows_registry::CURRENT_USER
            .open(r"Software\Valve\Steam\ActiveProcess")
            .and_then(|key| key.get_u32("ActiveUser"))
            .unwrap_or(0);
        let recent = self.account_names().into_iter().find(|(_, _, recent)| *recent).map(|(id, _, _)| id);
        let pick = [active, recent.unwrap_or(0)].into_iter().find_map(|id| users.iter().position(|u| u.id == id && id != 0));
        Ok(match pick {
            Some(i) => Some(users.swap_remove(i)),
            None => users.into_iter().next(),
        })
    }

    /// `(account id, persona name, signed in most recently)` from `config\loginusers.vdf`.
    fn account_names(&self) -> Vec<(u32, String, bool)> {
        let Ok(text) = std::fs::read_to_string(self.root.join(r"config\loginusers.vdf")) else { return Vec::new() };
        let Ok(root) = vdf::text::parse(&text) else { return Vec::new() };
        let Some(vdf::text::Value::Map(users)) = vdf::text::get(&root, "users") else { return Vec::new() };
        users
            .iter()
            .filter_map(|(steam_id, user)| {
                let vdf::text::Value::Map(user) = user else { return None };
                // The low 32 bits of a 64-bit Steam ID are the account ID.
                let account = (steam_id.parse::<u64>().ok()? & 0xFFFF_FFFF) as u32;
                let name = vdf::text::get_str(user, "PersonaName")?.to_string();
                Some((account, name, vdf::text::get_str(user, "MostRecent") == Some("1")))
            })
            .collect()
    }

    pub fn is_running() -> bool {
        crate::process::is_running("steam.exe")
    }

    /// Asks Steam to exit and waits until it has, up to 3 minutes. Steam saves its state on the way
    /// out, usually within seconds; a Steam that hangs on the way out took 2 minutes (2026-11-10),
    /// and a wait of 1 minute then left it closed with the change undone.
    pub fn shut_down(&self) -> crate::Result<()> {
        std::process::Command::new(self.root.join("steam.exe")).arg("-shutdown").spawn()?;
        let deadline = Instant::now() + Duration::from_secs(180);
        while Self::is_running() {
            if Instant::now() > deadline {
                return Err("Steam didn't close within 3 minutes; close it and try again".into());
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        Ok(())
    }

    /// Hands a `steam://` URL to the running Steam.
    pub fn open_url(&self, url: &str) -> crate::Result<()> {
        std::process::Command::new(self.root.join("steam.exe")).arg(url).spawn()?;
        Ok(())
    }

    /// `open_url`, waiting until the steam.exe that hands the URL to the running Steam has ended
    /// (30 to 90 ms on the development PC), at most 2 seconds, so that URLs opened one after another
    /// reach Steam in that order.
    pub fn open_url_in_order(&self, url: &str) -> crate::Result<()> {
        let mut child = std::process::Command::new(self.root.join("steam.exe")).arg(url).spawn()?;
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline && child.try_wait()?.is_none() {
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    }

    pub fn start(&self) -> crate::Result<()> {
        std::process::Command::new(self.root.join("steam.exe")).spawn()?;
        Ok(())
    }

    fn config_path(&self) -> PathBuf {
        self.root.join(r"config\config.vdf")
    }

    /// Whether Steam ignores controllers with this vendor and product ID: its controller
    /// blocklist, `controller_blacklist` in `config\config.vdf`, lists them.
    pub fn ignores_controller(&self, vendor: u16, product: u16) -> bool {
        self.ignored_controllers().contains(&(vendor, product))
    }

    /// The vendor and product IDs in Steam's controller blocklist.
    pub fn ignored_controllers(&self) -> Vec<(u16, u16)> {
        std::fs::read_to_string(self.config_path()).map(|text| ignored_controllers(&text)).unwrap_or_default()
    }

    /// Takes these controllers out of Steam's controller blocklist, as `ignore_controller` puts one
    /// in: Steam must be closed, and a backup copy goes first. Returns whether the file changed.
    pub fn unignore_controllers(&self, which: &[(u16, u16)]) -> crate::Result<bool> {
        if !cfg!(test) && Self::is_running() {
            return Err("Steam is running; it has to be closed while steambro changes its settings".into());
        }
        let path = self.config_path();
        let Some(text) = without_ignored_controllers(&std::fs::read_to_string(&path)?, which) else { return Ok(false) };
        std::fs::copy(&path, path.with_extension("vdf.steambro-backup"))?;
        let temp = path.with_extension("vdf.steambro-new");
        std::fs::write(&temp, text)?;
        std::fs::rename(&temp, &path)?;
        Ok(true)
    }

    /// Adds the vendor and product ID to Steam's controller blocklist, so that from its next start
    /// Steam ignores such controllers: no layout, no "controller connected", no place in its list.
    /// Programs other than Steam still see them. A backup copy goes to
    /// `config.vdf.steambro-backup` first. Steam writes the file when it exits, so it must be
    /// closed. Returns whether the file changed.
    pub fn ignore_controller(&self, vendor: u16, product: u16) -> crate::Result<bool> {
        if !cfg!(test) && Self::is_running() {
            return Err("Steam is running; it has to be closed while steambro changes its settings".into());
        }
        let path = self.config_path();
        let Some(text) = with_ignored_controller(&std::fs::read_to_string(&path)?, vendor, product)? else { return Ok(false) };
        std::fs::copy(&path, path.with_extension("vdf.steambro-backup"))?;
        let temp = path.with_extension("vdf.steambro-new");
        std::fs::write(&temp, text)?;
        std::fs::rename(&temp, &path)?;
        Ok(true)
    }
}

/// The vendor and product IDs in Steam's controller blocklist, a line like
/// `"controller_blacklist"		"54c/ce6,45e/7eb0"` at the top of `InstallConfigStore`: hex, maybe
/// with `0x`, separated by commas. Steam reads it only there (its controller code asks for the key
/// at the root of config.vdf); a line elsewhere, like the one an earlier steambro wrote under
/// `Software` > `Valve` > `Steam`, does nothing.
fn ignored_controllers(config: &str) -> Vec<(u16, u16)> {
    let lines: Vec<String> = config.lines().map(String::from).collect();
    blocklist_lines(&lines)
        .into_iter()
        .find(|(_, root)| *root)
        .and_then(|(i, _)| blocklist(&lines[i]).map(|list| list.split(',').filter_map(controller_id).collect()))
        .unwrap_or_default()
}

/// config.vdf without these controllers in any blocklist line; the other entries keep their
/// spelling. A line out of Steam's place that has nothing left goes. `None` when none of them is
/// listed.
fn without_ignored_controllers(config: &str, which: &[(u16, u16)]) -> Option<String> {
    without_controllers(config, which, true)
}

/// `without_ignored_controllers`, but leaving the root's line alone when `root_too` is false.
fn without_controllers(config: &str, which: &[(u16, u16)], root_too: bool) -> Option<String> {
    let newline = if config.contains("\r\n") { "\r\n" } else { "\n" };
    let mut lines: Vec<String> = config.split(newline).map(String::from).collect();
    let mut emptied = Vec::new();
    let mut changed = false;
    for (i, root) in blocklist_lines(&lines) {
        if root && !root_too {
            continue;
        }
        let entries: Vec<String> = blocklist(&lines[i]).unwrap_or_default().split(',').filter(|e| !e.trim().is_empty()).map(String::from).collect();
        let kept: Vec<String> = entries.iter().filter(|e| !controller_id(e).is_some_and(|id| which.contains(&id))).cloned().collect();
        if kept.len() == entries.len() {
            continue;
        }
        changed = true;
        if kept.is_empty() && !root {
            emptied.push(i);
        } else {
            let indent: String = lines[i].chars().take_while(|c| c.is_whitespace()).collect();
            lines[i] = format!("{indent}\"controller_blacklist\"\t\t\"{}\"", kept.join(","));
        }
    }
    for i in emptied.into_iter().rev() {
        lines.remove(i);
    }
    changed.then(|| lines.join(newline))
}

/// One entry of the blocklist, like `45e/7eb0` or `0x045E/0x7EB0`.
fn controller_id(entry: &str) -> Option<(u16, u16)> {
    let hex = |s: &str| u16::from_str_radix(s.trim().trim_start_matches("0x").trim_start_matches("0X"), 16).ok();
    let (vendor, product) = entry.split_once('/')?;
    Some((hex(vendor)?, hex(product)?))
}

/// The value of the blocklist's line, without its quotes. Steam's keys ignore case.
fn blocklist(line: &str) -> Option<&str> {
    const KEY: &str = "\"controller_blacklist\"";
    let line = clean(line);
    let rest = line.get(..KEY.len()).filter(|key| key.eq_ignore_ascii_case(KEY)).and_then(|_| line.get(KEY.len()..))?;
    rest.trim().strip_prefix('"')?.strip_suffix('"')
}

/// A line of config.vdf without its indent and without a byte order mark, which a text editor may
/// have put before the first line.
fn clean(line: &str) -> &str {
    line.trim().trim_start_matches('\u{feff}').trim()
}

/// The key of a line that holds only a quoted key, like `"Software"`.
fn key_line(line: &str) -> Option<&str> {
    (line.len() > 1 && line.starts_with('"') && line.ends_with('"') && line.matches('"').count() == 2).then(|| line.trim_matches('"'))
}

/// The lines of config.vdf that hold a blocklist, each with whether it sits at the top of
/// `InstallConfigStore`, where Steam reads it. Steam writes each key and each brace on a line of
/// its own; a key with its opening brace on one line, as people write by hand, counts too.
fn blocklist_lines(lines: &[String]) -> Vec<(usize, bool)> {
    let mut found = Vec::new();
    let mut path: Vec<String> = Vec::new();
    let mut key: Option<String> = None;
    for (i, line) in lines.iter().enumerate() {
        let line = clean(line);
        if line == "{" {
            path.push(key.take().unwrap_or_default());
        } else if line == "}" {
            path.pop();
        } else if blocklist(line).is_some() {
            found.push((i, path.len() == 1 && path[0].eq_ignore_ascii_case("InstallConfigStore")));
        } else if let Some(opened) = line.strip_suffix('{').map(str::trim).and_then(key_line) {
            path.push(opened.to_string());
        } else if let Some(name) = key_line(line) {
            key = Some(name.to_string());
        }
    }
    found
}

/// The line that opens `InstallConfigStore`, the top of config.vdf.
fn store_opening(lines: &[String]) -> Option<usize> {
    let is_store = |line: &str| key_line(line).is_some_and(|key| key.eq_ignore_ascii_case("InstallConfigStore"));
    for (i, line) in lines.iter().enumerate() {
        let line = clean(line);
        if line.strip_suffix('{').map(str::trim).is_some_and(is_store) {
            return Some(i);
        }
        if is_store(line) {
            let next = lines[i + 1..].iter().position(|l| !clean(l).is_empty()).map(|j| i + 1 + j)?;
            return (clean(&lines[next]) == "{").then_some(next);
        }
    }
    None
}

/// config.vdf with the controller in the blocklist: added to the list's line at the top of
/// `InstallConfigStore`, or a new line there, where Steam reads it. The controller also leaves any
/// blocklist line elsewhere, like the one an earlier steambro wrote under `Software` > `Valve` >
/// `Steam`. Only those lines change, so the rest of the file stays as Steam wrote it. `None` when
/// nothing changes.
fn with_ignored_controller(config: &str, vendor: u16, product: u16) -> crate::Result<Option<String>> {
    let cleaned = without_controllers(config, &[(vendor, product)], false);
    let config = cleaned.as_deref().unwrap_or(config);
    if ignored_controllers(config).contains(&(vendor, product)) {
        return Ok(cleaned);
    }
    let entry = format!("{vendor:x}/{product:x}");
    let newline = if config.contains("\r\n") { "\r\n" } else { "\n" };
    let mut lines: Vec<String> = config.split(newline).map(String::from).collect();
    if let Some((i, _)) = blocklist_lines(&lines).into_iter().find(|(_, root)| *root) {
        let list = blocklist(&lines[i]).unwrap_or_default().trim_end_matches(',');
        let indent: String = lines[i].chars().take_while(|c| c.is_whitespace()).collect();
        let list = if list.is_empty() { entry } else { format!("{list},{entry}") };
        lines[i] = format!("{indent}\"controller_blacklist\"\t\t\"{list}\"");
        return Ok(Some(lines.join(newline)));
    }
    let Some(open) = store_opening(&lines) else {
        return Err("Steam's config.vdf has no InstallConfigStore section".into());
    };
    lines.insert(open + 1, format!("\t\"controller_blacklist\"\t\t\"{entry}\""));
    Ok(Some(lines.join(newline)))
}

#[derive(Clone)]
pub struct SteamUser {
    pub id: u32,
    /// Persona name, when Steam remembers it.
    pub name: Option<String>,
    dir: PathBuf,
}

impl SteamUser {
    /// An account folder somewhere else, for tests.
    #[cfg(test)]
    pub fn at(id: u32, dir: PathBuf) -> Self {
        Self { id, name: None, dir }
    }

    pub fn shortcuts(&self) -> crate::Result<Vec<Shortcut>> {
        let Some(root) = self.read_shortcuts()? else { return Ok(Vec::new()) };
        let Some(Value::Map(list)) = vdf::get(&root, "shortcuts") else { return Ok(Vec::new()) };
        Ok(list
            .iter()
            .filter_map(|(_, entry)| match entry {
                Value::Map(fields) => Some(Shortcut::from_fields(fields)),
                _ => None,
            })
            .collect())
    }

    /// The account's controller settings, from `config\localconfig.vdf`. The file also holds login
    /// tokens, so steambro reads only the keys it needs and logs nothing from it.
    pub fn input_settings(&self) -> crate::Result<InputSettings> {
        InputSettings::parse(&std::fs::read_to_string(self.local_config())?)
    }

    /// The account's settings for Steam's overlay, from `config\localconfig.vdf`.
    pub fn overlay_settings(&self) -> crate::Result<OverlaySettings> {
        OverlaySettings::parse(&std::fs::read_to_string(self.local_config())?)
    }

    fn local_config(&self) -> PathBuf {
        self.dir.join(r"config\localconfig.vdf")
    }

    /// Where Steam looks for custom library artwork of this account.
    pub fn grid_dir(&self) -> PathBuf {
        self.dir.join(r"config\grid")
    }

    /// Appends the `added` shortcuts to shortcuts.vdf, points the shortcuts with the `updated` app
    /// IDs at what their new versions run (exe, start folder, launch options), and deletes the
    /// shortcuts with the `removed` app IDs. Everything else about an updated shortcut stays, its
    /// app ID included, so its artwork and playtime stay too. A backup copy goes to
    /// `shortcuts.vdf.steambro-backup` first. Steam keeps its own copy in memory and overwrites
    /// the file, so it must be closed.
    pub fn save_shortcuts(&self, added: &[&NewShortcut], updated: &[(u32, NewShortcut)], removed: &[u32]) -> crate::Result<()> {
        // Tests write to a scratch account folder, which the running Steam doesn't own.
        if !cfg!(test) && Steam::is_running() {
            return Err("Steam is running; it has to be closed while steambro changes its shortcuts".into());
        }
        let path = self.shortcuts_path();
        let mut root = self.read_shortcuts()?.unwrap_or_else(|| vec![("shortcuts".to_string(), Value::Map(Vec::new()))]);
        let Some(Value::Map(list)) = vdf::get_mut(&mut root, "shortcuts") else {
            return Err(format!("{} has no shortcut list", path.display()).into());
        };
        for (_, entry) in list.iter_mut() {
            let Value::Map(fields) = entry else { continue };
            let app_id = Shortcut::from_fields(fields).app_id;
            if let Some((_, new)) = updated.iter().find(|(id, _)| *id == app_id) {
                set(fields, "Exe", Value::Str(quoted(&new.exe)));
                set(fields, "StartDir", Value::Str(quoted(&new.start_dir)));
                set(fields, "LaunchOptions", Value::Str(new.launch_options.clone()));
            }
        }
        if !removed.is_empty() {
            list.retain(|(_, entry)| !matches!(entry, Value::Map(fields) if removed.contains(&Shortcut::from_fields(fields).app_id)));
            // Steam numbers the entries "0", "1", ... in order; close the gaps.
            for (number, (index, _)) in list.iter_mut().enumerate() {
                *index = number.to_string();
            }
        }
        // Entries are numbered "0", "1", ...; new ones continue after the highest number.
        let first = list.iter().filter_map(|(index, _)| index.parse::<u32>().ok()).max().map_or(0, |i| i + 1);
        for (index, shortcut) in (first..).zip(added) {
            list.push((index.to_string(), Value::Map(shortcut.fields())));
        }
        if path.exists() {
            std::fs::copy(&path, path.with_extension("vdf.steambro-backup"))?;
        } else {
            std::fs::create_dir_all(path.parent().expect("config folder"))?;
        }
        let temp = path.with_extension("vdf.steambro-new");
        std::fs::write(&temp, vdf::write(&root))?;
        std::fs::rename(&temp, &path)?;
        Ok(())
    }

    fn shortcuts_path(&self) -> PathBuf {
        self.dir.join(r"config\shortcuts.vdf")
    }

    /// Takes the app IDs out of every collection's removed list, and out of Hidden, so a game added
    /// again shows up where it did before. Steam keeps a shortcut's place in its collections after
    /// the shortcut is gone: a dynamic collection, like one of the installed games, lists it as
    /// removed, which keeps it out when it is added again. Each changed collection gets the time as
    /// its timestamp and version, as Steam ROM Manager does, so Steam keeps the local copy over its
    /// cloud's. A backup copy goes to `<file>.steambro-backup` first. Steam must be closed. Returns
    /// the names of the collections changed.
    pub fn restore_in_collections(&self, app_ids: &[u32]) -> crate::Result<Vec<String>> {
        let path = self.collections_path();
        if app_ids.is_empty() || !path.exists() {
            return Ok(Vec::new());
        }
        if !cfg!(test) && Steam::is_running() {
            return Err("Steam is running; it has to be closed while steambro changes its collections".into());
        }
        // A list of `[key, record]` pairs. A collection is a `user-collections.<id>` record whose
        // `value` is the collection as JSON text: `name`, `added`, `removed`, and `filterSpec` for a
        // dynamic one. The fields keep their order (`serde_json` with `preserve_order`).
        let mut entries: Vec<serde_json::Value> = serde_json::from_slice(&std::fs::read(&path)?)?;
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs();
        let listed = |id: &serde_json::Value| id.as_u64().is_some_and(|id| app_ids.iter().any(|a| u64::from(*a) == id));
        let mut changed = Vec::new();
        for entry in &mut entries {
            let Some([key, record]) = entry.as_array_mut().map(Vec::as_mut_slice) else { continue };
            let Some(key) = key.as_str().filter(|k| k.starts_with("user-collections.")) else { continue };
            if record.get("is_deleted").and_then(serde_json::Value::as_bool) == Some(true) {
                continue;
            }
            let Some(mut collection) = record.get("value").and_then(serde_json::Value::as_str).and_then(|v| serde_json::from_str::<serde_json::Value>(v).ok()) else { continue };
            // A hidden game is in Hidden's added list.
            let lists: &[&str] = if key == "user-collections.hidden" { &["removed", "added"] } else { &["removed"] };
            let mut touched = false;
            for list in lists {
                if let Some(ids) = collection.get_mut(*list).and_then(serde_json::Value::as_array_mut) {
                    let before = ids.len();
                    ids.retain(|id| !listed(id));
                    touched |= ids.len() != before;
                }
            }
            if touched {
                changed.push(collection.get("name").and_then(serde_json::Value::as_str).unwrap_or(key).to_string());
                record["value"] = collection.to_string().into();
                record["timestamp"] = now.into();
                record["version"] = now.to_string().into();
            }
        }
        if !changed.is_empty() {
            std::fs::copy(&path, path.with_extension("json.steambro-backup"))?;
            let temp = path.with_extension("json.steambro-new");
            std::fs::write(&temp, serde_json::to_vec(&entries)?)?;
            std::fs::rename(&temp, &path)?;
        }
        Ok(changed)
    }

    /// The account's collections, and other library settings Steam syncs:
    /// `config\cloudstorage\cloud-storage-namespace-<N>.json`, for the namespace with the highest
    /// version in `cloud-storage-namespaces.json`, or 1.
    fn collections_path(&self) -> PathBuf {
        let dir = self.dir.join(r"config\cloudstorage");
        let namespaces: Vec<(u32, String)> = std::fs::read(dir.join("cloud-storage-namespaces.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        let active = namespaces.iter().filter(|(_, version)| version != "0").max_by_key(|(_, version)| version.parse::<u64>().unwrap_or(0)).map_or(1, |(n, _)| *n);
        dir.join(format!("cloud-storage-namespace-{active}.json"))
    }

    fn read_shortcuts(&self) -> crate::Result<Option<vdf::Map>> {
        let path = self.shortcuts_path();
        if !path.exists() {
            return Ok(None);
        }
        vdf::parse(&std::fs::read(&path)?).map(Some).map_err(|e| format!("can't read {}: {e}", path.display()).into())
    }
}

/// An account's controller settings. Steam leaves a setting out of the file until the user changes
/// it, so `None` means Steam's default.
#[derive(Default, Debug, PartialEq)]
pub struct InputSettings {
    /// Steam Input for Xbox controllers (`SteamController_XBoxSupport`): `0` off, `1` on.
    pub xbox: Option<String>,
    /// For PlayStation controllers (`SteamController_PSSupport`): `0` off, `1` in games without
    /// PlayStation support, `2` on.
    pub playstation: Option<String>,
    /// For Switch Pro controllers (`SteamController_SwitchSupport`): `0` off, `1` on.
    pub nintendo: Option<String>,
    /// For other controllers (`SteamController_GenericGamepadSupport`): `0` off, `1` on.
    pub generic: Option<String>,
    /// The kinds of controllers used with this account (`ControllerTypesUsed`), like
    /// `controller_xboxone,controller_neptune,`.
    pub types_used: String,
    /// App IDs of the games with Steam Input turned off in their properties
    /// (`UseSteamControllerConfig` `0`; `1` is the default and `2` on).
    pub off_for: Vec<u32>,
}

impl InputSettings {
    fn parse(text: &str) -> crate::Result<Self> {
        use vdf::text::{Value, get, get_str};
        let root = vdf::text::parse(text)?;
        let Some(Value::Map(store)) = get(&root, "UserLocalConfigStore") else { return Ok(Self::default()) };
        let setting = |key: &str| get_str(store, key).map(String::from);
        let off_for = match get(store, "apps") {
            Some(Value::Map(apps)) => apps
                .iter()
                .filter_map(|(id, app)| match app {
                    Value::Map(app) if get_str(app, "UseSteamControllerConfig") == Some("0") => id.parse().ok(),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        };
        Ok(Self {
            xbox: setting("SteamController_XBoxSupport"),
            playstation: setting("SteamController_PSSupport"),
            nintendo: setting("SteamController_SwitchSupport"),
            generic: setting("SteamController_GenericGamepadSupport"),
            types_used: setting("ControllerTypesUsed").unwrap_or_default(),
            off_for,
        })
    }
}

/// An account's settings for Steam's overlay, in the `system` part of `localconfig.vdf`. Steam
/// leaves a setting out until the user changes it, so a missing one has Steam's default.
#[derive(Debug, PartialEq)]
pub struct OverlaySettings {
    /// "Enable the Steam Overlay while in-game" (`EnableGameOverlay`).
    pub enabled: bool,
    /// The keys that open the overlay (`InGameOverlayShortcutKey`): modifiers and a key name,
    /// separated by tabs, like `Shift\tKEY_TAB`, the default.
    pub key: String,
}

impl OverlaySettings {
    fn parse(text: &str) -> crate::Result<Self> {
        use vdf::text::{Value, get, get_str};
        let root = vdf::text::parse(text)?;
        let system = match get(&root, "UserLocalConfigStore") {
            Some(Value::Map(store)) => match get(store, "system") {
                Some(Value::Map(system)) => Some(system),
                _ => None,
            },
            _ => None,
        };
        let setting = |key: &str| system.and_then(|s| get_str(s, key)).map(String::from);
        Ok(Self {
            enabled: setting("EnableGameOverlay").as_deref() != Some("0"),
            key: setting("InGameOverlayShortcutKey").unwrap_or_else(|| "Shift\tKEY_TAB".into()),
        })
    }
}

pub struct Shortcut {
    /// Steam's ID for the shortcut; artwork files and playtime are keyed by it.
    pub app_id: u32,
    /// The name Steam shows.
    pub name: String,
    /// As Steam stores it, usually wrapped in quotes.
    pub exe: String,
    pub launch_options: String,
    /// "Enable the Steam Overlay while in-game" in the shortcut's properties (`AllowOverlay`).
    pub allow_overlay: bool,
}

impl Shortcut {
    fn from_fields(fields: &vdf::Map) -> Self {
        let text = |key| match vdf::get(fields, key) {
            Some(Value::Str(s)) => s.clone(),
            _ => String::new(),
        };
        let number = |key| match vdf::get(fields, key) {
            Some(Value::Int(n)) => Some(*n),
            _ => None,
        };
        Self {
            app_id: number("appid").unwrap_or(0),
            name: text("AppName"),
            exe: text("Exe"),
            launch_options: text("LaunchOptions"),
            allow_overlay: number("AllowOverlay") != Some(0),
        }
    }

    pub fn exe_path(&self) -> PathBuf {
        PathBuf::from(self.exe.trim().trim_matches('"'))
    }
}

/// A shortcut steambro adds.
pub struct NewShortcut {
    pub name: String,
    pub exe: PathBuf,
    pub start_dir: PathBuf,
    pub launch_options: String,
    pub icon: PathBuf,
    pub tags: Vec<String>,
}

impl NewShortcut {
    /// The ID steambro gives the shortcut: `crc32(quoted exe + name) | 0x80000000`, as older Steam
    /// versions made it and Steam ROM Manager does. So the same game gets the same ID, artwork file
    /// names, and playtime each time it is added. Current Steam gives a shortcut made in its own UI
    /// a random ID, but it uses a stored ID as it is.
    pub fn app_id(&self) -> u32 {
        crc32(format!("{}{}", quoted(&self.exe), self.name).as_bytes()) | 0x8000_0000
    }

    /// The fields Steam writes for a shortcut, in Steam's order.
    fn fields(&self) -> vdf::Map {
        let s = |v: &str| Value::Str(v.to_string());
        let tags = self.tags.iter().enumerate().map(|(i, t)| (i.to_string(), s(t))).collect();
        vec![
            ("appid".into(), Value::Int(self.app_id())),
            ("AppName".into(), s(&self.name)),
            ("Exe".into(), s(&quoted(&self.exe))),
            ("StartDir".into(), s(&quoted(&self.start_dir))),
            ("icon".into(), s(&self.icon.to_string_lossy())),
            ("ShortcutPath".into(), s("")),
            ("LaunchOptions".into(), s(&self.launch_options)),
            ("IsHidden".into(), Value::Int(0)),
            ("AllowDesktopConfig".into(), Value::Int(1)),
            ("AllowOverlay".into(), Value::Int(1)),
            ("openvr".into(), Value::Int(0)),
            ("Devkit".into(), Value::Int(0)),
            ("DevkitGameID".into(), s("")),
            ("DevkitOverrideAppID".into(), Value::Int(0)),
            ("LastPlayTime".into(), Value::Int(0)),
            ("FlatpakAppID".into(), s("")),
            ("sortas".into(), s("")),
            ("tags".into(), Value::Map(tags)),
        ]
    }
}

/// Sets a field, keeping its place and the casing of its key when the field exists.
fn set(fields: &mut vdf::Map, key: &str, value: Value) {
    match vdf::get_mut(fields, key) {
        Some(existing) => *existing = value,
        None => fields.push((key.to_string(), value)),
    }
}

fn quoted(path: &Path) -> String {
    format!("\"{}\"", path.display())
}

/// CRC-32 (IEEE), as Steam uses for shortcut IDs.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn added_games_leave_removed_lists_and_hidden() {
        let dir = std::env::temp_dir().join(format!("steambro-test-collections-{}", std::process::id()));
        let cloud = dir.join(r"config\cloudstorage");
        std::fs::create_dir_all(&cloud).unwrap();
        std::fs::write(cloud.join("cloud-storage-namespaces.json"), r#"[[3,"0"],[1,"906"],[4,"0"]]"#).unwrap();
        let entry = |key: &str, value: &str| serde_json::json!([key, {"key": key, "timestamp": 1, "value": value, "version": "906", "conflictResolutionMethod": "custom", "strMethodId": "union-collections"}]);
        let entries = serde_json::json!([
            entry("user-collections.uc-1", r#"{"id":"uc-1","name":"Installed locally","added":[],"removed":[3280236582,7],"filterSpec":{}}"#),
            entry("user-collections.hidden", r#"{"id":"hidden","name":"Hidden","added":[3280236582],"removed":[]}"#),
            entry("user-collections.uc-2", r#"{"id":"uc-2","name":"Other","added":[3280236582],"removed":[8]}"#),
            ["user-collections.uc-3", {"key": "user-collections.uc-3", "timestamp": 1, "is_deleted": true, "version": "877"}],
            entry("showcases.0", r#"{"nShowcaseId":0}"#),
        ]);
        let file = cloud.join("cloud-storage-namespace-1.json");
        std::fs::write(&file, entries.to_string()).unwrap();
        let user = SteamUser::at(1, dir.clone());

        assert_eq!(user.restore_in_collections(&[3280236582]).unwrap(), ["Installed locally", "Hidden"]);
        let after: serde_json::Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        let value = |i: usize| serde_json::from_str::<serde_json::Value>(after[i][1]["value"].as_str().unwrap()).unwrap();
        assert_eq!(value(0)["removed"], serde_json::json!([7]));
        assert_eq!(value(1)["added"], serde_json::json!([]));
        assert_ne!(after[0][1]["version"], "906", "a changed collection gets a new version");
        assert_eq!(after[0][1]["key"], "user-collections.uc-1", "the fields keep their order and values");
        assert_eq!(after[2], entries[2], "a collection the game is in stays as it is");
        assert_eq!(after[3], entries[3]);
        assert_eq!(after[4], entries[4]);
        assert!(file.with_extension("json.steambro-backup").exists());
        assert!(user.restore_in_collections(&[3280236582]).unwrap().is_empty(), "nothing left to change");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_controller_joins_steams_blocklist() {
        let config = "\"InstallConfigStore\"\r\n{\r\n\t\"Software\"\r\n\t{\r\n\t\t\"Valve\"\r\n\t\t{\r\n\t\t\t\"Steam\"\r\n\t\t\t{\r\n\t\t\t\t\"Rate\"\t\t\"30000\"\r\n\t\t\t\t\"ShaderCacheManager\"\r\n\t\t\t\t{\r\n\t\t\t\t}\r\n\t\t\t}\r\n\t\t}\r\n\t}\r\n\t\"SDL_GamepadBind\"\t\t\"x\"\r\n}\r\n";
        let line = "\t\"controller_blacklist\"\t\t\"45e/7eb0\"\r\n";
        // Steam reads the list only at the top of InstallConfigStore.
        let added = with_ignored_controller(config, 0x045E, 0x7EB0).unwrap().unwrap();
        assert!(added.starts_with(&format!("\"InstallConfigStore\"\r\n{{\r\n{line}\t\"Software\"")), "{added}");
        assert_eq!(added.replacen(line, "", 1), config, "nothing else changes");
        assert_eq!(ignored_controllers(&added), [(0x045E, 0x7EB0)]);
        assert!(with_ignored_controller(&added, 0x045E, 0x7EB0).unwrap().is_none(), "once is enough");

        // A line that an earlier steambro wrote under Software > Valve > Steam does nothing; it moves to the top.
        let old = config.replace("\t\t\t\t\"Rate\"", "\t\t\t\t\"controller_blacklist\"\t\t\"45e/7eb0\"\r\n\t\t\t\t\"Rate\"");
        assert_eq!(ignored_controllers(&old), []);
        assert_eq!(with_ignored_controller(&old, 0x045E, 0x7EB0).unwrap().unwrap(), added);
        let both = added.replace("\t\t\t\t\"Rate\"", "\t\t\t\t\"controller_blacklist\"\t\t\"45e/7eb0\"\r\n\t\t\t\t\"Rate\"");
        assert_eq!(with_ignored_controller(&both, 0x045E, 0x7EB0).unwrap().unwrap(), added, "the old line goes");

        // A list the user made keeps its entries, in whatever form, wherever it is.
        let theirs = config.replacen("{\r\n", "{\r\n\t\"controller_blacklist\"\t\t\"0x54c/0xCE6,\"\r\n", 1);
        assert!(with_ignored_controller(&theirs, 0x045E, 0x7EB0).unwrap().unwrap().contains("\"0x54c/0xCE6,45e/7eb0\""));
        assert!(with_ignored_controller(&theirs.replace("0x54c/0xCE6,", "0x45E/0x7EB0"), 0x045E, 0x7EB0).unwrap().is_none());
        let elsewhere = old.replace("\"45e/7eb0\"", "\"54c/ce6,45e/7eb0\"");
        let moved = with_ignored_controller(&elsewhere, 0x045E, 0x7EB0).unwrap().unwrap();
        assert!(moved.contains("\t\t\t\t\"controller_blacklist\"\t\t\"54c/ce6\"\r\n") && moved.contains(line), "{moved}");
        assert_eq!(ignored_controllers(&moved), [(0x045E, 0x7EB0)]);
        assert!(with_ignored_controller("\"Other\"\n{\n}\n", 1, 2).is_err());

        // Steam writes LF line ends; the same happens with them.
        let lf = |text: &str| text.replace("\r\n", "\n");
        assert_eq!(with_ignored_controller(&lf(config), 0x045E, 0x7EB0).unwrap().unwrap(), lf(&added));
        assert_eq!(with_ignored_controller(&lf(&old), 0x045E, 0x7EB0).unwrap().unwrap(), lf(&added));
        // A byte order mark from a text editor changes nothing.
        assert_eq!(ignored_controllers(&format!("\u{feff}{added}")), [(0x045E, 0x7EB0)]);
        assert_eq!(with_ignored_controller(&format!("\u{feff}{config}"), 0x045E, 0x7EB0).unwrap().unwrap(), format!("\u{feff}{added}"));
        // Hand-written: keys with their brace on one line, and the key in other capitals.
        let by_hand = config.replacen("\"InstallConfigStore\"\r\n{", "\"InstallConfigStore\" {", 1).replacen("\t\"Software\"\r\n\t{", "\t\"Software\" {", 1);
        assert!(with_ignored_controller(&by_hand, 0x045E, 0x7EB0).unwrap().unwrap().starts_with(&format!("\"InstallConfigStore\" {{\r\n{line}")));
        let capitals = by_hand.replace("\t\"SDL_GamepadBind\"", "\t\"Controller_Blacklist\"\t\t\"54c/ce6\"\r\n\t\"SDL_GamepadBind\"");
        assert_eq!(ignored_controllers(&capitals), [(0x054C, 0x0CE6)]);
        let joined = with_ignored_controller(&capitals, 0x045E, 0x7EB0).unwrap().unwrap();
        assert!(joined.contains("\"54c/ce6,45e/7eb0\"") && joined.matches("lacklist").count() == 1, "{joined}");

        // Taking one out keeps the others as spelled, and only those lines change.
        let full = with_ignored_controller(&theirs, 0x045E, 0x7EB0).unwrap().unwrap();
        let fewer = without_ignored_controllers(&full, &[(0x045E, 0x7EB0)]).unwrap();
        assert!(fewer.contains("\"controller_blacklist\"\t\t\"0x54c/0xCE6\""), "{fewer}");
        assert_eq!(ignored_controllers(&fewer), [(0x054C, 0x0CE6)]);
        assert!(without_ignored_controllers(&fewer, &[(0x045E, 0x7EB0)]).is_none(), "nothing left to take out");
        assert_eq!(ignored_controllers(&without_ignored_controllers(&fewer, &[(0x054C, 0x0CE6)]).unwrap()), []);
        assert!(without_ignored_controllers(config, &[(1, 2)]).is_none(), "no blocklist line");
        assert_eq!(without_ignored_controllers(&old, &[(0x045E, 0x7EB0)]).unwrap(), config, "an emptied old line goes");
    }

    #[test]
    fn crc32_matches_reference() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    /// Reads this PC's real shortcuts.vdf files (read-only) and checks that writing them back would
    /// produce the same bytes. Skipped where Steam isn't installed.
    #[test]
    fn real_shortcuts_round_trip() {
        let Ok(steam) = Steam::locate() else { return };
        for user in steam.users().unwrap() {
            let path = user.shortcuts_path();
            if let Ok(bytes) = std::fs::read(&path) {
                let parsed = vdf::parse(&bytes).unwrap();
                assert!(vdf::write(&parsed) == bytes, "{} doesn't round-trip", path.display());
            }
        }
    }

    #[test]
    fn input_settings_from_localconfig() {
        let text = r#""UserLocalConfigStore"
{
	"ControllerTypesUsed"		"controller_xboxone,controller_neptune,"
	"apps"
	{
		"367670" { "UseSteamControllerConfig" "2" }
		"3427979141" { "UseSteamControllerConfig" "0" "SteamControllerRumble" "-1" }
	}
	"SteamController_XBoxSupport"		"0"
	"SteamController_PSSupport"		"1"
}"#;
        let settings = InputSettings::parse(text).unwrap();
        assert_eq!(settings.xbox.as_deref(), Some("0"));
        assert_eq!(settings.playstation.as_deref(), Some("1"));
        assert_eq!(settings.nintendo, None);
        assert_eq!(settings.types_used, "controller_xboxone,controller_neptune,");
        assert_eq!(settings.off_for, [3427979141]);
    }

    /// Reads this PC's real localconfig.vdf files (read-only). Skipped where Steam isn't installed.
    #[test]
    fn real_input_settings_parse() {
        let Ok(steam) = Steam::locate() else { return };
        for user in steam.users().unwrap() {
            if user.local_config().exists() {
                user.input_settings().unwrap();
                user.overlay_settings().unwrap();
            }
        }
    }

    #[test]
    fn overlay_settings_from_localconfig() {
        let defaults = OverlaySettings::parse(r#""UserLocalConfigStore" { "system" { "PushToTalkKey" "0" } }"#).unwrap();
        assert_eq!(defaults, OverlaySettings { enabled: true, key: "Shift\tKEY_TAB".into() });
        let changed = OverlaySettings::parse(
            "\"UserLocalConfigStore\" { \"system\" { \"EnableGameOverlay\" \"0\" \"InGameOverlayShortcutKey\" \"Ctrl\tKEY_F1\" \"InGameOverlayScreenshotHotKey\" \"KEY_NONE\" } }",
        )
        .unwrap();
        assert_eq!(changed, OverlaySettings { enabled: false, key: "Ctrl\tKEY_F1".into() });
    }
}

//! Finds Xbox PC games: packages installed for the current user that ship a MicrosoftGame.config.
//! Also looks up the Windows apps the user added to the Xbox app's library (added.rs).

use std::path::{Path, PathBuf};

use windows::Management::Deployment::{PackageManager, PackageTypes};
use windows::core::HSTRING;

pub struct XboxGame {
    pub name: String,
    /// Application User Model ID, `<package family>!<app id>`: what Windows launches.
    pub aumid: String,
    /// Package family name; the Microsoft Store catalog finds the game's artwork by it.
    pub family: String,
    /// The folder the game's files are in, with links resolved (see `real_path`).
    pub install_dir: PathBuf,
    /// The package folder as Windows reports it.
    pub package_dir: PathBuf,
    /// The game's own executable relative to `install_dir`, as declared in MicrosoftGame.config.
    /// `None` for an app added in the Xbox app, which isn't a game.
    pub executable: Option<PathBuf>,
}

/// Games that can't be read are skipped, with a note in `warnings`.
pub fn installed_games(warnings: &mut Vec<String>) -> crate::Result<Vec<XboxGame>> {
    // An empty user SID means the current user, which needs no admin rights.
    let packages = PackageManager::new()?
        .FindPackagesByUserSecurityIdWithPackageTypes(&HSTRING::new(), PackageTypes::Main)?;
    let mut games = Vec::new();
    for package in packages {
        // A package that is mid-install or damaged can fail these calls; it isn't playable then.
        let Ok(dir) = package.InstalledPath() else { continue };
        let package_dir = PathBuf::from(dir.to_string());
        let install_dir = real_path(package_dir.clone());
        let config_path = install_dir.join("MicrosoftGame.config");
        if !config_path.is_file() {
            continue;
        }
        let config = match GameConfig::read(&config_path) {
            Ok(config) => config,
            Err(e) => {
                warnings.push(format!("skipped {}: {e}", config_path.display()));
                continue;
            }
        };
        let family = package.Id()?.FamilyName()?.to_string();
        // Windows resolves the localized name; the config often only has a resource reference.
        let package_name = package
            .DisplayName()
            .map(|n| n.to_string())
            .ok()
            .filter(|n| !n.is_empty())
            .unwrap_or(config.default_name);
        let app_ids: Vec<String> = package
            .GetAppListEntries()?
            .into_iter()
            .filter_map(|entry| entry.AppUserModelId().ok())
            .filter_map(|aumid| aumid.to_string().split_once('!').map(|(_, app)| app.to_string()))
            .collect();
        let several = config.executables.len() > 1;
        for exe in config.executables {
            // Configs may leave out the app ID when the package has a single app; Windows knows it.
            let app_id = match (exe.id, app_ids.as_slice()) {
                (Some(id), _) => id,
                (None, [only]) => only.clone(),
                (None, _) => {
                    warnings.push(format!("skipped {} in {family}: can't tell which app it is", exe.path.display()));
                    continue;
                }
            };
            let name = if several { format!("{package_name} ({app_id})") } else { package_name.clone() };
            games.push(XboxGame {
                name,
                aumid: format!("{family}!{app_id}"),
                family: family.clone(),
                install_dir: install_dir.clone(),
                package_dir: package_dir.clone(),
                executable: Some(exe.path),
            });
        }
    }
    games.sort_by_key(|g| g.name.to_lowercase());
    Ok(games)
}

/// The installed packages of the apps added in the Xbox app. An app that isn't installed any more
/// is left out.
pub fn installed_apps(added: &[super::added::AddedApp]) -> crate::Result<Vec<XboxGame>> {
    let manager = PackageManager::new()?;
    let mut apps = Vec::new();
    for app in added {
        let Some((family, _)) = app.aumid.split_once('!') else { continue };
        let Ok(found) = manager.FindPackagesByUserSecurityIdPackageFamilyName(&HSTRING::new(), &HSTRING::from(family)) else { continue };
        let Some(package) = found.into_iter().next() else { continue };
        let Ok(dir) = package.InstalledPath() else { continue };
        let package_dir = PathBuf::from(dir.to_string());
        let name = package.DisplayName().map(|n| n.to_string()).ok().filter(|n| !n.is_empty()).unwrap_or_else(|| app.title.clone());
        apps.push(XboxGame {
            name,
            aumid: app.aumid.clone(),
            family: family.to_string(),
            install_dir: real_path(package_dir.clone()),
            package_dir,
            executable: None,
        });
    }
    Ok(apps)
}

/// Games installed through the Xbox app sit in a folder like `C:\XboxGames\<game>\Content`, and
/// Windows' own package folder is just a link to it. Follow the link so paths match what users see.
fn real_path(path: PathBuf) -> PathBuf {
    let Ok(resolved) = std::fs::canonicalize(&path) else { return path };
    // canonicalize returns `\\?\C:\...`; drop that prefix for plain drive paths.
    match resolved.to_str().and_then(|s| s.strip_prefix(r"\\?\")) {
        Some(plain) if !plain.starts_with("UNC\\") => PathBuf::from(plain),
        _ => resolved,
    }
}

struct GameConfig {
    default_name: String,
    executables: Vec<Executable>,
}

struct Executable {
    id: Option<String>,
    path: PathBuf,
}

impl GameConfig {
    fn read(path: &Path) -> crate::Result<Self> {
        let text = decode(&std::fs::read(path)?)?;
        let doc = roxmltree::Document::parse(&text)?;
        let default_name = doc
            .descendants()
            .find(|n| n.has_tag_name("ShellVisuals"))
            .and_then(|n| n.attribute("DefaultDisplayName"))
            .filter(|n| !n.starts_with("ms-resource:"))
            .unwrap_or_default()
            .to_string();
        let executables: Vec<_> = doc
            .descendants()
            .filter(|n| n.has_tag_name("Executable"))
            // The same file can describe console builds; only PC entries launch here.
            .filter(|n| n.attribute("TargetDeviceFamily").is_none_or(|f| f.eq_ignore_ascii_case("PC")))
            .filter_map(|n| {
                Some(Executable { id: n.attribute("Id").map(String::from), path: PathBuf::from(n.attribute("Name")?) })
            })
            .collect();
        if executables.is_empty() {
            return Err("no PC executable listed".into());
        }
        Ok(Self { default_name, executables })
    }
}

/// Game configs are usually UTF-8, but some tools write UTF-16 with a byte order mark.
fn decode(bytes: &[u8]) -> crate::Result<String> {
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        let units: Vec<u16> = rest.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
        return Ok(String::from_utf16(&units)?);
    }
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    Ok(String::from_utf8(bytes.to_vec())?)
}

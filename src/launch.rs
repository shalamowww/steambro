//! `steambro launch <game key>`: what a game's Steam shortcut runs when its store needs steambro
//! to start it. Steam tracks steambro as the game, so steambro asks the game's store to start it
//! and stays open until the game has ended. While Steam can't see the game, steambro stands in
//! for Steam: its controllers and its overlay. When Steam ends the launch, the game ends with it.
//! An app starts in full screen. Each part follows the settings.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::bridge::{self, Bridge};
use crate::library::{self, Game, Started, is_inside};
use crate::overlay::{Hotkey, Overlay};
use crate::process::Job;
use crate::prompt;
use crate::settings::{AppRunning, Behavior, Settings};
use crate::steam::Steam;
use crate::window::{FullScreen, GameWindows};

pub fn run(game_key: &str) -> crate::Result<()> {
    crate::log!("starting {game_key}");
    let game = library::find_installed(game_key)?;
    let settings = Settings::load();
    let is_app = game.store.is_app(&game);
    if let Some(app) = game.store.app_name()
        && game.store.app_running()
    {
        restart_app(&game, app, &settings);
    }
    let mut job = exit_job(&settings.behavior(game_key, is_app, None));
    let started = game.store.start(&game)?;
    let behavior = settings.behavior(game_key, is_app, started.start_timeout);
    // The game's processes as last seen, and its packages; they tell which windows are the game's.
    let windows = Arc::new(Mutex::new(GameWindows::new(started.packages.clone())));
    let unseen = game.store.needs_bridge() || started.outside_steam;
    let stand_in = if unseen { StandIn::start(&behavior, &windows) } else { StandIn::default() };
    let full_screen = behavior.full_screen.then(|| FullScreen::start(windows.clone(), behavior.start_timeout));
    crate::log!("waiting for a process in {}", started.folders[0].display());
    let ended = wait_for_game(&started, &behavior, &windows, job.as_mut());
    drop(full_screen);
    drop(stand_in.overlay);
    drop(stand_in.bridge);
    // The game ended by itself, or never started: nothing more to end.
    drop(job);
    ended?;
    if let Some(after) = started.after {
        if behavior.close_app {
            after();
        } else {
            crate::log!("leaving the store's app open, as the settings say");
        }
    }
    Ok(())
}

/// Steam ends `steambro launch` and the processes it sees of the game, but not always all of them:
/// StarCraft II kept running when Steam ended Battle.net and the launch. And it sees no game that
/// Windows starts, like an Xbox game. So the launch's job ends the game with the launch.
fn exit_job(behavior: &Behavior) -> Option<Job> {
    if !behavior.exit_ends_game {
        crate::log!("the game keeps running if Steam ends steambro, as the settings say");
        return None;
    }
    Job::new().map_err(|e| crate::log!("Steam won't be able to end the game: {e}")).ok()
}

/// What steambro does in Steam's place while Steam can't see the game.
#[derive(Default)]
struct StandIn {
    bridge: Option<Bridge>,
    overlay: Option<Overlay>,
}

impl StandIn {
    fn start(behavior: &Behavior, windows: &Arc<Mutex<GameWindows>>) -> Self {
        let bridge = if behavior.layout {
            Bridge::start(behavior.bridge)
        } else {
            crate::log!("Steam keeps its desktop layout during the game, as the settings say");
            None
        };
        let overlay = if !behavior.overlay {
            crate::log!("no Steam overlay over the game, as the settings say");
            None
        } else {
            match steam_overlay() {
                Ok(key) => Some(Overlay::start(windows.clone(), if behavior.overlay_keys { key } else { None })),
                Err(reason) => {
                    crate::log!("no Steam overlay over the game: {reason}");
                    None
                }
            }
        };
        Self { bridge, overlay }
    }
}

/// Steam's overlay key for the launch, from the account's settings. An error says why there is no
/// overlay: Steam's settings turn it off for every game or for this one.
fn steam_overlay() -> Result<Option<Hotkey>, String> {
    let Some(app_id) = bridge::steam_app_id() else { return Err("Steam didn't start steambro".into()) };
    let Some(user) = Steam::locate().ok().and_then(|s| s.current_user().ok().flatten()) else { return Ok(Hotkey::parse("Shift\tKEY_TAB")) };
    let settings = user.overlay_settings().map_err(|e| e.to_string())?;
    if !settings.enabled {
        return Err("it's off in Steam's settings (In game)".into());
    }
    if user.shortcuts().unwrap_or_default().iter().any(|s| s.app_id == app_id && !s.allow_overlay) {
        return Err("it's off in the game's properties in Steam".into());
    }
    Ok(Hotkey::parse(&settings.key))
}

/// The store's app already runs outside Steam's launch, so Steam wouldn't see the game it starts.
/// As the settings say, or the user answers, steambro closes the app, so that the store's `start`
/// opens it again inside Steam's launch. An app that is busy, like with a download, stays.
fn restart_app(game: &Game, app: &str, settings: &Settings) {
    if let Some(reason) = game.store.app_busy() {
        crate::log!("{app} runs outside Steam, but {reason}, so it stays");
        return;
    }
    let restart = match settings.behavior(&game.key(), game.store.is_app(game), None).app_running {
        AppRunning::Restart => true,
        AppRunning::Leave => false,
        AppRunning::Ask => {
            crate::log!("{app} runs outside Steam; asking whether to restart it");
            let request = prompt::Request {
                store: game.store.id().to_string(),
                store_name: game.store.name().to_string(),
                app: app.to_string(),
                game: game.name().to_string(),
            };
            let timeout = Duration::from_secs(settings.launch.question_timeout_s.into());
            prompt::ask(&request, timeout).is_some_and(|answer| answer.restart)
        }
    };
    if !restart {
        crate::log!("{app} runs outside Steam and stays; Steam won't see the game");
        return;
    }
    crate::log!("closing {app}, which runs outside Steam, to start it inside Steam's launch");
    match game.store.close_app(game) {
        Ok(()) => crate::log!("{app} has closed"),
        Err(e) => crate::log!("couldn't close {app}: {e}"),
    }
}

/// Waits until a process of the game has run and then none has run for the grace time. Keeps the
/// processes in `windows` up to date, and adds each process to `job`, so that it ends when Steam
/// ends the launch.
fn wait_for_game(started: &Started, behavior: &Behavior, windows: &Mutex<GameWindows>, mut job: Option<&mut Job>) -> crate::Result<()> {
    let begun = Instant::now();
    let mut nudged = started.nudge.is_none();
    let mut last_seen: Option<Instant> = None;
    // The game's process the last time one ran, to log when it changes.
    let mut running: Option<PathBuf> = None;
    loop {
        let found = game_processes(started);
        windows.lock().unwrap().pids = found.iter().map(|(pid, _)| *pid).collect();
        if let Some(job) = job.as_deref_mut() {
            for (pid, path) in &found {
                match job.add(*pid) {
                    Ok(true) => crate::log!("{} ends if Steam ends the game", path.display()),
                    Ok(false) => {}
                    Err(e) => crate::log!("Steam can't end {}: {e}", path.display()),
                }
            }
        }
        let first = found.into_iter().next().map(|(_, path)| path);
        if let Some(path) = &first {
            if running.as_ref() != Some(path) {
                crate::log!("{} {}", if last_seen.is_some() { "now running:" } else { "game running:" }, path.display());
            }
            last_seen = Some(Instant::now());
        } else {
            if running.is_some() {
                crate::log!("no game process; waiting {}s in case it starts again", behavior.end_grace.as_secs());
            }
            match last_seen {
                Some(seen) if seen.elapsed() >= behavior.end_grace => {
                    crate::log!("game ended after {}s", begun.elapsed().as_secs());
                    return Ok(());
                }
                None if begun.elapsed() > behavior.start_timeout => return Err(started.not_started.clone().into()),
                None if !nudged && begun.elapsed() > behavior.nudge => {
                    nudged = true;
                    if let Some(nudge) = &started.nudge
                        && let Err(e) = nudge()
                    {
                        crate::log!("couldn't ask again: {e}");
                    }
                }
                _ => {}
            }
        }
        running = first;
        std::thread::sleep(behavior.check);
    }
}

/// The running processes of the game, with their executables: inside its folders, or with the
/// identity of one of its packages.
fn game_processes(started: &Started) -> Vec<(u32, PathBuf)> {
    crate::process::list()
        .iter()
        .filter(|p| p.name.to_lowercase().ends_with(".exe"))
        .filter_map(|p| {
            let path = crate::process::image_path(p.pid)?;
            let inside = started.folders.iter().any(|dir| is_inside(&path, dir));
            let packaged = !inside
                && !started.packages.is_empty()
                && crate::process::package_family(p.pid).is_some_and(|family| started.packages.iter().any(|f| f.eq_ignore_ascii_case(&family)));
            (inside || packaged).then_some((p.pid, path))
        })
        .collect()
}

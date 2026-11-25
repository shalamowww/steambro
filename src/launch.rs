//! `steambro launch <game key>`: what a game's Steam shortcut runs when its store needs steambro
//! to start it. Steam tracks steambro as the game, so steambro asks the game's store to start it
//! and stays open until the game has ended. While Steam can't see the game, steambro stands in
//! for Steam: its controllers and its overlay. When Steam ends the launch, the game ends with it.
//! An app starts in full screen. Each part follows the game's plan (plan.rs).

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::bridge::{self, Bridge};
use crate::library::{self, Game, Started, is_inside};
use crate::overlay::{Hotkey, Overlay};
use crate::plan::{self, Behavior};
use crate::process::{Exits, Family, Job};
use crate::prompt;
use crate::settings::{AppRunning, HandOver, Settings};
use crate::steam::Steam;
use crate::window::{Forward, FullScreen, GameWindows};

/// Starts the game with this key and waits until it has ended. An error means the game didn't
/// start; main.rs shows it in a message box.
pub fn run(game_key: &str) -> crate::Result<()> {
    crate::log!("starting {game_key}");
    let game = library::find_installed(game_key)?;
    let settings = Settings::load();
    let behavior = Behavior::of(&settings, &game);
    log_decisions(&game, &settings, &behavior);
    let mut family = Family::of_this_process();
    if let Some(app) = game.store.app_name()
        && game.store.app_running()
    {
        restart_app(&game, app, &behavior, &settings);
    }
    let mut job = exit_job(&behavior);
    // The game's processes as last seen, and its packages; they tell which windows are the game's.
    let windows = Arc::new(Mutex::new(GameWindows::default()));
    // For a store whose games Steam never sees, steambro stands in before the game starts, so the
    // bridge's virtual controllers are there when the game first looks for controllers.
    let mut stand_in = plan::stand_in(game.store.unseen(), false).map(|reason| StandIn::start(reason, &behavior, &windows));
    let started = game.store.start(&game)?;
    windows.lock().unwrap().packages = started.packages.clone();
    if stand_in.is_none() {
        stand_in = plan::stand_in(game.store.unseen(), started.outside_steam).map(|reason| StandIn::start(reason, &behavior, &windows));
    }
    // A game that Windows or a store started may show behind Steam, like Big Picture, or without
    // the front.
    let forward = Forward::start(windows.clone(), behavior.start_timeout);
    let stand_in = stand_in.unwrap_or_default();
    let full_screen = behavior.knobs.full_screen.then(|| FullScreen::start(windows.clone(), behavior.start_timeout));
    crate::log!("waiting for a process in {}", started.folders[0].display());
    // Only an app that this launch opened: one that ran before is the user's.
    let hand_over = game.store.app_name().filter(|_| started.after.is_some() && behavior.knobs.hand_over != HandOver::Keep);
    let handed_over = AtomicBool::new(false);
    let game_ended = AtomicBool::new(false);
    let ended = std::thread::scope(|scope| {
        if let Some(app) = hand_over {
            scope.spawn(|| hand_over_app(&game, app, &behavior, &windows, &game_ended, &handed_over));
        }
        // Only the app that this launch opened is the launch's; one that ran before is the user's.
        let app_exes = if started.after.is_some() { game.store.app_exes() } else { &[] };
        // A program that hides its window on purpose, like one in the tray, isn't hung.
        let watch_hung = !game.store.windows_may_hide(&game);
        let ended = wait_for_game(&started, &behavior, &windows, job.as_mut(), &mut family, (app_exes, &handed_over), watch_hung);
        game_ended.store(true, Ordering::Relaxed);
        ended
    });
    drop(forward);
    drop(full_screen);
    drop(stand_in.overlay);
    drop(stand_in.bridge);
    // The job stays until the store's app has closed below: if Steam ends the launch while it
    // waits for that, the app ends with it. Dropped then, or when the game never started, it ends
    // nothing more.
    ended?;
    family.update(&crate::process::list());
    let left = family.others();
    if !left.is_empty() {
        crate::log!("still running from this launch, which Steam counts as the game until they end: {}", left.join(", "));
    }
    if let Some(after) = started.after {
        if handed_over.load(Ordering::Relaxed) {
            crate::log!("the store's app was handed over during the game; nothing to close");
        } else if behavior.knobs.close_app {
            after();
        } else {
            crate::log!("leaving the store's app open, as the settings say");
        }
    }
    drop(job);
    Ok(())
}

/// Writes what decides this launch to the log, so that a report for a bug (report.rs) shows why
/// it went as it did: steambro's and Windows' versions, the settings and what they make of the
/// game, Steam's settings, the store's app and its own settings, and the driver.
fn log_decisions(game: &Game, settings: &Settings, behavior: &Behavior) {
    let joined = |lines: Vec<(String, String)>| lines.into_iter().map(|(k, v)| format!("{k}: {v}")).collect::<Vec<_>>().join("; ");
    crate::log!("{}", joined(crate::report::environment()));
    crate::log!("game: {} ({}), starts with {}, in {}", game.name(), game.store.name(), game.launch_description(), game.install_dir().display());
    let store = game.store.id();
    crate::log!(
        "settings: {:?}; {store}'s defaults {:?}; choices for {store} {:?}; for the game {:?}",
        settings.launch,
        game.store.defaults(),
        settings.store(store),
        settings.game(&game.key())
    );
    crate::log!("so this launch: {behavior:?}");
    crate::log!("Steam's app ID for the launch: {}", bridge::steam_app_id().map_or("none, Steam didn't start it".into(), |id| id.to_string()));
    crate::log!("{}", joined(crate::report::steam()));
    let app = game.store.diagnostics(Some(game));
    if !app.is_empty() {
        crate::log!("{}", joined(app));
    }
    crate::log!("{}", joined(crate::report::controllers()));
}

/// Hands the store's app that this launch opened over, as the settings say (`HandOver`), once the
/// game has run for `Behavior::hand_over_after`: steambro closes it, so that Steam stops counting
/// it as part of the game, and with `Reopen` opens it again outside Steam's launch. The game keeps
/// Steam's overlay and controller, which live in its own process. Gives up when the game ends
/// first; then the app closes after the game, as without a handover.
fn hand_over_app(game: &Game, app: &str, behavior: &Behavior, windows: &Mutex<GameWindows>, ended: &AtomicBool, handed_over: &AtomicBool) {
    // Closing an app that closes itself raced with it: a command to it could start a new one, and
    // one opened again right after hung on its login screen (Battle.net, 2026-11-08).
    if game.store.app_closes_itself() {
        crate::log!("{app} closes by itself once the game starts, as its own settings say, so steambro neither closes it nor opens it again");
        return;
    }
    let mut running_since: Option<Instant> = None;
    loop {
        if ended.load(Ordering::Relaxed) {
            return;
        }
        let running = !windows.lock().unwrap().pids.is_empty();
        running_since = match (running, running_since) {
            (true, None) => Some(Instant::now()),
            (true, since) => since,
            (false, _) => None,
        };
        if running_since.is_some_and(|since| since.elapsed() >= behavior.hand_over_after) {
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    crate::log!("the game has run for {}s; closing {app}, so Steam stops counting it as part of the game", behavior.hand_over_after.as_secs());
    match game.store.close_app(game) {
        Ok(()) => crate::log!("{app} has closed"),
        Err(e) => crate::log!("{app} didn't close completely: {e}"),
    }
    if game.store.app_running() {
        crate::log!("{app} still runs, so it closes after the game as usual");
        return;
    }
    handed_over.store(true, Ordering::Relaxed);
    if behavior.knobs.hand_over != HandOver::Reopen {
        return;
    }
    match game.store.reopen_app(game) {
        Ok(pid) => {
            crate::log!("opened {app} again outside Steam (process {pid})");
            Reopened::remember(game.store.id(), pid);
            let name = crate::process::list().into_iter().find(|p| p.pid == pid).map(|p| p.name);
            if let Some(name) = name
                && crate::window::keep_behind(&name, Duration::from_secs(15))
            {
                crate::log!("minimized {app}'s window, which came in front of the game");
            }
        }
        Err(e) => crate::log!("couldn't open {app} again: {e}"),
    }
}

/// The store apps that a launch opened again outside Steam (`HandOver::Reopen`), by store ID, in
/// `%LOCALAPPDATA%\steambro\reopened.json`. At the next launch of that store's game, steambro
/// restarts such an app inside Steam's launch without asking: it opened the app itself.
struct Reopened;

impl Reopened {
    fn path() -> Option<PathBuf> {
        crate::data_dir().ok().map(|d| d.join("reopened.json"))
    }

    fn load() -> BTreeMap<String, (u32, String)> {
        Self::path().and_then(|p| std::fs::read(p).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    fn remember(store: &str, pid: u32) {
        let Some(name) = crate::process::list().into_iter().find(|p| p.pid == pid).map(|p| p.name) else { return };
        let mut all = Self::load();
        all.insert(store.to_string(), (pid, name));
        if let Some(path) = Self::path() {
            let _ = std::fs::write(path, serde_json::to_vec(&all).unwrap_or_default());
        }
    }

    /// The store's app that runs now is the one a launch opened again: the same process, by its
    /// ID and its exe's name.
    fn runs(store: &str) -> bool {
        let Some((pid, name)) = Self::load().remove(store) else { return false };
        crate::process::list().iter().any(|p| p.pid == pid && p.name.eq_ignore_ascii_case(&name))
    }
}

/// Steam ends `steambro launch` and the processes it sees of the game, but not always all of them:
/// StarCraft II kept running when Steam ended Battle.net and the launch. And it sees no game that
/// Windows starts, like an Xbox game. So the launch's job ends the game with the launch.
fn exit_job(behavior: &Behavior) -> Option<Job> {
    if !behavior.knobs.exit_ends_game {
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
    fn start(reason: &str, behavior: &Behavior, windows: &Arc<Mutex<GameWindows>>) -> Self {
        crate::log!("steambro stands in for Steam: {reason}");
        let bridge = if behavior.knobs.layout {
            Bridge::start(behavior.knobs.bridge, windows.clone())
        } else {
            crate::log!("Steam keeps its desktop layout during the game, as the settings say");
            None
        };
        let overlay = if !behavior.knobs.overlay {
            crate::log!("no Steam overlay over the game, as the settings say");
            None
        } else {
            match steam_overlay() {
                Ok(key) => Some(Overlay::start(windows.clone(), if behavior.knobs.overlay_keys { key } else { None })),
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
fn restart_app(game: &Game, app: &str, behavior: &Behavior, settings: &Settings) {
    if let Some(reason) = game.store.app_busy() {
        crate::log!("{app} runs outside Steam, but {reason}, so it stays");
        return;
    }
    let reopened = behavior.knobs.hand_over == HandOver::Reopen && Reopened::runs(game.store.id());
    let restart = match behavior.knobs.app_running {
        _ if reopened => {
            crate::log!("{app} runs outside Steam because steambro opened it again after a game; restarting it inside Steam's launch");
            true
        }
        AppRunning::Restart => true,
        AppRunning::Leave => false,
        AppRunning::Ask => {
            crate::log!("{app} runs outside Steam; asking whether to restart it");
            let request = prompt::Request {
                store: game.store.id().to_string(),
                store_name: game.store.name().to_string(),
                app: app.to_string(),
                game: game.name().to_string(),
                controller: crate::controllers::any_connected(),
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

/// How long the game's processes may run on after its last window closed, once it was played,
/// before the launch ends them. Rocket League hung so after Steam's Exit game (2026-11-10): Steam
/// closed its window, the game's process ran on without one, and Steam showed the game as running
/// for 14 minutes, until Steam restarted. A game that saves on its way out has this long; Rocket
/// League itself took 9 to 21 s to end after its window closed.
const HUNG_AFTER: Duration = Duration::from_secs(45);
/// How long the game's windows had the front, in all, before it counts as played: longer than a
/// splash window has it.
const PLAYED_FOR: Duration = Duration::from_secs(30);

/// Waits until a process of the game has run and then none has run for the grace time. Looks for
/// new processes at each check, and learns at once when one ends. Keeps the processes in
/// `windows` up to date, and adds each process to `job`, so that it ends when Steam ends the
/// launch. Follows the launch's `family` too. A game whose processes outlive its windows ends
/// (`HUNG_AFTER`).
fn wait_for_game(
    started: &Started,
    behavior: &Behavior,
    windows: &Mutex<GameWindows>,
    mut job: Option<&mut Job>,
    family: &mut Family,
    (app_exes, handed_over): (&[&str], &AtomicBool),
    watch_hung: bool,
) -> crate::Result<()> {
    let begun = Instant::now();
    let mut nudged = started.nudge.is_none();
    let mut last_seen: Option<Instant> = None;
    // The game's process the last time one ran, to log when it changes.
    let mut running: Option<PathBuf> = None;
    let mut exits = Exits::default();
    let mut hung = Hung::default();
    loop {
        let snapshot = crate::process::list();
        // The store's app that this launch opened, whatever started the process that runs now.
        // Not after the handover: an app that runs then was opened again outside Steam's launch,
        // by steambro or by the user, and isn't the launch's.
        if !handed_over.load(Ordering::Relaxed) {
            for process in snapshot.iter().filter(|p| app_exes.iter().any(|exe| p.name.eq_ignore_ascii_case(exe))) {
                family.take_in(process);
            }
        }
        family.update(&snapshot);
        let mut found = game_processes(started, &snapshot);
        let alive = exits.follow(found.iter().map(|(pid, _)| *pid));
        found.retain(|(pid, _)| alive.contains(pid));
        {
            let mut windows = windows.lock().unwrap();
            windows.pids = found.iter().map(|(pid, _)| *pid).collect();
            windows.family = family.pids();
        }
        if let Some(job) = job.as_deref_mut() {
            for (pid, path) in &found {
                match job.add(*pid) {
                    Ok(true) => crate::log!("{} ends if Steam ends the game", path.display()),
                    Ok(false) => {}
                    Err(e) => crate::log!("Steam can't end {}: {e}", path.display()),
                }
            }
            for pid in family.pids() {
                let name = || snapshot.iter().find(|p| p.pid == pid).map_or_else(|| pid.to_string(), |p| format!("{} ({pid})", p.name));
                match job.adopt(pid) {
                    Ok(true) => crate::log!("{}, which this launch started, had left the launch's job; it ends with the launch again", name()),
                    Ok(false) => {}
                    Err(e) => crate::log!("{}, which this launch started, left the launch's job and can't join it again, so it outlives a launch that Steam ends: {e}", name()),
                }
            }
        }
        if watch_hung && hung.check(&found, &family.pids()) {
            end_hung(&found, job.as_deref());
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
        // During the grace time, the next look is the one that ends it.
        match last_seen {
            Some(seen) if running.is_none() => std::thread::sleep(behavior.check.min(behavior.end_grace.saturating_sub(seen.elapsed())).max(Duration::from_millis(10))),
            _ => exits.wait(behavior.check),
        }
    }
}

/// Whether the game hangs on its way out: it was played, and then its processes had no window for
/// `HUNG_AFTER` while the user was back in Steam or on the desktop. Played means its windows had
/// the front for `PLAYED_FOR` since its last new process, so neither a splash window nor a
/// launcher of the game's own before a game that loads without a window counts. A window of a
/// program that the launch started counts too, like a game that a launcher in the game's folder
/// started from another folder. The launch doesn't look at all for a game whose windows may hide
/// on purpose (`Store::windows_may_hide`). A window owned by Edge or Windows for a packaged app
/// isn't the game's process's, so such an app never counts as played.
#[derive(Default)]
struct Hung {
    seen: HashSet<u32>,
    front_for: Duration,
    last_look: Option<Instant>,
    windowless_since: Option<Instant>,
    told: bool,
}

impl Hung {
    /// True once, when the game is hung. `family` are the launch's processes.
    fn check(&mut self, found: &[(u32, PathBuf)], family: &[u32]) -> bool {
        let pids: Vec<u32> = found.iter().map(|(pid, _)| *pid).collect();
        let in_front = pids.contains(&crate::window::front_pid());
        let away_without_windows = || {
            let mut all = pids.clone();
            all.extend(family.iter().copied().filter(|&pid| pid != std::process::id()));
            crate::window::steam_or_desktop_in_front() && crate::window::with_windows(&all).is_empty()
        };
        self.step(Instant::now(), &pids, in_front, away_without_windows)
    }

    /// The rule, for one look at `now`: the game's processes, whether one of its windows has the
    /// front, and whether Steam, the desktop or nothing is in front while none of the launch's
    /// processes has a window (asked only once the game was played).
    fn step(&mut self, now: Instant, pids: &[u32], in_front: bool, away_without_windows: impl FnOnce() -> bool) -> bool {
        let since_last = self.last_look.replace(now).map_or(Duration::ZERO, |last| now - last);
        if pids.iter().any(|pid| !self.seen.contains(pid)) {
            self.seen.extend(pids);
            (self.front_for, self.windowless_since) = (Duration::ZERO, None);
            return false;
        }
        if pids.is_empty() {
            return false;
        }
        if in_front {
            self.front_for += since_last;
        }
        if self.front_for < PLAYED_FOR || !away_without_windows() {
            self.windowless_since = None;
            return false;
        }
        let hung = now - *self.windowless_since.get_or_insert(now) >= HUNG_AFTER && !self.told;
        self.told |= hung;
        hung
    }
}

/// Ends a game that hangs on its way out, as Steam's Exit game would have, unless the settings keep
/// the game running when Steam ends steambro. A process in the launch's job that Windows doesn't
/// let steambro end alone ends with the whole launch.
fn end_hung(found: &[(u32, PathBuf)], job: Option<&Job>) {
    let names: Vec<String> = found.iter().map(|(pid, path)| format!("{} ({pid})", path.file_name().unwrap_or_default().to_string_lossy())).collect();
    let Some(job) = job else {
        crate::log!("the game's windows closed {}s ago, but {} still runs; it keeps running, as the settings say", HUNG_AFTER.as_secs(), names.join(", "));
        return;
    };
    crate::log!("the game's windows closed {}s ago, but {} still runs; ending it", HUNG_AFTER.as_secs(), names.join(", "));
    let pids: Vec<u32> = found.iter().map(|(pid, _)| *pid).collect();
    let left = job.end(&pids);
    if left.is_empty() {
        return;
    }
    let list = left.iter().map(u32::to_string).collect::<Vec<_>>().join(", ");
    if left.iter().any(|&pid| job.holds(pid)) {
        crate::log!("Windows didn't let steambro end {list}; ending the launch, so that Windows ends the game with it");
        job.end_launch();
    }
    crate::log!("Windows didn't let steambro end {list}, which isn't in the launch's job; Steam shows the game as running until it ends");
}

/// The running processes of the game, with their executables: inside its folders, or with the
/// identity of one of its packages.
fn game_processes(started: &Started, running: &[crate::process::Process]) -> Vec<(u32, PathBuf)> {
    running
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Looks once a second for `seconds`, with the game's window in front or not, and Steam in
    /// front without a window of the game or not; returns the seconds after which the game was hung.
    fn looks(hung: &mut Hung, at: &mut Instant, seconds: u64, pids: &[u32], in_front: bool, away: bool) -> Option<u64> {
        (1..=seconds).find(|_| {
            *at += Duration::from_secs(1);
            hung.step(*at, pids, in_front, || away)
        })
    }

    #[test]
    fn a_played_game_without_windows_is_hung_once() {
        let (mut hung, mut at) = (Hung::default(), Instant::now());
        assert!(!hung.step(at, &[7], false, || true), "a new process isn't hung");
        assert_eq!(looks(&mut hung, &mut at, 120, &[7], false, true), None, "a game that never had the front isn't hung");
        assert_eq!(looks(&mut hung, &mut at, 29, &[7], true, false), None);
        assert_eq!(looks(&mut hung, &mut at, 120, &[7], false, false), None, "the user is in another program, or the game has a window");
        assert_eq!(looks(&mut hung, &mut at, 2, &[7], true, false), None, "played for 30 s by now");
        assert_eq!(looks(&mut hung, &mut at, 44, &[7], false, true), None);
        assert_eq!(looks(&mut hung, &mut at, 5, &[7], false, false), None, "a window came back, so the time starts again");
        assert_eq!(looks(&mut hung, &mut at, 120, &[7], false, true), Some(46), "45 s after the first look without a window");
        assert_eq!(looks(&mut hung, &mut at, 120, &[7], false, true), None, "said once");
    }

    #[test]
    fn a_new_process_of_the_game_starts_the_count_again() {
        let (mut hung, mut at) = (Hung::default(), Instant::now());
        hung.step(at, &[7], false, || false);
        assert_eq!(looks(&mut hung, &mut at, 40, &[7], true, false), None);
        // The game's launcher gave way to the game, which loads without a window.
        assert_eq!(looks(&mut hung, &mut at, 300, &[7, 8], false, true), None, "the new process hasn't been played");
        assert_eq!(looks(&mut hung, &mut at, 300, &[], false, true), None, "no process, nothing to end");
    }
}

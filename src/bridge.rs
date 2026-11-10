//! The Steam Input bridge, for games that Steam doesn't see because Windows starts them (Xbox
//! games).
//!
//! `steambro launch` runs inside Steam's launch, so Steam's hook in it shows Steam Input's virtual
//! controllers through XInput. The bridge makes Steam use the game's own layout while the game has
//! a window, whatever window is in front (`steam://forceinputappid`), and copies each of Steam's virtual controllers to a
//! virtual Xbox 360 controller from the ViGEmBus driver, which the game reads like a real one.
//!
//! The launch owns the virtual controllers: they go in before the game starts, each of Steam's
//! controllers drives one with a saved number, which Windows sees as the same device every time,
//! and a helper change doesn't unplug them. A process only gets the virtual controllers of Steam's that existed when it started, so
//! a helper process (`steambro bridge-helper`) reads Steam's controllers and sends their states to
//! the launch through a pipe, and the launch starts a new helper whenever Steam's list changes.
//! While no window of the game is in front, the virtual controllers rest, so input meant for
//! another program doesn't reach the game too. The game's rumble goes the other way: ViGEmBus
//! reports it for each virtual controller, and the helper passes it to Steam's controller.
//!
//! A real Xbox controller reaches the game by itself and isn't copied. While one is connected, no
//! virtual controller stands in for a controller of Steam's that isn't there yet (`stand_in_pad`):
//! an idle one would sit beside the real controller as a second player.

use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Write};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use vigem_client::{Client, TargetId, XButtons, XGamepad, Xbox360Wired};
use windows::Win32::Foundation::{CloseHandle, WAIT_TIMEOUT};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::System::Threading::{CREATE_NO_WINDOW, OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject};
use windows::Win32::UI::Input::XboxController::{XINPUT_GAMEPAD, XINPUT_STATE, XINPUT_VIBRATION, XInputGetState, XInputSetState};
use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

use crate::Worker;
use crate::controllers::{self, Kind};
use crate::steam::Steam;
use crate::window::{GameWindows, InFront};

/// How often `steambro launch` reads Steam's list of virtual controllers and looks whether the
/// game is in front.
const WATCH: Duration = Duration::from_millis(100);
/// How often the helper reads Steam's controllers: 250 times a second.
const COPY_PERIOD: Duration = Duration::from_millis(4);
/// How long the launch waits for the first virtual controller before it lets the game start.
const PLUG_IN_WAIT: Duration = Duration::from_secs(2);
/// One controller's state from the helper: the virtual controller's number, whether Steam's
/// controller is connected, and XInput's gamepad report (12 bytes).
const STATE_LEN: usize = 14;
/// One rumble from the launch: the virtual controller's number and the two motors' speeds.
const RUMBLE_LEN: usize = 3;
/// The vendor and product ID of steambro's virtual controllers: Microsoft's, so `Kind::of_vendor`
/// takes them for Xbox controllers, with a product ID of steambro's own, so that Steam's
/// controller blocklist can hide them from Steam without hiding real Xbox 360 controllers
/// (`Steam::ignore_controller`). Windows still loads its Xbox 360 driver (`xusb22`) for them, and
/// XInput and Windows.Gaming.Input list them as "Xbox 360 Controller for Windows".
pub const PAD_ID: (u16, u16) = (0x045E, 0x7EB0);

/// The bridge while the game runs. When dropped, it stops the helper, unplugs the virtual
/// controllers, and gives Steam its usual layouts back.
pub struct Bridge {
    _watcher: Worker,
}

impl Bridge {
    /// Starts the bridge when Steam launched steambro; `None` otherwise. The launch starts it only
    /// with the game's `layout` setting on, before it starts the game. `copy` is the game's `bridge`
    /// setting (plan.rs); without it, or without the driver, the bridge only makes Steam use the
    /// game's layout (`watch`). `game` is kept up to date by launch.rs.
    pub fn start(copy: bool, game: Arc<Mutex<GameWindows>>) -> Option<Bridge> {
        let Some(app_id) = steam_app_id() else {
            crate::log!("bridge: not started by Steam, so no Steam Input bridge");
            return None;
        };
        let pads = if !copy {
            crate::log!("bridge: copying is off for this game in the settings");
            None
        } else {
            match Client::connect() {
                Ok(client) => {
                    let mut numbers = Numbers::load();
                    let mut xbox_logged = false;
                    let plugged = first_pads(&mut numbers, &mut xbox_logged);
                    Some(Copying { pads: Pads::start(client, &plugged), numbers, plugged, xbox_logged })
                }
                Err(e) => {
                    crate::log!("bridge: ViGEmBus isn't available ({e:?}), so Steam's controllers can't be copied");
                    None
                }
            }
        };
        Some(Bridge { _watcher: Worker::spawn(move |stop| watch(app_id, pads, &game, stop)) })
    }
}

/// The copying of Steam's controllers to virtual ones, while the game's `bridge` setting is on and
/// the driver answers.
struct Copying {
    pads: Pads,
    /// The launch's own copy of the numbers; `Numbers::merge` takes the file's changes in.
    numbers: Numbers,
    /// The numbers of the virtual controllers plugged in now, in the helper's order.
    plugged: Vec<u32>,
    /// The last look for a real Xbox controller found one, and the log said so (`xbox_connected`).
    xbox_logged: bool,
}

/// The virtual controllers that go in before the game starts: one for each of Steam's controllers
/// to copy, so the game finds them when it looks. Without one, as when the controller is still
/// asleep, one waits for it, with the number of the controller used last, if the account has used
/// a kind that the bridge copies before and no real Xbox controller is connected
/// (`stand_in_pad`).
fn first_pads(numbers: &mut Numbers, xbox_logged: &mut bool) -> Vec<u32> {
    let to_copy: Vec<Slot> = steam_slots().into_iter().filter(|s| s.kind.bridged()).collect();
    if !to_copy.is_empty() {
        return identify(&to_copy, &mut HashMap::new()).iter().map(|id| numbers.of(id)).collect();
    }
    let input = Steam::locate().ok().and_then(|s| s.current_user().ok().flatten()).and_then(|u| u.input_settings().ok());
    let used_before = input.is_some_and(|i| controllers::seen_by_steam(&i.types_used).iter().any(|k| k.bridged()));
    // The Xbox controller is looked for only when it decides something: a stand-in was due.
    if !used_before || !stand_in_pad(false, used_before, xbox_connected(xbox_logged)) {
        return Vec::new();
    }
    vec![numbers.last.as_ref().and_then(|id| numbers.numbers.get(id)).copied().unwrap_or(1)]
}

/// Whether a virtual controller stands in for a controller of Steam's that isn't there to copy:
/// only when the account has used a kind that the bridge copies before, so one may still wake up,
/// and no real Xbox controller is connected. The game reads a real Xbox controller itself, so an
/// idle virtual controller beside it would only take a player's place.
fn stand_in_pad(bridged_present: bool, bridged_used_before: bool, xbox_connected: bool) -> bool {
    !bridged_present && bridged_used_before && !xbox_connected
}

/// A real Xbox controller is connected now. Called only when a stand-in is due, so the log says
/// that the controller blocks it; `logged` is whether the last look said so, so that the line
/// comes once while the controller stays connected.
fn xbox_connected(logged: &mut bool) -> bool {
    let connected = controllers::connected_kinds().contains(&Kind::Xbox);
    if connected && !*logged {
        crate::log!("bridge: a real Xbox controller is connected, so no virtual controller stands in");
    }
    *logged = connected;
    connected
}

/// The number of each virtual controller, by the name of the controller of Steam's that drives it
/// (`identify`), in `%LOCALAPPDATA%\steambro\controllers.json`. A virtual controller with the same
/// number is the same device in Windows (`Xbox360Wired::plugin_at`), so a game sees the same
/// controller for the same one of Steam's in every launch, and two controllers stay two players.
#[derive(Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Numbers {
    numbers: BTreeMap<String, u32>,
    /// The controller that drove the first virtual controller last, whose number waits for it.
    last: Option<String>,
}

impl Numbers {
    fn path() -> crate::Result<PathBuf> {
        Ok(crate::data_dir()?.join("controllers.json"))
    }

    /// The saved numbers; none when the file is missing or broken.
    fn load() -> Self {
        Self::path().ok().map(|path| Self::load_from(&path)).unwrap_or_default()
    }

    fn load_from(path: &Path) -> Self {
        std::fs::read_to_string(path).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
    }

    fn save(&self) -> crate::Result<()> {
        self.save_to(&Self::path()?)
    }

    /// Writes the file whole: a launch and the settings window write it, and the window reads it
    /// often, so a reader never sees a half-written file, which would read as no numbers at all.
    fn save_to(&self, path: &Path) -> crate::Result<()> {
        std::fs::create_dir_all(path.parent().expect("the data folder"))?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        // Within one folder, a rename replaces the file in one step.
        Ok(std::fs::rename(&tmp, path)?)
    }

    /// The file's numbers as they are now, with these controllers' numbers kept over them. A
    /// launch keeps its own copy while the game runs, and the settings window may forget
    /// controllers meanwhile (`forget_controllers`); the launch writes back only the controllers
    /// it drives now, whose virtual controllers stay the same devices, and leaves the others as
    /// the file has them. No other controller keeps a kept number.
    fn merge(mut file: Numbers, driving: &[(String, u32)]) -> Numbers {
        for (id, number) in driving {
            file.numbers.retain(|other, n| other == id || n != number);
            file.numbers.insert(id.clone(), *number);
        }
        file
    }

    /// The controller's number, or for a new one the lowest that no other controller has.
    fn of(&mut self, id: &str) -> u32 {
        if let Some(&number) = self.numbers.get(id) {
            return number;
        }
        let number = (1..).find(|n| !self.numbers.values().any(|v| v == n)).expect("a free number");
        self.numbers.insert(id.to_string(), number);
        number
    }

    /// Every controller with a number, sorted by number.
    fn known(&self) -> Vec<KnownController> {
        let mut known: Vec<KnownController> =
            self.numbers.iter().map(|(id, &number)| KnownController { id: id.clone(), number, last: self.last.as_deref() == Some(id) }).collect();
        known.sort_by_key(|c| c.number);
        known
    }

    /// Drops these controllers, so their numbers are free for new ones.
    fn forget(&mut self, ids: &[String]) {
        self.numbers.retain(|id, _| !ids.contains(id));
        if self.last.as_ref().is_some_and(|last| ids.contains(last)) {
            self.last = None;
        }
    }
}

/// A controller of Steam's that the bridge knows, for the settings window.
pub struct KnownController {
    /// Its name for the bridge, like `28de:1302#0` (`identify`).
    pub id: String,
    /// The number of the virtual controller it drives (`Numbers`).
    pub number: u32,
    /// It drove the first virtual controller in the last launch, so its number waits for it.
    pub last: bool,
}

/// The controllers the bridge knows, from `controllers.json`, sorted by number.
pub fn known_controllers() -> Vec<KnownController> {
    Numbers::load().known()
}

/// Forgets the controllers with these IDs, so their numbers are free for new controllers; the file
/// stays, empty if they were all. An ID the bridge doesn't know is nothing to forget.
pub fn forget_controllers(ids: &[String]) -> crate::Result<()> {
    let mut numbers = Numbers::load();
    numbers.forget(ids);
    numbers.save()
}

/// Names each of Steam's controllers for the bridge: its model, by vendor and product ID, and its
/// place among the controllers of that model, like `28de:1302#0` and `28de:1302#1` for two Steam
/// Controllers. Steam gives no serial number, so two controllers of one model are told apart by the
/// order they come in. A controller keeps its name while Steam lists it (`known`, by Steam's
/// handle), so one leaving doesn't hand its name to another.
fn identify(slots: &[Slot], known: &mut HashMap<String, String>) -> Vec<String> {
    let handle = |slot: &Slot| if slot.handle.is_empty() { format!("slot {}", slot.index) } else { slot.handle.clone() };
    known.retain(|h, _| slots.iter().any(|s| handle(s) == *h));
    let mut ids = Vec::new();
    for slot in slots {
        let id = match known.get(&handle(slot)) {
            Some(id) => id.clone(),
            None => {
                let model = format!("{}:{}", slot.vid.trim_start_matches("0x"), slot.pid.trim_start_matches("0x"));
                let id = (0..).map(|n| format!("{model}#{n}")).find(|id| !known.values().any(|v| v == id)).expect("a free name");
                known.insert(handle(slot), id.clone());
                id
            }
        };
        ids.push(id);
    }
    ids
}

/// Follows Steam's list of virtual controllers and the window in front until `stop`. Steam is made
/// to use the game's layout once it has a controller: without that, Steam's desktop layout would
/// turn the controller into mouse and keyboard on top of the game, and an Xbox controller, which
/// the game reads itself, would give its input twice. With `copying`, each controller that
/// `Kind::bridged` allows is copied, to the virtual controller with its number (`Numbers`), the
/// first plugged in before the game starts (`plugged`). A controller connected during the game gets
/// its own virtual controller then, which the game picks up like a controller plugged in; one
/// disconnected takes its virtual controller with it, except the last, which rests so the game
/// keeps a controller, unless a real Xbox controller is connected: then the game keeps that one
/// (`stand_in_pad`).
fn watch(app_id: u32, mut copying: Option<Copying>, game: &Mutex<GameWindows>, stop: &AtomicBool) {
    // COM reads a window's app ID (InFront).
    let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    // The layout Steam is made to use now; none while Steam picks it itself.
    let mut forced: Option<u32> = None;
    let mut controller_seen = false;
    // Whether the game has a window, looked at once in a while when it isn't in front.
    let mut has_window = false;
    let mut window_looked: Option<Instant> = None;
    let mut helper: Option<Helper> = None;
    let mut copied: Option<Vec<Slot>> = None;
    let mut known = HashMap::new();
    let mut in_front = InFront::default();
    let mut front = false;
    while !stop.load(Ordering::Relaxed) {
        let slots = steam_slots();
        controller_seen |= !slots.is_empty();
        let windows = game.lock().unwrap().clone();
        let game_in_front = in_front.game(&windows, unsafe { GetForegroundWindow() });
        if game_in_front {
            (has_window, window_looked) = (true, Some(Instant::now()));
        } else if window_looked.is_none_or(|at| at.elapsed() >= WINDOW_LOOK) {
            (has_window, window_looked) = (windows.main_window().is_some(), Some(Instant::now()));
        }
        let wanted = if !controller_seen {
            None
        } else if crate::overlay::is_up() {
            Some(STEAM_MENUS_APP)
        } else if game_in_front || (has_window && crate::window::steam_window_in_front().is_none()) {
            Some(app_id)
        } else {
            None
        };
        if wanted != forced {
            set_layout(app_id, forced, wanted);
            forced = wanted;
        }
        if let Some(Copying { pads, numbers, plugged, xbox_logged }) = copying.as_mut() {
            let to_copy: Vec<Slot> = slots.into_iter().filter(|s| s.kind.bridged()).collect();
            if copied.as_ref() != Some(&to_copy) {
                crate::log!("bridge: Steam's controllers to copy: {}", describe(&to_copy));
                if let Some(mut old) = helper.take() {
                    old.stop(pads);
                }
                let ids = identify(&to_copy, &mut known);
                // The settings window may have forgotten controllers since the last save.
                let driving: Vec<(String, u32)> = known.values().filter_map(|id| numbers.numbers.get(id).map(|n| (id.clone(), *n))).collect();
                *numbers = Numbers::merge(Numbers::load(), &driving);
                let mut wanted: Vec<u32> = ids.iter().map(|id| numbers.of(id)).collect();
                for (id, number) in ids.iter().zip(&wanted) {
                    crate::log!("bridge: {id} drives virtual controller {number:02}");
                }
                if let Some(first) = ids.first() {
                    numbers.last = Some(first.clone());
                }
                if let Err(e) = numbers.save() {
                    crate::log!("bridge: couldn't save the virtual controllers' numbers: {e}");
                }
                // A plugged virtual controller means a kind the bridge copies was used before.
                if wanted.is_empty() && !plugged.is_empty() && stand_in_pad(false, true, xbox_connected(xbox_logged)) {
                    wanted = plugged.first().copied().into_iter().collect();
                }
                pads.plug_in(&wanted);
                helper = if to_copy.is_empty() { None } else { Helper::start(&to_copy, &wanted, pads) };
                *plugged = wanted;
                copied = Some(to_copy);
            } else if let Some(code) = helper.as_mut().and_then(Helper::ended) {
                crate::log!("bridge: the helper ended with code {code}; trying again when Steam's controllers change");
                helper = None;
            }
            if game_in_front != front {
                front = game_in_front;
                crate::log!("bridge: {}", if front { "the game is in front; Steam's controllers reach it" } else { "the game isn't in front; its controllers rest" });
                pads.send(Message::Front(front));
            }
        }
        std::thread::sleep(WATCH);
    }
    if let Some(Copying { pads, .. }) = copying {
        if let Some(mut helper) = helper {
            helper.stop(&pads);
        }
        pads.close();
    }
    if forced.is_some() {
        match give_back_layout() {
            Ok(()) => crate::log!("bridge: released the layout of app {app_id}"),
            Err(e) => crate::log!("bridge: couldn't release the layout: {e}"),
        }
        unmark();
    }
}

/// The app ID Steam gave the launch: the game's shortcut. `None` when Steam didn't start steambro.
pub fn steam_app_id() -> Option<u32> {
    std::env::var("SteamAppId").ok().and_then(|v| v.parse::<u32>().ok()).filter(|id| *id != 0)
}

/// Steam's own layout for its menus (`basicui`), which Steam switches to when its overlay opens over a
/// game it started, and uses for its own windows, like Big Picture (its log:
/// "k_nGameIDControllerConfigs_ClientUI, AppID 769").
const STEAM_MENUS_APP: u32 = 769;
/// Steam's desktop layout, which Steam uses for a window of a program it didn't start (its log:
/// "k_nGameIDControllerConfigs_Desktop, AppID 413080").
const STEAM_DESKTOP_APP: u32 = 413080;
/// How often the bridge looks whether the game has a window while none of its windows is in front.
const WINDOW_LOOK: Duration = Duration::from_secs(1);

/// Changes the layout steambro makes Steam use, `from` one to `to` another, and keeps the marker for
/// the tray app, which gives Steam its layouts back if this process is killed (`release_orphans`).
/// - The game's, while the game is in front, or has a window and no window of Steam's is in front:
///   Steam can't see the game's windows, so it would use its desktop layout and turn the
///   controller into mouse and keyboard on top of the game.
/// - Steam's layout for its menus while Steam's overlay is open over the game, so that the
///   controller moves through them; the game's would replace it the moment the overlay opens, and
///   only B would reach the overlay.
/// - None while the game has no window, as before it shows and after it closed, or while a window
///   of Steam's is in front, so that Big Picture answers the controller.
fn set_layout(app_id: u32, from: Option<u32>, to: Option<u32>) {
    let Some(id) = to else {
        match give_back_layout() {
            Ok(()) => crate::log!("bridge: the game has no window or Steam is in front; Steam picks the layout again"),
            Err(e) => crate::log!("bridge: couldn't give Steam its layouts back: {e}"),
        }
        unmark();
        return;
    };
    // A steam:// URL starts Steam when it doesn't run.
    if !Steam::is_running() {
        return;
    }
    match Steam::locate().and_then(|steam| steam.open_url_in_order(&format!("steam://forceinputappid/{id}"))) {
        Ok(()) if id == STEAM_MENUS_APP => crate::log!("bridge: Steam's overlay is open; Steam uses its layout for its menus"),
        Ok(()) if from == Some(STEAM_MENUS_APP) => crate::log!("bridge: Steam's overlay closed; Steam uses the layout of app {app_id} again"),
        Ok(()) => crate::log!("bridge: Steam uses the layout of app {app_id} while the game has a window"),
        Err(e) => crate::log!("bridge: couldn't make Steam use the layout of app {id}: {e}"),
    }
    if let Ok(path) = marker_path(std::process::id()) {
        let _ = std::fs::create_dir_all(path.parent().expect("a marker has a folder"));
        let _ = std::fs::write(path, id.to_string());
    }
}

fn unmark() {
    if let Ok(path) = marker_path(std::process::id()) {
        let _ = std::fs::remove_file(path);
    }
}

/// Gives Steam back the choice of layout. `forceinputappid/0` alone isn't enough: Steam picks a
/// layout again only when the window in front changes or a layout is forced, so the window that
/// came to the front while a layout was forced, like Big Picture when the game closed, would keep
/// that layout and not answer the controller. So steambro first forces the layout Steam picks for
/// the window in front itself, its menus' for Big Picture and the desktop's for another window,
/// and then none. While another launch forces its game's layout, Steam gets that one again. A
/// Steam that doesn't run has no forced layout: nothing is sent, which would start it.
fn give_back_layout() -> crate::Result<()> {
    if !Steam::is_running() {
        return Ok(());
    }
    let steam = Steam::locate()?;
    if let Some(id) = other_launchs_layout() {
        return steam.open_url_in_order(&format!("steam://forceinputappid/{id}"));
    }
    let fitting = match crate::window::steam_window_in_front() {
        Some(crate::window::SteamWindow::BigPicture) => STEAM_MENUS_APP,
        _ => STEAM_DESKTOP_APP,
    };
    steam.open_url_in_order(&format!("steam://forceinputappid/{fitting}"))?;
    steam.open_url_in_order("steam://forceinputappid/0")
}

/// The layout another launch that still runs forces, from its marker.
fn other_launchs_layout() -> Option<u32> {
    let dir = crate::data_dir().ok()?.join("bridges");
    let running: Vec<u32> = crate::process::list().into_iter().filter(|p| p.name.eq_ignore_ascii_case("steambro.exe")).map(|p| p.pid).collect();
    std::fs::read_dir(dir).ok()?.flatten().find_map(|entry| {
        let pid = entry.file_name().to_str()?.parse::<u32>().ok()?;
        (pid != std::process::id() && running.contains(&pid)).then(|| std::fs::read_to_string(entry.path()).ok()?.trim().parse::<u32>().ok())?
    })
}

/// `bridges\<pid>`: a launch with that process ID forces the layout of the app ID in the file: its
/// game's, or Steam's layout for its menus while Steam's overlay is open over the game. None while
/// it forces none.
fn marker_path(pid: u32) -> crate::Result<PathBuf> {
    Ok(crate::data_dir()?.join("bridges").join(pid.to_string()))
}

/// For the tray app: gives Steam its layouts back for launches that ended without doing it. Steam's
/// Exit game and Stop always end `steambro launch` that way, without a chance to clean up. When
/// another launch still forces a layout, Steam gets that one again.
pub fn release_orphans() {
    let Ok(dir) = crate::data_dir().map(|d| d.join("bridges")) else { return };
    let Ok(entries) = std::fs::read_dir(&dir) else { return };
    let running: Vec<u32> = crate::process::list().into_iter().filter(|p| p.name.eq_ignore_ascii_case("steambro.exe")).map(|p| p.pid).collect();
    let mut released = false;
    for entry in entries.flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else { continue };
        if !running.contains(&pid) {
            let app_id = std::fs::read_to_string(entry.path()).ok().and_then(|t| t.trim().parse::<u32>().ok());
            crate::log!("bridge: launch {pid} ended without releasing the layout of app {}", app_id.unwrap_or_default());
            let _ = std::fs::remove_file(entry.path());
            released = true;
        }
    }
    if released && let Err(e) = give_back_layout() {
        crate::log!("bridge: couldn't give Steam its layouts back: {e}");
    }
}

/// What the virtual controllers' thread hears from the rest of the launch.
enum Message {
    /// Have the virtual controllers with these numbers, in this order, which is the helper's.
    PlugIn(Vec<u32>, SyncSender<()>),
    /// From now on, states come from the helper with this number; until its first, every
    /// controller rests.
    Helper(u32),
    /// A state from a helper: the virtual controller's place in the order and the gamepad, or
    /// `None` while Steam's controller isn't connected.
    State { helper: u32, pad: usize, gamepad: Option<XGamepad> },
    /// Whether a window of the game is in front.
    Front(bool),
    /// The game has ended: unplug every controller.
    End,
}

/// The current helper, as the game's rumble needs it.
#[derive(Default)]
struct Rumble {
    /// The helper's input; `None` without a helper.
    helper: Option<ChildStdin>,
    /// The numbers of the virtual controllers, in the order of the helper's slots.
    order: Vec<u32>,
}

/// The virtual Xbox 360 controllers, on a thread of their own that owns them.
struct Pads {
    inbox: Sender<Message>,
    thread: JoinHandle<()>,
    rumble: Arc<Mutex<Rumble>>,
    helpers: u32,
}

impl Pads {
    /// Starts the thread and waits until the controllers with these numbers are plugged in, at
    /// most `PLUG_IN_WAIT`, so the game finds them when it starts.
    fn start(client: Client, numbers: &[u32]) -> Pads {
        let (inbox, messages) = std::sync::mpsc::channel();
        let rumble = Arc::new(Mutex::new(Rumble::default()));
        let thread = std::thread::spawn({
            let rumble = rumble.clone();
            move || drive(&client, &messages, &rumble)
        });
        let pads = Pads { inbox, thread, rumble, helpers: 0 };
        pads.plug_in(numbers);
        pads
    }

    fn send(&self, message: Message) {
        let _ = self.inbox.send(message);
    }

    /// Plugs in the virtual controllers with these numbers and unplugs the others, and waits until
    /// that's done, at most `PLUG_IN_WAIT`.
    fn plug_in(&self, numbers: &[u32]) {
        let (done, wait) = std::sync::mpsc::sync_channel(1);
        self.send(Message::PlugIn(numbers.to_vec(), done));
        let _ = wait.recv_timeout(PLUG_IN_WAIT);
    }

    /// The next helper's number; the thread takes states only from that one from now on.
    fn next_helper(&mut self) -> u32 {
        self.helpers += 1;
        self.send(Message::Helper(self.helpers));
        self.helpers
    }

    /// Unplugs the controllers and waits until they're gone.
    fn close(self) {
        self.send(Message::End);
        let _ = self.thread.join();
    }
}

/// One virtual controller.
struct Pad<'a> {
    /// Its number (`Numbers`).
    number: u32,
    target: Xbox360Wired<&'a Client>,
    /// What Steam's controller gives, from the helper; `None` while it isn't connected.
    from_steam: Option<XGamepad>,
    /// What the game has from this controller now.
    sent: XGamepad,
}

/// The virtual controllers' thread: plugs them in and out, gives each the state of its Steam
/// controller while the game is in front and a resting state otherwise, and passes the game's
/// rumble to the helper. Ends at `Message::End`, which unplugs the controllers.
fn drive(client: &Client, messages: &Receiver<Message>, rumble: &Arc<Mutex<Rumble>>) {
    let mut pads: Vec<Pad> = Vec::new();
    // The numbers in the helper's order.
    let mut order: Vec<u32> = Vec::new();
    let mut helper = 0;
    let mut front = false;
    for message in messages {
        match message {
            Message::PlugIn(numbers, done) => {
                // Unplugged by dropping.
                pads.retain(|pad| numbers.contains(&pad.number));
                for &number in &numbers {
                    if pads.iter().any(|pad| pad.number == number) {
                        continue;
                    }
                    match plug_in(client, number, rumble) {
                        Ok(pad) => pads.push(pad),
                        Err(e) => crate::log!("bridge: couldn't create virtual controller {number:02}: {e}"),
                    }
                }
                order = numbers;
                let _ = done.send(());
            }
            Message::Helper(id) => {
                helper = id;
                pads.iter_mut().for_each(|pad| pad.from_steam = None);
            }
            Message::State { helper: from, pad, gamepad } => {
                if from != helper {
                    continue;
                }
                if let Some(pad) = order.get(pad).and_then(|&number| pads.iter_mut().find(|p| p.number == number)) {
                    pad.from_steam = gamepad;
                }
            }
            Message::Front(now) => front = now,
            Message::End => return,
        }
        for pad in &mut pads {
            let gamepad = if front { pad.from_steam.unwrap_or_default() } else { XGamepad::default() };
            if gamepad != pad.sent {
                pad.sent = gamepad;
                if let Err(e) = pad.target.update(&gamepad) {
                    crate::log!("bridge: couldn't update the virtual controller: {e:?}");
                }
            }
        }
    }
}

/// Plugs in the virtual controller with this number as its serial number, so that Windows gives it
/// the same device as before (`USB\VID_045E&PID_7EB0\<number>`). When another program's virtual
/// controller has the number, it gets the next free one, as a new device.
fn plug_in<'a>(client: &'a Client, number: u32, rumble: &Arc<Mutex<Rumble>>) -> crate::Result<Pad<'a>> {
    let mut target = Xbox360Wired::new(client, TargetId { vendor: PAD_ID.0, product: PAD_ID.1 });
    if let Err(e) = target.plugin_at(number) {
        crate::log!("bridge: number {number:02} is taken ({e:?}); virtual controller {number:02} gets another for this game");
        target.plugin().map_err(|e| format!("{e:?}"))?;
    }
    target.wait_ready().map_err(|e| format!("{e:?}"))?;
    crate::log!("bridge: virtual Xbox 360 controller {number:02} is plugged in");
    pass_rumble(&mut target, number, rumble.clone());
    Ok(Pad { number, target, from_steam: None, sent: XGamepad::default() })
}

/// Passes the rumble that the game asks of the virtual controller to the helper, on a thread that
/// ends when the virtual controller is unplugged. The helper gives it to Steam's controller, and
/// Steam's hook takes it to the real controller; Steam's own rumble test on the virtual controller
/// reaches it the same way.
fn pass_rumble(target: &mut Xbox360Wired<&Client>, number: u32, rumble: Arc<Mutex<Rumble>>) {
    let notifications = match target.request_notification() {
        Ok(notifications) => notifications,
        Err(e) => {
            crate::log!("bridge: no rumble for virtual controller {number:02}: {e:?}");
            return;
        }
    };
    notifications.spawn_thread(move |_, motors| {
        let mut rumble = rumble.lock().unwrap();
        let Some(pad) = rumble.order.iter().position(|&n| n == number) else { return };
        if let Some(helper) = rumble.helper.as_mut() {
            let _ = helper.write_all(&[pad as u8, motors.large_motor, motors.small_motor]).and_then(|()| helper.flush());
        }
    });
}

/// A running `steambro bridge-helper`.
struct Helper {
    child: Child,
}

impl Helper {
    /// Starts a helper for these Steam controllers, which feed the virtual controllers in their
    /// order, the virtual controllers with `numbers`, and a thread that passes the helper's states
    /// on.
    fn start(slots: &[Slot], numbers: &[u32], pads: &mut Pads) -> Option<Helper> {
        let id = pads.next_helper();
        let exe = std::env::current_exe().ok()?;
        let indices = slots.iter().map(|s| s.index.to_string()).collect::<Vec<_>>().join(",");
        let spawned = Command::new(exe)
            .args(["bridge-helper", "--parent", &std::process::id().to_string(), "--slots", &indices])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW.0)
            .spawn();
        let mut child = match spawned {
            Ok(child) => child,
            Err(e) => {
                crate::log!("bridge: couldn't start the helper: {e}");
                return None;
            }
        };
        if let Some(states) = child.stdout.take() {
            let inbox = pads.inbox.clone();
            std::thread::spawn(move || read_states(states, &inbox, id));
        }
        *pads.rumble.lock().unwrap() = Rumble { helper: child.stdin.take(), order: numbers.to_vec() };
        Some(Helper { child })
    }

    /// The exit code once the helper has ended.
    fn ended(&mut self) -> Option<i32> {
        self.child.try_wait().ok().flatten().map(|status| status.code().unwrap_or(-1))
    }

    /// Ends the helper. The virtual controllers stay; they rest until the next helper's states.
    fn stop(&mut self, pads: &Pads) {
        // Ended first, so a rumble that waits on its input fails instead of holding the lock.
        let _ = self.child.kill();
        let _ = self.child.wait();
        *pads.rumble.lock().unwrap() = Rumble::default();
    }
}

/// Passes the states that the helper writes to the virtual controllers' thread, until the helper
/// ends.
fn read_states(mut states: ChildStdout, inbox: &Sender<Message>, helper: u32) {
    let mut bytes = [0u8; STATE_LEN];
    while states.read_exact(&mut bytes).is_ok() {
        let (pad, gamepad) = decode_state(&bytes);
        if inbox.send(Message::State { helper, pad, gamepad }).is_err() {
            return;
        }
    }
}

fn encode_state(pad: usize, gamepad: Option<&XINPUT_GAMEPAD>) -> [u8; STATE_LEN] {
    let mut bytes = [0u8; STATE_LEN];
    bytes[0] = pad as u8;
    if let Some(g) = gamepad {
        bytes[1] = 1;
        bytes[2..4].copy_from_slice(&g.wButtons.0.to_le_bytes());
        bytes[4] = g.bLeftTrigger;
        bytes[5] = g.bRightTrigger;
        for (i, axis) in [g.sThumbLX, g.sThumbLY, g.sThumbRX, g.sThumbRY].into_iter().enumerate() {
            bytes[6 + 2 * i..8 + 2 * i].copy_from_slice(&axis.to_le_bytes());
        }
    }
    bytes
}

fn decode_state(bytes: &[u8; STATE_LEN]) -> (usize, Option<XGamepad>) {
    let axis = |i: usize| i16::from_le_bytes([bytes[6 + 2 * i], bytes[7 + 2 * i]]);
    let gamepad = (bytes[1] != 0).then(|| XGamepad {
        buttons: XButtons(u16::from_le_bytes([bytes[2], bytes[3]])),
        left_trigger: bytes[4],
        right_trigger: bytes[5],
        thumb_lx: axis(0),
        thumb_ly: axis(1),
        thumb_rx: axis(2),
        thumb_ry: axis(3),
    });
    (usize::from(bytes[0]), gamepad)
}

/// `steambro bridge-helper`: reads Steam's controllers in these XInput slots and writes their
/// states for the launch that started it, until that launch ends. Virtual controller N gets the
/// N-th slot.
pub fn helper(parent: u32, slots: &[u32]) -> i32 {
    // Steam's hook is a DLL that Steam puts in each process of a game it launched; without it, this
    // process can't see Steam's controllers.
    if crate::overlay::steam_hook().is_none() {
        crate::log!("bridge: Steam's hook isn't in the helper");
        return 1;
    }
    let Ok(parent) = (unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, parent) }) else {
        crate::log!("bridge: the launch {parent} has already ended");
        return 0;
    };
    std::thread::spawn({
        let slots = slots.to_vec();
        move || pass_rumble_to_steam(&slots)
    });
    read_controllers(slots, || unsafe { WaitForSingleObject(parent, 0) } != WAIT_TIMEOUT);
    let _ = unsafe { CloseHandle(parent) };
    0
}

/// Writes the state of each of Steam's controllers in `slots` whenever it changes, and once at
/// the start, until `done` or until the launch stops reading. Only the controllers that existed
/// when this process started can be read.
fn read_controllers(slots: &[u32], done: impl Fn() -> bool) {
    let mut out = std::io::stdout().lock();
    // The packet number last written for each slot; `Some(None)` when it wasn't connected.
    let mut written: Vec<Option<Option<u32>>> = vec![None; slots.len()];
    while !done() {
        for (pad, &slot) in slots.iter().enumerate() {
            let mut state = XINPUT_STATE::default();
            let connected = unsafe { XInputGetState(slot, &mut state) } == 0;
            let packet = connected.then_some(state.dwPacketNumber);
            if written[pad] == Some(packet) {
                continue;
            }
            if connected && written[pad].flatten().is_none() {
                crate::log!("bridge: Steam's controller in slot {slot} drives virtual controller {pad}");
            }
            written[pad] = Some(packet);
            let bytes = encode_state(pad, connected.then_some(&state.Gamepad));
            if out.write_all(&bytes).and_then(|()| out.flush()).is_err() {
                return;
            }
        }
        std::thread::sleep(COPY_PERIOD);
    }
}

/// Gives the rumble that the launch passes on to Steam's controller in the virtual controller's
/// slot, until the launch closes the pipe.
fn pass_rumble_to_steam(slots: &[u32]) {
    let mut input = std::io::stdin().lock();
    let mut bytes = [0u8; RUMBLE_LEN];
    let mut felt = false;
    while input.read_exact(&mut bytes).is_ok() {
        let Some(&slot) = slots.get(usize::from(bytes[0])) else { continue };
        // The motors' speeds come as 0 to 255; XInput takes 0 to 65535.
        let vibration = XINPUT_VIBRATION { wLeftMotorSpeed: u16::from(bytes[1]) * 257, wRightMotorSpeed: u16::from(bytes[2]) * 257 };
        let result = unsafe { XInputSetState(slot, &vibration) };
        if !felt && (bytes[1] > 0 || bytes[2] > 0) {
            felt = true;
            crate::log!("bridge: the game's rumble goes to Steam's controller in slot {slot} (XInput result {result})");
        }
    }
}

/// One of Steam's virtual controllers in this process.
#[derive(Clone, PartialEq, Debug)]
struct Slot {
    /// The XInput slot, 0 to 3.
    index: u32,
    name: String,
    vid: String,
    pid: String,
    /// Steam's handle for the controller, like `0x0000000000000003`; the same while it stays
    /// connected. Steam gives no serial number.
    handle: String,
    /// From the vendor ID; it tells whether the bridge copies the controller (`Kind::bridged`).
    kind: Kind,
}

fn describe(slots: &[Slot]) -> String {
    if slots.is_empty() {
        return "none".into();
    }
    slots.iter().map(|s| format!("slot {} {} ({}:{})", s.index, s.name, s.vid, s.pid)).collect::<Vec<_>>().join(", ")
}

/// Steam's virtual controllers, from the file Steam names in `SteamVirtualGamepadInfo`.
fn steam_slots() -> Vec<Slot> {
    std::env::var_os("SteamVirtualGamepadInfo")
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|text| parse_slots(&text))
        .unwrap_or_default()
}

/// Reads `[slot N]` sections with `name=`, `VID=`, `PID=`, and `handle=` lines (and `type=`, like
/// `steam` or `ps5`, which steambro doesn't need).
fn parse_slots(text: &str) -> Vec<Slot> {
    let mut slots: Vec<Slot> = Vec::new();
    for line in text.lines().map(str::trim) {
        if let Some(index) = line.strip_prefix("[slot ").and_then(|rest| rest.strip_suffix(']')).and_then(|n| n.parse().ok()) {
            slots.push(Slot { index, name: String::new(), vid: String::new(), pid: String::new(), handle: String::new(), kind: Kind::Other });
        } else if let Some((key, value)) = line.split_once('=')
            && let Some(slot) = slots.last_mut()
        {
            match key {
                "name" => slot.name = value.to_string(),
                "VID" => {
                    slot.vid = value.to_lowercase();
                    slot.kind = u16::from_str_radix(slot.vid.trim_start_matches("0x"), 16).map_or(Kind::Other, Kind::of_vendor);
                }
                "PID" => slot.pid = value.to_lowercase(),
                "handle" => slot.handle = value.to_lowercase(),
                _ => {}
            }
        }
    }
    slots.retain(|slot| slot.index < 4);
    slots
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::UI::Input::XboxController::XINPUT_GAMEPAD_BUTTON_FLAGS;

    #[test]
    fn steam_slots_and_their_kinds() {
        let text = "[slot 0]\nname=Steam Controller\nVID=0x28de\nPID=0x1302\nhandle=0x0000000000000003\ntype=steam\n[slot 1]\nname=Xbox 360 Controller\nVID=0x045e\nPID=0x028e\n[slot 2]\nname=DualSense\nVID=0x054c\nPID=0x0ce6\n[slot 3]\nname=Xbox Wireless Controller\nVID=0x045E\nPID=0x0b13\n[slot 4]\nname=Fifth\nVID=0x054c\n";
        let slots = parse_slots(text);
        let kinds: Vec<Kind> = slots.iter().map(|s| s.kind).collect();
        assert_eq!(kinds, [Kind::Steam, Kind::Xbox, Kind::PlayStation, Kind::Xbox]);
        assert_eq!(slots[2].name, "DualSense");
        assert_eq!((slots[0].handle.as_str(), slots[1].handle.as_str()), ("0x0000000000000003", ""));
        assert!(parse_slots("").is_empty());
    }

    fn slot(index: u32, vid: &str, handle: &str) -> Slot {
        Slot { index, name: String::new(), vid: vid.into(), pid: "0x1302".into(), handle: handle.into(), kind: Kind::Steam }
    }

    #[test]
    fn two_controllers_of_one_model_keep_their_names() {
        let mut known = HashMap::new();
        let both = [slot(0, "0x28de", "0x1"), slot(1, "0x28de", "0x2")];
        assert_eq!(identify(&both, &mut known), ["28de:1302#0", "28de:1302#1"]);
        // The first one leaves: the second keeps its name, though it's in slot 0 now.
        assert_eq!(identify(&[slot(0, "0x28de", "0x2")], &mut known), ["28de:1302#1"]);
        // It comes back with a new handle and takes the free name.
        assert_eq!(identify(&[slot(0, "0x28de", "0x2"), slot(1, "0x28de", "0x9")], &mut known), ["28de:1302#1", "28de:1302#0"]);
        // A fresh launch names them by their order.
        assert_eq!(identify(&[slot(0, "0x054c", ""), slot(1, "0x28de", "")], &mut HashMap::new()), ["054c:1302#0", "28de:1302#0"]);
    }

    #[test]
    fn each_controller_keeps_its_number() {
        let mut numbers = Numbers::default();
        assert_eq!((numbers.of("28de:1302#0"), numbers.of("054c:0ce6#0"), numbers.of("28de:1302#0")), (1, 2, 1));
        numbers.numbers.remove("28de:1302#0");
        assert_eq!(numbers.of("28de:1302#1"), 1, "the lowest free number");
        let saved: Numbers = serde_json::from_str(&serde_json::to_string(&numbers).unwrap()).unwrap();
        assert_eq!(saved.numbers, numbers.numbers);
        let old: Numbers = serde_json::from_str("{}").unwrap();
        assert!(old.numbers.is_empty() && old.last.is_none());
    }

    #[test]
    fn known_controllers_come_by_number_and_can_be_forgotten() {
        // A folder of its own, not `steambro-test`: the sync test empties that one while tests run in parallel.
        let dir = std::env::temp_dir().join(format!("steambro-test-controllers-{}", std::process::id()));
        let path = dir.join("controllers.json");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(Numbers::load_from(&path).known().is_empty(), "no file, nothing known");
        let mut numbers = Numbers::default();
        numbers.of("054c:0ce6#0");
        numbers.of("28de:1302#0");
        numbers.of("28de:1302#1");
        numbers.last = Some("28de:1302#0".into());
        numbers.save_to(&path).unwrap();
        let known = Numbers::load_from(&path).known();
        let rows: Vec<(&str, u32, bool)> = known.iter().map(|c| (c.id.as_str(), c.number, c.last)).collect();
        assert_eq!(rows, [("054c:0ce6#0", 1, false), ("28de:1302#0", 2, true), ("28de:1302#1", 3, false)]);

        let mut numbers = Numbers::load_from(&path);
        numbers.forget(&["28de:1302#0".to_string(), "not known".to_string()]);
        numbers.save_to(&path).unwrap();
        let mut numbers = Numbers::load_from(&path);
        assert_eq!(numbers.known().iter().map(|c| c.number).collect::<Vec<_>>(), [1, 3]);
        assert!(numbers.last.is_none(), "the forgotten controller was the last one");
        assert_eq!(numbers.of("057e:2009#0"), 2, "the freed number goes to the next new controller");
        numbers.forget(&["054c:0ce6#0".into(), "28de:1302#1".into(), "057e:2009#0".into()]);
        numbers.save_to(&path).unwrap();
        assert!(path.is_file() && Numbers::load_from(&path).known().is_empty(), "the file stays, empty");
        assert!(!dir.join("controllers.json.tmp").exists(), "the file is written through a temporary one, which the rename takes");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_launch_takes_the_files_changes_but_keeps_the_controllers_it_drives() {
        // The launch started with three controllers known; the settings window has forgotten two since.
        let mut launch = Numbers::default();
        launch.of("054c:0ce6#0");
        launch.of("28de:1302#0");
        launch.of("28de:1302#1");
        let mut file = Numbers::default();
        file.of("054c:0ce6#0");
        file.forget(&["054c:0ce6#0".into()]);
        file.last = Some("057e:2009#0".into());
        file.of("057e:2009#0");
        // The launch drives the second Steam Controller: it keeps its number 3; the others stay forgotten.
        let merged = Numbers::merge(file, &[("28de:1302#1".to_string(), 3)]);
        let known = merged.known();
        let rows: Vec<(&str, u32)> = known.iter().map(|c| (c.id.as_str(), c.number)).collect();
        assert_eq!(rows, [("057e:2009#0", 1), ("28de:1302#1", 3)]);
        assert_eq!(merged.last.as_deref(), Some("057e:2009#0"), "the file's last stays until the launch saves its own");
        // A kept number wins over the file's entry with that number.
        let mut file = Numbers::default();
        file.of("057e:2009#0");
        let merged = Numbers::merge(file, &[("28de:1302#0".to_string(), 1)]);
        assert_eq!(merged.known().iter().map(|c| (c.id.clone(), c.number)).collect::<Vec<_>>(), [("28de:1302#0".to_string(), 1)]);
        assert!(Numbers::merge(Numbers::default(), &[]).known().is_empty());
    }

    #[test]
    fn a_virtual_controller_stands_in_only_without_a_real_xbox_controller() {
        assert!(stand_in_pad(false, true, false), "a controller of Steam's may still wake up");
        assert!(!stand_in_pad(false, true, true), "the game reads the Xbox controller itself");
        assert!(!stand_in_pad(false, false, false), "the account never used a kind the bridge copies");
        assert!(!stand_in_pad(true, true, false), "Steam's controllers get their own");
    }

    #[test]
    fn states_pass_the_pipe_whole() {
        let gamepad = XINPUT_GAMEPAD {
            wButtons: XINPUT_GAMEPAD_BUTTON_FLAGS(0x1000 | 0x0400),
            bLeftTrigger: 7,
            bRightTrigger: 255,
            sThumbLX: -32768,
            sThumbLY: 32767,
            sThumbRX: -1,
            sThumbRY: 300,
        };
        let (pad, read) = decode_state(&encode_state(2, Some(&gamepad)));
        assert_eq!(pad, 2);
        assert_eq!(
            read,
            Some(XGamepad { buttons: XButtons(0x1400), left_trigger: 7, right_trigger: 255, thumb_lx: -32768, thumb_ly: 32767, thumb_rx: -1, thumb_ry: 300 })
        );
        assert_eq!(decode_state(&encode_state(1, None)), (1, None));
    }
}

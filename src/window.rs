//! The windows of a game during its launch: which windows are the game's, whether one of them is
//! in front, bringing it in front, and showing an app in full screen.
//!
//! A window is the game's when one of the game's processes owns it, or when it carries the app ID
//! of one of the game's packages. A web app from the Xbox app, like Netflix, needs the second: its
//! window belongs to Edge's process, which has no package identity, but the window has the app's ID.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM, PROPERTYKEY, RECT};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx};
use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_KEYUP, MAPVK_VK_TO_VSC, MapVirtualKeyW, SendInput, VIRTUAL_KEY, VK_F11,
};
use windows::Win32::UI::Shell::PropertiesSystem::{IPropertyStore, SHGetPropertyStoreForWindow};
use windows::Win32::UI::Shell::{ITaskbarList2, TaskbarList};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GW_OWNER, GWL_STYLE, GetClassNameW, GetClientRect, GetForegroundWindow, GetWindow, GetWindowLongPtrW, GetWindowRect, GetWindowTextW,
    GetWindowThreadProcessId, IsIconic, IsWindowVisible, SWP_FRAMECHANGED, SWP_NOZORDER, SetWindowLongPtrW, SetWindowPos, WS_CAPTION,
    WS_MAXIMIZEBOX, WS_MINIMIZEBOX, WS_THICKFRAME,
};
use windows::core::BOOL;

use crate::Worker;

/// `System.AppUserModel.ID`, the app ID that Windows keeps for a window.
const APP_ID: PROPERTYKEY = PROPERTYKEY { fmtid: windows::core::GUID::from_u128(0x9f4c2855_9f79_4b39_a8d0_e1d42de1d5f3), pid: 5 };
/// How often the full screen step looks for the app's window.
const FULL_SCREEN_POLL: Duration = Duration::from_millis(200);
/// How long the app's window is in front before it goes full screen, so that the app has built it.
const FULL_SCREEN_SETTLE: Duration = Duration::from_millis(800);
/// Edge's windows, which include web apps; F11 shows them in full screen.
const EDGE_WINDOW: &str = "Chrome_WidgetWin_1";

/// The game as launch.rs last saw it: its processes and its packages.
#[derive(Clone, Default)]
pub struct GameWindows {
    pub pids: Vec<u32>,
    /// Package family names; a window with an app ID from one of them is the game's.
    pub packages: Vec<String>,
    /// The launch's processes (`process::Family`): steambro's and the ones it started, like the
    /// store's app.
    pub family: Vec<u32>,
}

impl GameWindows {
    /// The window is the game's.
    pub fn owns(&self, hwnd: HWND) -> bool {
        !hwnd.is_invalid() && (self.pids.contains(&window_pid(hwnd)) || self.has_app_id(hwnd))
    }

    /// The window carries the app ID of one of the game's packages. The calling thread must have COM.
    pub fn has_app_id(&self, hwnd: HWND) -> bool {
        !self.packages.is_empty()
            && app_id(hwnd)
                .and_then(|id| id.split_once('!').map(|(family, _)| family.to_string()))
                .is_some_and(|family| self.packages.iter().any(|p| p.eq_ignore_ascii_case(&family)))
    }

    /// The game's main window: its largest visible top-level window.
    pub fn main_window(&self) -> Option<HWND> {
        struct Search<'a> {
            game: &'a GameWindows,
            best: Option<(HWND, i64)>,
        }
        unsafe extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> BOOL {
            let search = unsafe { &mut *(lparam.0 as *mut Search) };
            let candidate = unsafe { IsWindowVisible(hwnd).as_bool() && !IsIconic(hwnd).as_bool() && GetWindow(hwnd, GW_OWNER).is_err() } && search.game.owns(hwnd);
            if candidate {
                let mut rect = RECT::default();
                if unsafe { GetClientRect(hwnd, &mut rect) }.is_ok() {
                    let area = i64::from(rect.right - rect.left) * i64::from(rect.bottom - rect.top);
                    if area > 0 && search.best.is_none_or(|(_, best)| area > best) {
                        search.best = Some((hwnd, area));
                    }
                }
            }
            true.into()
        }
        if self.pids.is_empty() && self.packages.is_empty() {
            return None;
        }
        let mut search = Search { game: self, best: None };
        let _ = unsafe { EnumWindows(Some(visit), LPARAM(&raw mut search as isize)) };
        search.best.map(|(hwnd, _)| hwnd)
    }
}

/// Whether a window of the game is in front. It reads the window's app ID, a COM call, only when
/// another window comes to the front, so a loop can ask it many times a second, as the overlay
/// does.
#[derive(Default)]
pub struct InFront {
    window: HWND,
    has_app_id: bool,
}

impl InFront {
    /// The calling thread must have COM (`GameWindows::has_app_id`).
    pub fn game(&mut self, game: &GameWindows, foreground: HWND) -> bool {
        if foreground != self.window {
            *self = Self { window: foreground, has_app_id: game.has_app_id(foreground) };
        }
        !foreground.is_invalid() && (self.has_app_id || game.pids.contains(&window_pid(foreground)))
    }
}

/// How often the step that brings the game forward looks for its window.
const FORWARD_POLL: Duration = Duration::from_millis(250);
/// Steam's own processes; a window of theirs in front, like Big Picture, is where Play was pressed.
const STEAM_EXES: [&str; 2] = ["steamwebhelper.exe", "steam.exe"];

/// One of Steam's own windows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SteamWindow {
    /// Steam uses its layout for its menus there (769).
    BigPicture,
    /// The desktop client, Friends and chat, and the like: Steam uses the desktop layout there.
    Other,
}

/// The window of Steam's in front, if one is. Big Picture's window is titled "Steam Big Picture
/// Mode" and covers its screen; the other windows of Steam's processes are the desktop client's.
pub fn steam_window_in_front() -> Option<SteamWindow> {
    let front = unsafe { GetForegroundWindow() };
    if front.is_invalid() {
        return None;
    }
    let exe = crate::process::image_path(window_pid(front))?;
    if !STEAM_EXES.iter().any(|name| crate::library::is_named(&exe, name)) {
        return None;
    }
    let mut text = [0u16; 128];
    let len = usize::try_from(unsafe { GetWindowTextW(front, &mut text) }).unwrap_or(0);
    let titled = String::from_utf16_lossy(&text[..len.min(text.len())]).contains("Big Picture");
    let mut rect = RECT::default();
    let covers = unsafe { GetWindowRect(front, &mut rect) }.is_ok()
        && monitor_rect(front).is_some_and(|m| (m.left, m.top, m.right, m.bottom) == (rect.left, rect.top, rect.right, rect.bottom));
    Some(if titled || covers { SteamWindow::BigPicture } else { SteamWindow::Other })
}

/// Explorer's windows that get the front when the window that had it closes: the desktop and the
/// taskbars.
const SHELL_CLASSES: [&str; 4] = ["Progman", "WorkerW", "Shell_TrayWnd", "Shell_SecondaryTrayWnd"];

/// Brings the game's window in front when it shows behind a window that the user didn't choose.
/// Windows lets a program take the front only in some cases, so a game that a store or Windows
/// starts may show without it, often seconds after Play: behind Big Picture, or in front of
/// everything but without the keyboard and the controller, which stay with the window that had the
/// front. Rocket League showed so (2026-11-10), started by the Epic Games launcher: it didn't take
/// "press any key" from the controller, and Steam kept the desktop layout until the user clicked it.
///
/// It watches until a window of the game has kept the front for `FORWARD_SETTLED`: from then on,
/// the window in front is the user's choice. Until then it brings the game's main window in front
/// of Steam's windows, of the desktop, where Windows leaves the front when the window that had it
/// closes, of no window, which is also a window that isn't shown, and of the launch's own programs
/// (`GameWindows::family`), like the store's app. Any other program's window is the user's choice
/// and stays, as do steambro's own windows, like the overlay's; the step looks again when that
/// window gives the front away. Betrayer showed so (2026-11-10): a window of Windows' own
/// (`PickerHost.exe`) had the front, then Big Picture took it, and the Steam button opened Big
/// Picture's menu behind the game. It puts one window in front at most `FORWARD_TAKES` times, so
/// that it never fights a program that takes the front back.
pub struct Forward {
    _step: Worker,
}

/// How long a window of the game keeps the front before the step stops.
const FORWARD_SETTLED: Duration = Duration::from_secs(10);
/// How often the step puts one window of the game in front.
const FORWARD_TAKES: u32 = 5;

impl Forward {
    /// `game` is kept up to date by launch.rs. The step stops after `wait`.
    pub fn start(game: Arc<Mutex<GameWindows>>, wait: Duration) -> Self {
        Self { _step: Worker::spawn(move |stop| forward(&game, wait, stop)) }
    }
}

/// What the game's window shows behind.
enum Behind {
    /// Another window of the game, like its splash, or steambro's own: look again later.
    Wait,
    /// No window, Steam's, the desktop, or one of the launch's programs.
    Taken(String),
    /// A window of another program.
    Users(String),
}

fn behind(game: &GameWindows, front: HWND) -> Behind {
    if front.is_invalid() || !shown(front) {
        return Behind::Taken("no window".into());
    }
    let pid = window_pid(front);
    if game.owns(front) || pid == std::process::id() {
        return Behind::Wait;
    }
    if let Some(what) = steam_or_desktop(front) {
        return Behind::Taken(what.into());
    }
    let name = crate::process::image_path(pid).map_or_else(|| "a window".to_string(), |p| p.file_name().unwrap_or_default().to_string_lossy().into_owned());
    if game.family.contains(&pid) { Behind::Taken(name) } else { Behind::Users(name) }
}

/// `Steam` or `the desktop` when the window is Steam's, or Explorer's desktop or a taskbar.
fn steam_or_desktop(hwnd: HWND) -> Option<&'static str> {
    let exe = crate::process::image_path(window_pid(hwnd))?;
    if STEAM_EXES.iter().any(|steam| crate::library::is_named(&exe, steam)) {
        return Some("Steam");
    }
    (crate::library::is_named(&exe, "explorer.exe") && SHELL_CLASSES.contains(&class_name(hwnd).as_str())).then_some("the desktop")
}

/// Whether the window in front is none, Steam's, or Explorer's desktop or a taskbar: where the user
/// is after a game's window closed, as after Steam's Exit game.
pub fn steam_or_desktop_in_front() -> bool {
    let front = unsafe { GetForegroundWindow() };
    front.is_invalid() || !shown(front) || steam_or_desktop(front).is_some()
}

/// A window that the user can see: visible, and not one that Windows keeps hidden (cloaked), as it
/// does the windows of its own hosts and of apps on another desktop.
fn shown(hwnd: HWND) -> bool {
    use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
    let mut cloaked = 0u32;
    let asked = unsafe { DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, (&raw mut cloaked).cast(), size_of::<u32>() as u32) };
    unsafe { IsWindowVisible(hwnd) }.as_bool() && !(asked.is_ok() && cloaked != 0)
}

fn forward(game: &Mutex<GameWindows>, wait: Duration, stop: &AtomicBool) {
    let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    let begun = Instant::now();
    // How often each main window was put in front and when last, and the last line about one,
    // said once.
    let mut takes: HashMap<isize, (u32, Instant)> = HashMap::new();
    let mut said: Option<(isize, String)> = None;
    let mut game_in_front: Option<Instant> = None;
    while !stop.load(Ordering::Relaxed) && begun.elapsed() < wait {
        std::thread::sleep(FORWARD_POLL);
        let game = game.lock().unwrap().clone();
        let front = unsafe { GetForegroundWindow() };
        if game.owns(front) {
            if game_in_front.get_or_insert_with(Instant::now).elapsed() >= FORWARD_SETTLED {
                return;
            }
            continue;
        }
        game_in_front = None;
        let Some(hwnd) = game.main_window() else { continue };
        let id = hwnd.0 as isize;
        let line = match behind(&game, front) {
            Behind::Wait => continue,
            Behind::Users(name) => format!("the game's window showed behind {name}, which the user has in front; it stays there while that window has the front"),
            // A second apart, so that a refusal by Windows has time to pass.
            Behind::Taken(_) if takes.get(&id).is_some_and(|(n, last)| *n >= FORWARD_TAKES || last.elapsed() < Duration::from_secs(1)) => continue,
            Behind::Taken(what) => {
                let (n, last) = takes.entry(id).or_insert((0, Instant::now()));
                (*n, *last) = (*n + 1, Instant::now());
                if take_front(hwnd) { format!("the game's window showed behind {what}; brought it in front") } else { format!("the game's window showed behind {what}, and Windows didn't let steambro bring it in front") }
            }
        };
        if said.as_ref() != Some(&(id, line.clone())) {
            crate::log!("{line}");
            said = Some((id, line));
        }
    }
}

/// The processes among `pids` that have a visible top-level window, minimized or not, owned or
/// not, like a game's question.
pub fn with_windows(pids: &[u32]) -> HashSet<u32> {
    struct Search<'a> {
        pids: &'a [u32],
        found: HashSet<u32>,
    }
    unsafe extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let search = unsafe { &mut *(lparam.0 as *mut Search) };
        let pid = window_pid(hwnd);
        if search.pids.contains(&pid) && unsafe { IsWindowVisible(hwnd) }.as_bool() {
            search.found.insert(pid);
        }
        true.into()
    }
    let mut search = Search { pids, found: HashSet::new() };
    if !pids.is_empty() {
        let _ = unsafe { EnumWindows(Some(visit), LPARAM(&raw mut search as isize)) };
    }
    search.found
}

/// The process of the window in front; 0 for none.
pub fn front_pid() -> u32 {
    window_pid(unsafe { GetForegroundWindow() })
}

/// Puts a window in front. Windows lets a program do that when it joins the input of the window in
/// front.
pub fn take_front(hwnd: HWND) -> bool {
    use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    use windows::Win32::UI::WindowsAndMessaging::{BringWindowToTop, SW_RESTORE, SetForegroundWindow, ShowWindow};
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        if SetForegroundWindow(hwnd).as_bool() && GetForegroundWindow() == hwnd {
            return true;
        }
        let front = GetWindowThreadProcessId(GetForegroundWindow(), None);
        let ours = GetCurrentThreadId();
        let _ = AttachThreadInput(ours, front, true);
        let _ = BringWindowToTop(hwnd);
        let _ = SetForegroundWindow(hwnd);
        let _ = AttachThreadInput(ours, front, false);
        GetForegroundWindow() == hwnd
    }
}

/// For `time` from now, minimizes a window of a process with this exe name, like `Battle.net.exe`,
/// that comes to the front: an app that steambro opens again during the game shouldn't cover the
/// game, and some apps show their first window whatever they're asked. Returns whether it did.
pub fn keep_behind(exe_name: &str, time: Duration) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{SW_MINIMIZE, ShowWindow};
    let deadline = Instant::now() + time;
    let mut minimized = false;
    while Instant::now() < deadline {
        let front = unsafe { GetForegroundWindow() };
        let pid = window_pid(front);
        if pid != 0 && crate::process::image_path(pid).is_some_and(|p| crate::library::is_named(&p, exe_name)) {
            let _ = unsafe { ShowWindow(front, SW_MINIMIZE) };
            minimized = true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    minimized
}

/// The process that owns a window; 0 for none.
pub fn window_pid(hwnd: HWND) -> u32 {
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    pid
}

/// The app ID that Windows keeps for the window, like `4DF9E0F8.Netflix_mcm4njqhnhss8!Netflix.App`.
/// The calling thread must have COM.
fn app_id(hwnd: HWND) -> Option<String> {
    let store: IPropertyStore = unsafe { SHGetPropertyStoreForWindow(hwnd) }.ok()?;
    let value = unsafe { store.GetValue(&APP_ID) }.ok()?;
    let id = value.to_string();
    (!id.is_empty()).then_some(id)
}

/// Shows an app's window in full screen once, when it first comes to the front. The window stays
/// as the user leaves it afterwards.
pub struct FullScreen {
    _step: Worker,
}

impl FullScreen {
    /// `game` is kept up to date by launch.rs. The step gives up after `wait`.
    pub fn start(game: Arc<Mutex<GameWindows>>, wait: Duration) -> Self {
        Self { _step: Worker::spawn(move |stop| full_screen(&game, wait, stop)) }
    }
}

fn full_screen(game: &Mutex<GameWindows>, wait: Duration, stop: &AtomicBool) {
    let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    // Window positions in real pixels, whatever the display scaling.
    unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    let begun = Instant::now();
    let mut in_front_since: Option<Instant> = None;
    while !stop.load(Ordering::Relaxed) {
        if begun.elapsed() > wait {
            crate::log!("full screen: the app's window didn't come to the front within {}s; it keeps its window", wait.as_secs());
            return;
        }
        std::thread::sleep(FULL_SCREEN_POLL);
        let game = game.lock().unwrap().clone();
        let Some(hwnd) = game.main_window() else { continue };
        if unsafe { GetForegroundWindow() } != hwnd {
            in_front_since = None;
            continue;
        }
        if in_front_since.get_or_insert_with(Instant::now).elapsed() < FULL_SCREEN_SETTLE {
            continue;
        }
        let Some(monitor) = monitor_rect(hwnd) else { return };
        if window_rect(hwnd) == Some(monitor) {
            crate::log!("full screen: the app's window already covers the screen");
        } else if class_name(hwnd) == EDGE_WINDOW {
            press(VK_F11.0);
            crate::log!("full screen: pressed F11 in the web app's window");
        } else {
            borderless(hwnd, monitor);
            crate::log!("full screen: the app's window covers the screen, without its frame");
        }
        return;
    }
}

/// Takes the frame off the window, sizes it to its monitor, and tells the taskbar to stay behind it.
fn borderless(hwnd: HWND, monitor: RECT) {
    let frame = (WS_CAPTION | WS_THICKFRAME | WS_MINIMIZEBOX | WS_MAXIMIZEBOX).0 as isize;
    unsafe {
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
        SetWindowLongPtrW(hwnd, GWL_STYLE, style & !frame);
        let _ = SetWindowPos(hwnd, None, monitor.left, monitor.top, monitor.right - monitor.left, monitor.bottom - monitor.top, SWP_FRAMECHANGED | SWP_NOZORDER);
    }
    let taskbar: windows::core::Result<ITaskbarList2> = unsafe { CoCreateInstance(&TaskbarList, None, CLSCTX_INPROC_SERVER) };
    if let Ok(taskbar) = taskbar
        && unsafe { taskbar.HrInit() }.is_ok()
    {
        let _ = unsafe { taskbar.MarkFullscreenWindow(hwnd, true) };
    }
}

/// Presses and releases a key, as input from the keyboard; it goes to the window in front.
fn press(vk: u16) {
    let inputs = [key_input(vk, false), key_input(vk, true)];
    unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
}

/// A key going down or up, for `SendInput`.
pub fn key_input(vk: u16, up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                wScan: unsafe { MapVirtualKeyW(u32::from(vk), MAPVK_VK_TO_VSC) } as u16,
                dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) },
                ..Default::default()
            },
        },
    }
}

fn class_name(hwnd: HWND) -> String {
    let mut buffer = [0u16; 64];
    let len = unsafe { GetClassNameW(hwnd, &mut buffer) };
    String::from_utf16_lossy(&buffer[..len.max(0) as usize])
}

fn window_rect(hwnd: HWND) -> Option<RECT> {
    let mut rect = RECT::default();
    unsafe { GetWindowRect(hwnd, &mut rect) }.ok().map(|()| rect)
}

/// The whole monitor that shows most of the window.
pub fn monitor_rect(hwnd: HWND) -> Option<RECT> {
    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool().then_some(info.rcMonitor)
}

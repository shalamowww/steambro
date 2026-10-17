//! The windows of a game during its launch: which windows are the game's, and showing an app in
//! full screen.
//!
//! A window is the game's when one of the game's processes owns it, or when it carries the app ID
//! of one of the game's packages. A web app from the Xbox app, like Netflix, needs the second: its
//! window belongs to Edge's process, which has no package identity, but the window has the app's ID.

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
    EnumWindows, GW_OWNER, GWL_STYLE, GetClassNameW, GetClientRect, GetForegroundWindow, GetWindow, GetWindowLongPtrW, GetWindowRect,
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
}

impl GameWindows {
    pub fn new(packages: Vec<String>) -> Self {
        Self { pids: Vec::new(), packages }
    }

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

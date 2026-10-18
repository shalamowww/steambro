//! Steam's overlay over a game that Steam doesn't see, like an Xbox game.
//!
//! Steam loads its overlay (`GameOverlayRenderer64.dll`) into `steambro launch`, which Steam
//! started, but not into the game, which Windows started. So steambro has a window of its own for
//! the overlay to draw into: transparent, always on top, and hidden until the overlay opens. It
//! draws empty Direct3D 11 frames whenever the overlay asks for one (`BOverlayNeedsPresent`), also
//! while hidden, which keeps the overlay working. When the overlay opens (`SteamOverlayIsUsing*`),
//! for example with the Steam button, the window shows over the game and takes the focus; when it
//! closes, the window hides and the game gets the focus back. The overlay's notifications don't
//! show, because the window stays hidden for them: a window over the game costs the game frames.
//! Steam's screenshot key and FPS counter see this window, not the game, and a game in exclusive
//! full screen loses the front to it.
//!
//! The Steam button opens nothing until Steam has prepared its overlay for the window, which it
//! does once the window has had the focus while drawing. So the first time the game is in front,
//! the window takes the focus for a few frames, unseen, and gives it back.
//!
//! Steam's overlay key (Shift+Tab unless the user changed it) goes to the game, not to this
//! window. So while the game is in front, steambro holds the key as a hotkey; when it comes, the
//! window shows, takes the focus, and presses the key again, now where the overlay sees it. The
//! overlay learns the modifiers from the key messages of its window, so they go down again even
//! when the user still holds them, and it looks for the key itself each time it's asked for a
//! frame, so the key stays down until the overlay opens.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HMODULE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDeviceAndSwapChain, ID3D11Device, ID3D11DeviceContext, ID3D11RenderTargetView,
    ID3D11Texture2D,
};
use windows::Win32::Graphics::Dwm::DwmExtendFrameIntoClientArea;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_UNKNOWN, DXGI_MODE_DESC, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    DXGI_PRESENT, DXGI_SWAP_CHAIN_DESC, DXGI_SWAP_CHAIN_FLAG, DXGI_SWAP_EFFECT_DISCARD, DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGISwapChain,
};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, INPUT, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN, RegisterHotKey, SendInput, UnregisterHotKey, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect, GetForegroundWindow, GetWindowThreadProcessId, HWND_TOPMOST,
    IDC_ARROW, LoadCursorW, MSG, MsgWaitForMultipleObjects, PM_REMOVE, PeekMessageW, QS_ALLINPUT, RegisterClassExW, SW_HIDE, SWP_NOACTIVATE,
    SWP_SHOWWINDOW, SetForegroundWindow, SetWindowPos, ShowWindow, TranslateMessage, WM_HOTKEY, WNDCLASSEXW, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_POPUP,
};
use windows::core::{PCSTR, s, w};

use crate::Worker;
use crate::window::{GameWindows, InFront, key_input, monitor_rect, window_pid};

/// How long steambro waits for Steam to load its overlay into the launch.
const OVERLAY_WAIT: Duration = Duration::from_secs(3);
/// How often the hidden window asks the overlay whether it needs a frame. Valve suggests about 33
/// times a second for programs that draw only when needed.
const IDLE_POLL: Duration = Duration::from_millis(30);
/// After taking the focus, the overlay sets up its input hook in the window's next frames.
const FOCUS_FRAMES: u32 = 4;
/// The frames the window draws with the focus so that Steam prepares its overlay for it.
const PREPARE_FRAMES: u32 = 10;
/// The key stays down until the overlay opens, but not longer than this.
const KEY_HOLD: Duration = Duration::from_millis(500);
/// After the key, how long the window keeps the focus while waiting for the overlay to open.
const KEY_WAIT: Duration = Duration::from_millis(1000);
const HOTKEY_ID: i32 = 1;

/// Steam's overlay over the game while it runs; it closes when dropped.
pub struct Overlay {
    _window: Worker,
}

impl Overlay {
    /// `game` is the game's processes and packages, which launch.rs keeps up to date. `key` is
    /// Steam's overlay key, or `None` to leave the key to the game.
    pub fn start(game: Arc<Mutex<GameWindows>>, key: Option<Hotkey>) -> Self {
        let window = Worker::spawn(move |stop| {
            if let Err(e) = run(&game, key.as_ref(), stop) {
                crate::log!("overlay: {e}");
            }
        });
        Self { _window: window }
    }
}

/// Waits for Steam to load its overlay, which is also its hook, into this process. `None` when it
/// doesn't, as when Steam didn't start the process, or the overlay is off in Steam for every game
/// or for this one.
pub fn steam_hook() -> Option<HMODULE> {
    let deadline = Instant::now() + OVERLAY_WAIT;
    loop {
        if let Ok(module) = unsafe { GetModuleHandleW(w!("GameOverlayRenderer64.dll")) } {
            return Some(module);
        }
        if Instant::now() > deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// A key with modifiers, as Steam stores its overlay key.
#[derive(Clone, Debug, PartialEq)]
pub struct Hotkey {
    pub modifiers: HOT_KEY_MODIFIERS,
    pub key: VIRTUAL_KEY,
    /// As Steam stores it, like `Shift\tKEY_TAB`, for the log.
    pub text: String,
}

impl Hotkey {
    /// Parses Steam's form: modifiers and then a key name, separated by tabs. `None` for
    /// `KEY_NONE`, an empty setting, or a name steambro doesn't know.
    pub fn parse(text: &str) -> Option<Self> {
        let mut modifiers = HOT_KEY_MODIFIERS(0);
        let mut key = None;
        for part in text.split('\t').map(str::trim).filter(|p| !p.is_empty()) {
            match part.to_ascii_lowercase().as_str() {
                "shift" => modifiers |= MOD_SHIFT,
                "ctrl" | "control" => modifiers |= MOD_CONTROL,
                "alt" => modifiers |= MOD_ALT,
                "win" => modifiers |= MOD_WIN,
                _ => key = Some(virtual_key(part)?),
            }
        }
        Some(Self { modifiers, key: key?, text: text.to_string() })
    }

    /// The text for the log and the window, like `Shift+Tab`.
    pub fn display(&self) -> String {
        self.text
            .split('\t')
            .map(|part| {
                let name = part.trim().strip_prefix("KEY_").unwrap_or(part.trim()).to_ascii_lowercase();
                let mut chars = name.chars();
                chars.next().map(|c| c.to_ascii_uppercase().to_string() + chars.as_str()).unwrap_or_default()
            })
            .collect::<Vec<_>>()
            .join("+")
    }
}

/// The Windows key code for one of Steam's key names (Source's `KEY_*` button codes).
fn virtual_key(name: &str) -> Option<VIRTUAL_KEY> {
    let name = name.strip_prefix("KEY_")?;
    let code: u16 = match name {
        _ if name.len() == 1 && name.as_bytes()[0].is_ascii_alphanumeric() => u16::from(name.as_bytes()[0]),
        _ if name.starts_with("PAD_") && name.len() == 5 && name.as_bytes()[4].is_ascii_digit() => 0x60 + u16::from(name.as_bytes()[4] - b'0'),
        _ if name.starts_with('F') && name[1..].parse::<u16>().is_ok_and(|n| (1..=12).contains(&n)) => 0x6F + name[1..].parse::<u16>().ok()?,
        "TAB" => 0x09,
        "SPACE" => 0x20,
        "ENTER" => 0x0D,
        "ESCAPE" => 0x1B,
        "BACKSPACE" => 0x08,
        "BACKQUOTE" => 0xC0,
        "MINUS" => 0xBD,
        "EQUAL" => 0xBB,
        "LBRACKET" => 0xDB,
        "RBRACKET" => 0xDD,
        "BACKSLASH" => 0xDC,
        "SEMICOLON" => 0xBA,
        "APOSTROPHE" => 0xDE,
        "COMMA" => 0xBC,
        "PERIOD" => 0xBE,
        "SLASH" => 0xBF,
        "INSERT" => 0x2D,
        "DELETE" => 0x2E,
        "HOME" => 0x24,
        "END" => 0x23,
        "PAGEUP" => 0x21,
        "PAGEDOWN" => 0x22,
        "UP" => 0x26,
        "DOWN" => 0x28,
        "LEFT" => 0x25,
        "RIGHT" => 0x27,
        "PAD_DIVIDE" => 0x6F,
        "PAD_MULTIPLY" => 0x6A,
        "PAD_MINUS" => 0x6D,
        "PAD_PLUS" => 0x6B,
        "PAD_DECIMAL" => 0x6E,
        "PAD_ENTER" => 0x0D,
        "PAUSE" | "BREAK" => 0x13,
        "SCROLLLOCK" => 0x91,
        "APP" => 0x5D,
        _ => return None,
    };
    Some(VIRTUAL_KEY(code))
}

/// The overlay's exported calls, which Valve's own Steamworks library uses. Each takes nothing and
/// returns a byte flag.
struct Exports {
    needs_present: unsafe extern "system" fn() -> u8,
    using_mouse: unsafe extern "system" fn() -> u8,
    using_keyboard: unsafe extern "system" fn() -> u8,
    using_gamepad: unsafe extern "system" fn() -> u8,
}

impl Exports {
    /// The calls of Steam's overlay in this process (`steam_hook`).
    fn find() -> Option<Self> {
        let module = steam_hook()?;
        let get = |name: PCSTR| -> Option<unsafe extern "system" fn() -> u8> {
            let proc = unsafe { GetProcAddress(module, name) }?;
            // The exports take no arguments and return a byte, as their code shows.
            Some(unsafe { std::mem::transmute::<unsafe extern "system" fn() -> isize, unsafe extern "system" fn() -> u8>(proc) })
        };
        Some(Self {
            needs_present: get(s!("BOverlayNeedsPresent"))?,
            using_mouse: get(s!("SteamOverlayIsUsingMouse"))?,
            using_keyboard: get(s!("SteamOverlayIsUsingKeyboard"))?,
            using_gamepad: get(s!("SteamOverlayIsUsingGamepad"))?,
        })
    }

    fn needs_frame(&self) -> bool {
        unsafe { (self.needs_present)() != 0 }
    }

    /// The overlay is open and takes input.
    fn is_open(&self) -> bool {
        unsafe { (self.using_mouse)() != 0 || (self.using_keyboard)() != 0 || (self.using_gamepad)() != 0 }
    }
}

fn run(game_windows: &Mutex<GameWindows>, key: Option<&Hotkey>, stop: &AtomicBool) -> crate::Result<()> {
    let Some(overlay) = Exports::find() else {
        crate::log!("overlay: Steam's overlay isn't loaded in steambro; it's off in Steam, for every game or for this one");
        return Ok(());
    };
    // The window must cover the game in real pixels, whatever the display scaling.
    unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    // COM reads a window's app ID (GameWindows::owns).
    let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    let mut window = Window::create()?;
    crate::log!(
        "overlay: ready; it opens with the Steam button{}",
        key.map(|k| format!(" and {} while the game is in front", k.display())).unwrap_or_default()
    );

    let mut hotkey_held = false;
    let mut passing: Option<Passing> = None;
    // The window has the focus for the overlay; the window that had it gets it back afterwards.
    let mut focus_from: Option<HWND> = None;
    let mut was_open = false;
    let mut prepared = false;
    let mut in_front = InFront::default();
    // EXPERIMENT: the overlay's state on each change, to find why it sometimes opens unseen.
    let mut front_window = HWND::default();
    let mut front_name = String::new();
    let mut last_state = String::new();
    // One frame now, so that the overlay is ready before anyone opens it.
    window.draw();
    while !stop.load(Ordering::Relaxed) {
        let wait = if window.shown { 0 } else { IDLE_POLL.as_millis() as u32 };
        unsafe { MsgWaitForMultipleObjects(None, false, wait, QS_ALLINPUT) };
        let mut hotkey = false;
        let mut msg = MSG::default();
        while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
            if msg.message == WM_HOTKEY && msg.wParam.0 == HOTKEY_ID as usize {
                hotkey = true;
            }
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        let game = game_windows.lock().unwrap().clone();
        let foreground = unsafe { GetForegroundWindow() };
        let game_in_front = in_front.game(&game, foreground);
        if foreground != front_window {
            front_window = foreground;
            front_name = crate::process::image_path(window_pid(foreground))
                .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                .unwrap_or_else(|| format!("{foreground:?}"));
        }
        if !prepared && game_in_front && !window.shown {
            prepared = true;
            if window.prepare(&game, foreground) {
                crate::log!("overlay: prepared for the Steam button");
            } else {
                crate::log!("overlay: Windows didn't let steambro take the focus; the Steam button works after the overlay key has opened the overlay once");
            }
        }

        // The key is steambro's only while the game is in front; everywhere else it stays the
        // user's, and in the overlay it closes the overlay.
        if let Some(key) = key
            && game_in_front != hotkey_held
        {
            hotkey_held = if game_in_front {
                if let Err(e) = unsafe { RegisterHotKey(Some(window.hwnd), HOTKEY_ID, key.modifiers | MOD_NOREPEAT, u32::from(key.key.0)) } {
                    // Not tried again until the game comes to the front again.
                    crate::log!("overlay: couldn't take {} while the game is in front: {e}", key.display());
                }
                true
            } else {
                let _ = unsafe { UnregisterHotKey(Some(window.hwnd), HOTKEY_ID) };
                false
            };
        }

        let open = overlay.is_open();
        if open != was_open {
            crate::log!("overlay: {}", if open { "open" } else { "closed" });
            was_open = open;
        }
        let state = format!(
            "open={open} needs_frame={} shown={} front={} game_in_front={game_in_front} focus_from={} passing={} key_held={hotkey_held}",
            !window.shown && overlay.needs_frame(),
            window.shown,
            if foreground == window.hwnd { "steambro" } else { &front_name },
            focus_from.is_some(),
            passing.is_some(),
        );
        if state != last_state {
            crate::log!("overlay: EXPERIMENT {state}");
            last_state = state;
        }
        if hotkey && (open || passing.is_some()) {
            crate::log!("overlay: EXPERIMENT the key came while open={open} passing={}; ignored", passing.is_some());
        }
        if hotkey
            && passing.is_none()
            && !open
            && let Some(key) = key
        {
            crate::log!("overlay: {} pressed in the game", key.display());
            window.show_over(game.main_window());
            if window.take_focus() {
                focus_from.get_or_insert(foreground);
                let _ = unsafe { UnregisterHotKey(Some(window.hwnd), HOTKEY_ID) };
                hotkey_held = false;
                passing = Some(Passing::Focused(0));
            } else {
                crate::log!("overlay: Windows didn't let steambro take the focus");
            }
        }
        // The window belongs over the game only while the game or the window itself is in front;
        // always on top, it would cover another program the user switched to.
        let ours_or_game = game_in_front || foreground == window.hwnd;
        if open && !window.shown && ours_or_game {
            // Opened another way, like with the Steam button, or the user came back to the game.
            window.show_over(game.main_window());
        }
        if open && window.shown && focus_from.is_none() && ours_or_game && window.take_focus() {
            focus_from = Some(foreground);
        }

        // The key goes to the overlay in steps, a few frames apart.
        passing = match (passing.take(), key) {
            (Some(Passing::Held(_)), Some(key)) if open => {
                key_up(key);
                None
            }
            (Some(_), _) if open => None,
            (Some(Passing::Focused(frames)), Some(key)) if frames >= FOCUS_FRAMES => {
                key_down(key);
                Some(Passing::Held(Instant::now()))
            }
            (Some(Passing::Held(since)), Some(key)) if since.elapsed() >= KEY_HOLD => {
                key_up(key);
                Some(Passing::Sent(Instant::now()))
            }
            (Some(Passing::Sent(since)), _) if since.elapsed() > KEY_WAIT => {
                crate::log!("overlay: didn't open after the key");
                None
            }
            (other, _) => other,
        };

        if window.shown {
            window.draw();
            if let Some(Passing::Focused(frames)) = &mut passing {
                *frames += 1;
            }
            if !open && passing.is_none() {
                if let Some(from) = focus_from.take() {
                    window.give_focus(from, &game);
                }
                window.hide();
            } else if !ours_or_game && passing.is_none() {
                // The user switched to another program; the overlay stays open for their return.
                focus_from = None;
                window.hide();
            }
        } else if overlay.needs_frame() {
            window.draw();
        }
    }
    if hotkey_held {
        let _ = unsafe { UnregisterHotKey(Some(window.hwnd), HOTKEY_ID) };
    }
    Ok(())
}

/// Passing the overlay key on to the overlay, which sees it only in the window that has the focus.
enum Passing {
    /// The window took the focus this many frames ago.
    Focused(u32),
    /// The key and its modifiers are down, since then.
    Held(Instant),
    /// The key is up again; waiting for the overlay to open.
    Sent(Instant),
}

/// The Windows key codes of the key's modifiers.
fn modifier_keys(key: &Hotkey) -> Vec<u16> {
    [(MOD_SHIFT, 0x10), (MOD_CONTROL, 0x11), (MOD_ALT, 0x12), (MOD_WIN, 0x5B)]
        .into_iter()
        .filter(|(flag, _)| key.modifiers.0 & flag.0 != 0)
        .map(|(_, vk)| vk)
        .collect()
}

/// Presses the modifiers and then the key, as input from the keyboard.
fn key_down(key: &Hotkey) {
    let mut inputs: Vec<INPUT> = modifier_keys(key).into_iter().map(|vk| key_input(vk, false)).collect();
    inputs.push(key_input(key.key.0, false));
    unsafe { SendInput(&inputs, size_of::<INPUT>() as i32) };
}

/// Lets go of the key and then of the modifiers.
fn key_up(key: &Hotkey) {
    let mut inputs = vec![key_input(key.key.0, true)];
    inputs.extend(modifier_keys(key).into_iter().rev().map(|vk| key_input(vk, true)));
    unsafe { SendInput(&inputs, size_of::<INPUT>() as i32) };
}

/// The overlay's window and its Direct3D 11 frames. Transparent where nothing is drawn: DWM
/// takes the frame's alpha because the window's glass frame covers it all. It isn't a layered
/// window, which would be click-through but makes Direct3D take a slow path; it shows only while
/// the overlay is open and takes the clicks anyway.
struct Window {
    hwnd: HWND,
    swap_chain: IDXGISwapChain,
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    target: Option<ID3D11RenderTargetView>,
    size: (i32, i32),
    shown: bool,
    /// Since the window last showed: when, the frames drawn, and the time they took, for the log.
    shown_at: Instant,
    frames: u32,
    drawing: Duration,
}

impl Window {
    fn create() -> crate::Result<Self> {
        unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        let instance = unsafe { GetModuleHandleW(None) }?;
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(proc),
            hInstance: instance.into(),
            // Without a cursor of its own, the window keeps whatever cursor was last shown.
            hCursor: unsafe { LoadCursorW(None, IDC_ARROW) }?,
            lpszClassName: w!("steambro-overlay"),
            ..Default::default()
        };
        unsafe { RegisterClassExW(&class) };
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
                w!("steambro-overlay"),
                w!("steambro overlay"),
                WS_POPUP,
                0,
                0,
                1,
                1,
                None,
                None,
                Some(instance.into()),
                None,
            )
        }?;
        unsafe { DwmExtendFrameIntoClientArea(hwnd, &MARGINS { cxLeftWidth: -1, cxRightWidth: -1, cyTopHeight: -1, cyBottomHeight: -1 })? };
        let desc = DXGI_SWAP_CHAIN_DESC {
            BufferDesc: DXGI_MODE_DESC { Format: DXGI_FORMAT_B8G8R8A8_UNORM, ..Default::default() },
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 1,
            OutputWindow: hwnd,
            Windowed: true.into(),
            SwapEffect: DXGI_SWAP_EFFECT_DISCARD,
            Flags: 0,
        };
        let (mut swap_chain, mut device, mut context) = (None, None, None);
        unsafe {
            D3D11CreateDeviceAndSwapChain(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&desc),
                Some(&mut swap_chain),
                Some(&mut device),
                None,
                Some(&mut context),
            )
        }?;
        Ok(Self {
            hwnd,
            swap_chain: swap_chain.ok_or("Direct3D gave no swap chain")?,
            device: device.ok_or("Direct3D gave no device")?,
            context: context.ok_or("Direct3D gave no device context")?,
            target: None,
            size: (1, 1),
            shown: false,
            shown_at: Instant::now(),
            frames: 0,
            drawing: Duration::ZERO,
        })
    }

    /// Shows the window over the game's window, or over the screen in front without one.
    fn show_over(&mut self, game: Option<HWND>) {
        let Some(rect) = game.and_then(client_rect).or_else(|| monitor_rect(unsafe { GetForegroundWindow() })) else { return };
        let size = (rect.right - rect.left, rect.bottom - rect.top);
        if size != self.size {
            self.target = None;
            let resized = unsafe { self.swap_chain.ResizeBuffers(0, size.0 as u32, size.1 as u32, DXGI_FORMAT_UNKNOWN, DXGI_SWAP_CHAIN_FLAG(0)) };
            if let Err(e) = resized {
                crate::log!("overlay: couldn't resize the frames: {e}");
            }
            self.size = size;
        }
        let _ = unsafe { SetWindowPos(self.hwnd, Some(HWND_TOPMOST), rect.left, rect.top, size.0, size.1, SWP_NOACTIVATE | SWP_SHOWWINDOW) };
        if !self.shown {
            (self.shown_at, self.frames, self.drawing) = (Instant::now(), 0, Duration::ZERO);
        }
        self.shown = true;
    }

    fn hide(&mut self) {
        // An empty frame first, so the overlay's last picture doesn't flash when the window shows again.
        self.draw();
        let _ = unsafe { ShowWindow(self.hwnd, SW_HIDE) };
        self.shown = false;
        if self.frames > 0 {
            crate::log!(
                "overlay: shown {:.1}s at {}x{}, {} frames, {:.1} ms each",
                self.shown_at.elapsed().as_secs_f32(),
                self.size.0,
                self.size.1,
                self.frames,
                self.drawing.as_secs_f32() * 1000.0 / self.frames as f32
            );
        }
    }

    /// Shows the window over the game with the focus for a few frames, then hides it and gives
    /// the focus back, so that Steam prepares its overlay for the window. False when Windows
    /// didn't let steambro take the focus.
    fn prepare(&mut self, game: &GameWindows, from: HWND) -> bool {
        self.show_over(game.main_window());
        let focused = self.take_focus();
        if focused {
            for _ in 0..PREPARE_FRAMES {
                self.draw();
            }
            self.give_focus(from, game);
        }
        let _ = unsafe { ShowWindow(self.hwnd, SW_HIDE) };
        self.shown = false;
        focused
    }

    /// Draws an empty, fully transparent frame; the overlay draws itself into it.
    fn draw(&mut self) {
        let begun = Instant::now();
        if self.target.is_none() {
            let buffer: windows::core::Result<ID3D11Texture2D> = unsafe { self.swap_chain.GetBuffer(0) };
            if let Ok(buffer) = buffer {
                let mut target = None;
                if unsafe { self.device.CreateRenderTargetView(&buffer, None, Some(&mut target)) }.is_ok() {
                    self.target = target;
                }
            }
        }
        if let Some(target) = &self.target {
            unsafe {
                self.context.OMSetRenderTargets(Some(&[Some(target.clone())]), None);
                self.context.ClearRenderTargetView(target, &[0.0, 0.0, 0.0, 0.0]);
            }
        }
        let _ = unsafe { self.swap_chain.Present(1, DXGI_PRESENT(0)) };
        self.frames += 1;
        self.drawing += begun.elapsed();
    }

    /// Takes the focus. Windows lets a program take it after its hotkey, or when it joins the
    /// input of the window in front.
    fn take_focus(&self) -> bool {
        if unsafe { SetForegroundWindow(self.hwnd) }.as_bool() {
            return true;
        }
        let front = unsafe { GetWindowThreadProcessId(GetForegroundWindow(), None) };
        let ours = unsafe { GetCurrentThreadId() };
        unsafe {
            let _ = AttachThreadInput(ours, front, true);
            let taken = SetForegroundWindow(self.hwnd).as_bool();
            let _ = AttachThreadInput(ours, front, false);
            taken
        }
    }

    /// Gives the focus to the window that had it, or else to the game's window.
    fn give_focus(&self, from: HWND, game: &GameWindows) {
        let to = if !from.is_invalid() && from != self.hwnd { Some(from) } else { game.main_window() };
        if unsafe { GetForegroundWindow() } == self.hwnd
            && let Some(to) = to
        {
            let _ = unsafe { SetForegroundWindow(to) };
        }
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        let _ = unsafe { DestroyWindow(self.hwnd) };
    }
}

/// A window's client area, in screen pixels.
fn client_rect(hwnd: HWND) -> Option<RECT> {
    let mut rect = RECT::default();
    unsafe { GetClientRect(hwnd, &mut rect) }.ok()?;
    let mut origin = POINT::default();
    unsafe { ClientToScreen(hwnd, &mut origin) }.ok().ok()?;
    let rect = RECT { left: origin.x, top: origin.y, right: origin.x + rect.right, bottom: origin.y + rect.bottom };
    (rect.right > rect.left && rect.bottom > rect.top).then_some(rect)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steam_key_names() {
        let tab = Hotkey::parse("Shift\tKEY_TAB").unwrap();
        assert_eq!((tab.modifiers, tab.key), (MOD_SHIFT, VIRTUAL_KEY(0x09)));
        assert_eq!(tab.display(), "Shift+Tab");
        let f1 = Hotkey::parse("Ctrl\tAlt\tKEY_F1").unwrap();
        assert_eq!((f1.modifiers, f1.key), (MOD_CONTROL | MOD_ALT, VIRTUAL_KEY(0x70)));
        assert_eq!(Hotkey::parse("KEY_F12").unwrap().key, VIRTUAL_KEY(0x7B));
        assert_eq!(Hotkey::parse("KEY_Q").unwrap().key, VIRTUAL_KEY(u16::from(b'Q')));
        assert_eq!(Hotkey::parse("KEY_PAD_5").unwrap().key, VIRTUAL_KEY(0x65));
        assert_eq!(Hotkey::parse("KEY_NONE"), None);
        assert_eq!(Hotkey::parse(""), None);
        assert_eq!(Hotkey::parse("Shift"), None);
    }
}

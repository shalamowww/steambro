//! A Windows notification from the tray app: the balloon of a short-lived notification-area icon,
//! which Windows 10 and 11 show as a toast. Unlike a toast of its own, it needs no registration with
//! Windows, which the portable zip doesn't have. A click on it opens steambro's window.

use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

use tauri::AppHandle;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_LARGE_ICON, NIIF_USER, NIM_ADD, NIM_DELETE, NIM_SETVERSION, NIN_BALLOONHIDE, NIN_BALLOONTIMEOUT,
    NIN_BALLOONUSERCLICK, NOTIFYICON_VERSION_4, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyWindow, DispatchMessageW, HICON, HWND_MESSAGE, IMAGE_ICON, LR_DEFAULTCOLOR, LoadImageW, MSG,
    MsgWaitForMultipleObjects, PM_REMOVE, PeekMessageW, QS_ALLINPUT, RegisterClassW, TranslateMessage, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WNDCLASSW,
};
use windows::core::{PCWSTR, w};

/// The message the icon sends to its window.
const CALLBACK: u32 = WM_APP + 1;
/// The icon stays this long at most, waiting for a click on its notification.
const WAIT: Duration = Duration::from_secs(30);
/// The exe's icon, as tauri-build embeds it.
const APP_ICON: u16 = 32512;

/// What happened to the notification: 0 nothing yet, 1 clicked, 2 gone.
static OUTCOME: AtomicU8 = AtomicU8::new(0);

/// Shows the notification on a thread of its own and returns at once.
pub fn show(app: &AppHandle, title: &str, text: &str) {
    let (app, title, text) = (app.clone(), title.to_string(), text.to_string());
    std::thread::spawn(move || {
        if let Err(e) = run(&app, &title, &text) {
            crate::log!("notification: {e}");
        }
    });
}

fn run(app: &AppHandle, title: &str, text: &str) -> crate::Result<()> {
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let class = WNDCLASSW { lpfnWndProc: Some(window_proc), hInstance: instance.into(), lpszClassName: w!("steambro-notification"), ..Default::default() };
        RegisterClassW(&class);
        let hwnd = CreateWindowExW(WINDOW_EX_STYLE(0), class.lpszClassName, w!("steambro"), WINDOW_STYLE(0), 0, 0, 0, 0, Some(HWND_MESSAGE), None, Some(instance.into()), None)?;
        let icon = LoadImageW(Some(instance.into()), PCWSTR(APP_ICON as usize as *const u16), IMAGE_ICON, 64, 64, LR_DEFAULTCOLOR).map(|h| HICON(h.0)).unwrap_or_default();
        let mut data = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: 1,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_INFO,
            uCallbackMessage: CALLBACK,
            hIcon: icon,
            dwInfoFlags: NIIF_USER | NIIF_LARGE_ICON,
            hBalloonIcon: icon,
            ..Default::default()
        };
        copy(&mut data.szTip, "steambro");
        copy(&mut data.szInfoTitle, title);
        copy(&mut data.szInfo, text);
        OUTCOME.store(0, Ordering::Relaxed);
        let shown = Shell_NotifyIconW(NIM_ADD, &data).as_bool();
        if shown {
            data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
            let _ = Shell_NotifyIconW(NIM_SETVERSION, &data);
            let begun = Instant::now();
            while OUTCOME.load(Ordering::Relaxed) == 0 && begun.elapsed() < WAIT {
                MsgWaitForMultipleObjects(None, false, 500, QS_ALLINPUT);
                let mut msg = MSG::default();
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            let _ = Shell_NotifyIconW(NIM_DELETE, &data);
        }
        let _ = DestroyWindow(hwnd);
        if !icon.is_invalid() {
            let _ = DestroyIcon(icon);
        }
        if !shown {
            return Err("Windows didn't take the notification".into());
        }
    }
    if OUTCOME.load(Ordering::Relaxed) == 1 {
        super::show_window(app);
    }
    Ok(())
}

unsafe extern "system" fn window_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if message == CALLBACK {
        // With version 4, the low word of lparam is the event.
        match (lparam.0 & 0xffff) as u32 {
            NIN_BALLOONUSERCLICK => OUTCOME.store(1, Ordering::Relaxed),
            NIN_BALLOONTIMEOUT | NIN_BALLOONHIDE => OUTCOME.store(2, Ordering::Relaxed),
            _ => {}
        }
        return LRESULT(0);
    }
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

/// Copies text into a fixed buffer, cut to fit, ending with a zero.
fn copy(buffer: &mut [u16], text: &str) {
    let units: Vec<u16> = text.encode_utf16().take(buffer.len() - 1).collect();
    buffer[..units.len()].copy_from_slice(&units);
    buffer[units.len()] = 0;
}

//! The tray icon and its menu. A left click opens the window. The icon shows the cloud's mood.

use serde::Deserialize;
use tauri::image::Image;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, include_image};
use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSMICON};

use super::{autostart, show_window};

/// The cloud's face on the tray icon. The window chooses it; the exe and the window keep the wink.
#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mood {
    Wink,
    Glance,
    Squeeze,
}

/// The tray images from scripts/icons.py, smallest first, at these sizes.
const SIZES: [i32; 4] = [16, 20, 24, 32];
const WINK: [Image<'static>; 4] = [
    include_image!("icons/tray-wink-16.png"),
    include_image!("icons/tray-wink-20.png"),
    include_image!("icons/tray-wink-24.png"),
    include_image!("icons/tray-wink-32.png"),
];
const GLANCE: [Image<'static>; 4] = [
    include_image!("icons/tray-glance-16.png"),
    include_image!("icons/tray-glance-20.png"),
    include_image!("icons/tray-glance-24.png"),
    include_image!("icons/tray-glance-32.png"),
];
const SQUEEZE: [Image<'static>; 4] = [
    include_image!("icons/tray-squeeze-16.png"),
    include_image!("icons/tray-squeeze-20.png"),
    include_image!("icons/tray-squeeze-24.png"),
    include_image!("icons/tray-squeeze-32.png"),
];

pub fn create(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open steambro", true, None::<&str>)?;
    let start_with_windows = CheckMenuItem::with_id(app, "autostart", "Start with Windows", true, autostart::is_enabled(), None::<&str>)?;
    let log = MenuItem::with_id(app, "log", "Open log", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit steambro", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &start_with_windows, &log, &PredefinedMenuItem::separator(app)?, &quit])?;

    TrayIconBuilder::with_id("main")
        .icon(image(Mood::Wink))
        .tooltip("steambro")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(move |app, event| match event.id.as_ref() {
            "open" => show_window(app),
            "autostart" => {
                // The menu has already flipped the check mark; undo it if the registry refused.
                let wanted = start_with_windows.is_checked().unwrap_or(false);
                if let Err(e) = autostart::set(wanted) {
                    let _ = start_with_windows.set_checked(!wanted);
                    crate::show_error(&format!("couldn't change \"Start with Windows\": {e}"));
                }
            }
            "log" => {
                if let Err(e) = open_log() {
                    crate::show_error(&format!("couldn't open the log: {e}"));
                }
            }
            "quit" => {
                crate::log!("quit from the tray");
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                show_window(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

/// Opens the log in the app Windows uses for `.log` files, usually Notepad.
fn open_log() -> crate::Result<()> {
    let path = crate::log::path()?;
    if !path.exists() {
        crate::log!("log opened from the tray");
    }
    std::process::Command::new("explorer.exe").arg(path).spawn()?;
    Ok(())
}

pub fn set_mood(app: &AppHandle, mood: Mood) -> tauri::Result<()> {
    match app.tray_by_id("main") {
        Some(tray) => tray.set_icon(Some(image(mood))),
        None => Ok(()),
    }
}

/// The image for the tray's icon size, which grows with the display scale (16 px at 100 %).
/// Windows scales an image of another size, and a scaled 16 px icon turns blurry.
fn image(mood: Mood) -> Image<'static> {
    let images = match mood {
        Mood::Wink => WINK,
        Mood::Glance => GLANCE,
        Mood::Squeeze => SQUEEZE,
    };
    let wanted = unsafe { GetSystemMetrics(SM_CXSMICON) };
    let index = SIZES.iter().position(|&size| size >= wanted).unwrap_or(SIZES.len() - 1);
    images[index].clone()
}

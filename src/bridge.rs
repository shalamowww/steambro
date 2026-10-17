//! The Steam Input bridge, for games that Steam doesn't see because Windows starts them (Xbox
//! games).
//!
//! `steambro launch` runs inside Steam's launch, so Steam's hook in it shows Steam Input's virtual
//! controllers through XInput. The bridge makes Steam use the game's own layout whatever window is
//! in front (`steam://forceinputappid`), and copies each of Steam's virtual controllers to a
//! virtual Xbox 360 controller from the ViGEmBus driver, which the game reads like a real one.
//!
//! A process only gets the virtual controllers that existed when it started, so a helper process
//! (`steambro bridge-helper`) does the copying, and `steambro launch` starts a new one whenever
//! Steam's list of controllers changes. The game's rumble goes the other way: ViGEmBus reports it
//! for each virtual controller, and the helper passes it to Steam's controller.

use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use vigem_client::{Client, TargetId, XButtons, XGamepad, Xbox360Wired};
use windows::Win32::Foundation::{CloseHandle, WAIT_TIMEOUT};
use windows::Win32::System::Threading::{CREATE_NO_WINDOW, OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject};
use windows::Win32::UI::Input::XboxController::{XINPUT_STATE, XINPUT_VIBRATION, XInputGetState, XInputSetState};

use crate::Worker;
use crate::steam::Steam;

/// How often `steambro launch` reads Steam's list of virtual controllers.
const WATCH: Duration = Duration::from_millis(500);
/// How often the helper copies the controllers: 250 times a second.
const COPY_PERIOD: Duration = Duration::from_millis(4);

/// The bridge while the game runs. When dropped, it stops the helper, which unplugs the virtual
/// controllers, and gives Steam its usual layouts back.
pub struct Bridge {
    _watcher: Worker,
}

impl Bridge {
    /// Starts the bridge when Steam launched steambro; `None` otherwise. Steam uses the game's
    /// layout while it plays (the game's `layout` setting, which the launch checked). `copy` is
    /// the game's `bridge` setting (`Settings::behavior`); without it, or without the driver, the
    /// bridge only makes Steam use the game's layout (`watch`).
    pub fn start(copy: bool) -> Option<Bridge> {
        let Some(app_id) = steam_app_id() else {
            crate::log!("bridge: not started by Steam, so no Steam Input bridge");
            return None;
        };
        let copy = if !copy {
            crate::log!("bridge: copying is off for this game in the settings");
            false
        } else if let Err(e) = Client::connect() {
            crate::log!("bridge: ViGEmBus isn't available ({e:?}), so Steam's controllers can't be copied");
            false
        } else {
            true
        };
        Some(Bridge { _watcher: Worker::spawn(move |stop| watch(app_id, copy, stop)) })
    }
}

/// Follows Steam's list of virtual controllers until `stop`. Steam is made to use the game's
/// layout once it has a controller: without that, Steam's desktop layout would turn the
/// controller into mouse and keyboard on top of the game. With `copy`, each controller that isn't
/// an Xbox controller is copied; the game reads Xbox controllers itself.
fn watch(app_id: u32, copy: bool, stop: &AtomicBool) {
    let mut forced = false;
    let mut helper: Option<Helper> = None;
    let mut copied: Option<Vec<Slot>> = None;
    while !stop.load(Ordering::Relaxed) {
        let slots = steam_slots();
        let to_copy: Vec<Slot> = slots.iter().filter(|s| !s.microsoft).cloned().collect();
        if !forced && !slots.is_empty() {
            force_layout(app_id);
            forced = true;
        }
        if copy {
            if copied.as_ref() != Some(&to_copy) {
                crate::log!("bridge: Steam's controllers to copy: {}", describe(&to_copy));
                if let Some(mut old) = helper.take() {
                    old.stop();
                }
                helper = if to_copy.is_empty() { None } else { Helper::start() };
                copied = Some(to_copy);
            } else if let Some(code) = helper.as_mut().and_then(Helper::ended) {
                crate::log!("bridge: the helper ended with code {code}; trying again when Steam's controllers change");
                helper = None;
            }
        }
        std::thread::sleep(WATCH);
    }
    if let Some(mut helper) = helper {
        helper.stop();
    }
    if forced {
        release_layout(app_id);
    }
}

/// The app ID Steam gave the launch: the game's shortcut. `None` when Steam didn't start steambro.
pub fn steam_app_id() -> Option<u32> {
    std::env::var("SteamAppId").ok().and_then(|v| v.parse::<u32>().ok()).filter(|id| *id != 0)
}

/// Makes Steam use the game's layout and leaves a marker for the tray app, which gives Steam its
/// layouts back if this process is killed (`release_orphans`).
fn force_layout(app_id: u32) {
    match Steam::locate().and_then(|steam| steam.open_url(&format!("steam://forceinputappid/{app_id}"))) {
        Ok(()) => crate::log!("bridge: Steam uses the layout of app {app_id} until the game ends"),
        Err(e) => crate::log!("bridge: couldn't force the layout of app {app_id}: {e}"),
    }
    if let Ok(path) = marker_path(std::process::id()) {
        let _ = std::fs::create_dir_all(path.parent().expect("a marker has a folder"));
        let _ = std::fs::write(path, app_id.to_string());
    }
}

fn release_layout(app_id: u32) {
    match Steam::locate().and_then(|steam| steam.open_url("steam://forceinputappid/0")) {
        Ok(()) => crate::log!("bridge: released the layout of app {app_id}"),
        Err(e) => crate::log!("bridge: couldn't release the layout: {e}"),
    }
    if let Ok(path) = marker_path(std::process::id()) {
        let _ = std::fs::remove_file(path);
    }
}

/// `bridges\<pid>`: a launch with that process ID forced the layout of the app ID in the file.
fn marker_path(pid: u32) -> crate::Result<PathBuf> {
    Ok(crate::data_dir()?.join("bridges").join(pid.to_string()))
}

/// For the tray app: gives Steam its layouts back for launches that ended without doing it, as
/// when the user closes a game from Steam's menu and Steam ends `steambro launch`. When another
/// launch still forces a layout, Steam gets that one again.
pub fn release_orphans() {
    let Ok(dir) = crate::data_dir().map(|d| d.join("bridges")) else { return };
    let Ok(entries) = std::fs::read_dir(&dir) else { return };
    let running: Vec<u32> = crate::process::list().into_iter().filter(|p| p.name.eq_ignore_ascii_case("steambro.exe")).map(|p| p.pid).collect();
    let mut released = false;
    let mut live = None;
    for entry in entries.flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else { continue };
        let app_id = std::fs::read_to_string(entry.path()).ok().and_then(|t| t.trim().parse::<u32>().ok());
        if running.contains(&pid) {
            live = app_id.or(live);
        } else {
            crate::log!("bridge: launch {pid} ended without releasing the layout of app {}", app_id.unwrap_or_default());
            let _ = std::fs::remove_file(entry.path());
            released = true;
        }
    }
    if released && let Ok(steam) = Steam::locate() {
        let _ = steam.open_url(&format!("steam://forceinputappid/{}", live.unwrap_or(0)));
    }
}

/// A running `steambro bridge-helper`.
struct Helper {
    child: Child,
}

impl Helper {
    fn start() -> Option<Helper> {
        let exe = std::env::current_exe().ok()?;
        let spawned = Command::new(exe)
            .args(["bridge-helper", "--parent", &std::process::id().to_string()])
            .creation_flags(CREATE_NO_WINDOW.0)
            .spawn();
        match spawned {
            Ok(child) => Some(Helper { child }),
            Err(e) => {
                crate::log!("bridge: couldn't start the helper: {e}");
                None
            }
        }
    }

    /// The exit code once the helper has ended.
    fn ended(&mut self) -> Option<i32> {
        self.child.try_wait().ok().flatten().map(|status| status.code().unwrap_or(-1))
    }

    /// Ends the helper. Windows closes its connection to ViGEmBus, which unplugs its controllers.
    fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// `steambro bridge-helper`: copies Steam's controllers until the launch that started it ends.
pub fn helper(parent: u32) -> i32 {
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
    let result = copy_controllers(|| unsafe { WaitForSingleObject(parent, 0) } != WAIT_TIMEOUT);
    let _ = unsafe { CloseHandle(parent) };
    match result {
        Ok(()) => 0,
        Err(e) => {
            crate::log!("bridge: {e}");
            1
        }
    }
}

/// Copies each of Steam's virtual controllers to its own virtual Xbox 360 controller, until
/// `done`, and the game's rumble back. Only the controllers that existed when this process started
/// can be read.
fn copy_controllers(done: impl Fn() -> bool) -> crate::Result<()> {
    let client = Client::connect().map_err(|e| format!("ViGEmBus isn't available: {e:?}"))?;
    let slots: Vec<Slot> = steam_slots().into_iter().filter(|s| !s.microsoft).collect();
    let mut pads: [Option<Xbox360Wired<&Client>>; 4] = [None, None, None, None];
    let mut last_packet = [u32::MAX; 4];
    while !done() {
        for slot in &slots {
            let mut state = XINPUT_STATE::default();
            if unsafe { XInputGetState(slot.index, &mut state) } != 0 {
                continue;
            }
            let i = slot.index as usize;
            if pads[i].is_none() {
                let mut pad = plug_in(&client).map_err(|e| format!("couldn't create a virtual controller: {e}"))?;
                crate::log!("bridge: Steam's controller in slot {} ({}) now drives a virtual Xbox 360 controller", slot.index, slot.name);
                pass_rumble(&mut pad, slot.index);
                pads[i] = Some(pad);
            }
            if state.dwPacketNumber != last_packet[i] {
                last_packet[i] = state.dwPacketNumber;
                let g = state.Gamepad;
                let gamepad = XGamepad {
                    buttons: XButtons(g.wButtons.0),
                    left_trigger: g.bLeftTrigger,
                    right_trigger: g.bRightTrigger,
                    thumb_lx: g.sThumbLX,
                    thumb_ly: g.sThumbLY,
                    thumb_rx: g.sThumbRX,
                    thumb_ry: g.sThumbRY,
                };
                if let Some(pad) = pads[i].as_mut()
                    && let Err(e) = pad.update(&gamepad)
                {
                    crate::log!("bridge: couldn't update the virtual controller: {e:?}");
                }
            }
        }
        std::thread::sleep(COPY_PERIOD);
    }
    for pad in pads.iter_mut().flatten() {
        let _ = pad.unplug();
    }
    Ok(())
}

/// Passes the rumble that the game asks of the virtual controller to Steam's controller in `slot`,
/// on a thread that ends when the virtual controller is unplugged.
fn pass_rumble(pad: &mut Xbox360Wired<&Client>, slot: u32) {
    let notifications = match pad.request_notification() {
        Ok(notifications) => notifications,
        Err(e) => {
            crate::log!("bridge: no rumble for slot {slot}: {e:?}");
            return;
        }
    };
    let mut felt = false;
    notifications.spawn_thread(move |_, rumble| {
        // The motors' speeds come as 0 to 255; XInput takes 0 to 65535.
        let vibration = XINPUT_VIBRATION { wLeftMotorSpeed: u16::from(rumble.large_motor) * 257, wRightMotorSpeed: u16::from(rumble.small_motor) * 257 };
        let result = unsafe { XInputSetState(slot, &vibration) };
        if !felt && (rumble.large_motor > 0 || rumble.small_motor > 0) {
            felt = true;
            crate::log!("bridge: the game's rumble goes to Steam's controller in slot {slot} (XInput result {result})");
        }
    });
}

fn plug_in(client: &Client) -> crate::Result<Xbox360Wired<&Client>> {
    let mut pad = Xbox360Wired::new(client, TargetId::XBOX360_WIRED);
    pad.plugin().map_err(|e| format!("{e:?}"))?;
    pad.wait_ready().map_err(|e| format!("{e:?}"))?;
    Ok(pad)
}

/// One of Steam's virtual controllers in this process.
#[derive(Clone, PartialEq, Debug)]
struct Slot {
    /// The XInput slot, 0 to 3.
    index: u32,
    name: String,
    vid: String,
    pid: String,
    /// A Microsoft controller (vendor 0x045e). The bridge doesn't copy these: an Xbox 360
    /// controller there is one of the bridge's own, which copying would loop, and a real Xbox
    /// controller reaches the game natively, which copying would double.
    microsoft: bool,
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

/// Reads `[slot N]` sections with `name=`, `VID=` and `PID=` lines.
fn parse_slots(text: &str) -> Vec<Slot> {
    let mut slots: Vec<Slot> = Vec::new();
    for line in text.lines().map(str::trim) {
        if let Some(index) = line.strip_prefix("[slot ").and_then(|rest| rest.strip_suffix(']')).and_then(|n| n.parse().ok()) {
            slots.push(Slot { index, name: String::new(), vid: String::new(), pid: String::new(), microsoft: false });
        } else if let Some((key, value)) = line.split_once('=')
            && let Some(slot) = slots.last_mut()
        {
            match key {
                "name" => slot.name = value.to_string(),
                "VID" => {
                    slot.vid = value.to_lowercase();
                    slot.microsoft = slot.vid == "0x045e";
                }
                "PID" => slot.pid = value.to_lowercase(),
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

    #[test]
    fn steam_slots_mark_microsoft_controllers() {
        let text = "[slot 0]\nname=Steam Controller\nVID=0x28de\nPID=0x1302\ntype=steam\n[slot 1]\nname=Xbox 360 Controller\nVID=0x045e\nPID=0x028e\n[slot 2]\nname=DualSense\nVID=0x054c\nPID=0x0ce6\n[slot 3]\nname=Xbox Wireless Controller\nVID=0x045E\nPID=0x0b13\n[slot 4]\nname=Fifth\nVID=0x054c\n";
        let slots = parse_slots(text);
        let copied: Vec<u32> = slots.iter().filter(|s| !s.microsoft).map(|s| s.index).collect();
        assert_eq!(copied, [0, 2]);
        assert_eq!(slots.len(), 4);
        assert_eq!(slots[2].name, "DualSense");
        assert!(slots[3].microsoft);
        assert!(parse_slots("").is_empty());
    }
}

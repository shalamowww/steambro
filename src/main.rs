//! steambro: one Windows executable that is the tray app, a small command line (`list`), and the
//! program that each Steam shortcut runs (`launch`). Steam tracks that program as the game, so it
//! must live exactly as long as the game.

// A windowed app, so neither the tray app nor a game started through Steam opens a console.
#![windows_subsystem = "windows"]

mod app;
mod art;
mod bridge;
mod cli;
mod controllers;
mod driver;
mod launch;
mod library;
mod log;
mod overlay;
mod plan;
mod process;
mod prompt;
mod settings;
mod steam;
mod sync;
mod window;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use clap::{Parser, Subcommand};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Your Xbox, Game Pass, Battle.net, Epic, and GOG games in Steam, done properly. Without a command,
/// steambro runs in the tray.
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    /// Start in the tray without opening the window. Used when steambro starts with Windows.
    #[arg(long)]
    background: bool,
    /// Show the question a game launch asks (src/prompt.rs).
    #[arg(long, hide = true)]
    prompt: Option<String>,
}

#[derive(Subcommand)]
enum Command {
    /// List the games found on this PC, where they come from and whether they're in Steam. Changes nothing.
    List {
        /// Also show game keys, install folders, launch commands and artwork files.
        #[arg(short, long)]
        verbose: bool,
    },
    /// Start a game and wait until it has ended. The Steam shortcuts steambro adds run this.
    Launch {
        /// The game's key, as `list -v` shows it, e.g. `xbox:<app id>`.
        game: String,
    },
    /// Copies Steam's controllers for a game launch. `launch` runs this.
    #[command(hide = true)]
    BridgeHelper {
        /// The process ID of the launch; the helper ends with it.
        #[arg(long)]
        parent: u32,
    },
}

fn main() -> ExitCode {
    if std::env::args_os().len() > 1 {
        attach_console();
    }
    let cli = Cli::parse();
    match cli.command {
        None => {
            log::set_role("app");
            app::run(cli.background, cli.prompt);
            ExitCode::SUCCESS
        }
        Some(Command::List { verbose }) => {
            log::set_role("list");
            match cli::list(verbose) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("error: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some(Command::Launch { game }) => {
            log::set_role("launch");
            end_startup_cursor();
            match launch::run(&game) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    crate::log!("couldn't start the game: {e}");
                    // Steam starts this without a console, so the user sees a message box instead.
                    show_error(&format!("steambro couldn't start the game: {e}"));
                    ExitCode::FAILURE
                }
            }
        }
        Some(Command::BridgeHelper { parent }) => {
            log::set_role("bridge");
            end_startup_cursor();
            ExitCode::from(bridge::helper(parent) as u8)
        }
    }
}

/// `%LOCALAPPDATA%\steambro`: steambro's own files: the state, the settings, the art cache, the
/// log, and the files of running launches (their questions and layout markers).
pub fn data_dir() -> Result<PathBuf> {
    // Tests never touch the user's real state.
    if cfg!(test) {
        return Ok(std::env::temp_dir().join("steambro-test").join("data"));
    }
    let dir = std::env::var_os("LOCALAPPDATA").ok_or("LOCALAPPDATA isn't set")?;
    Ok(PathBuf::from(dir).join("steambro"))
}

/// The steambro.exe that Steam's shortcuts and Windows' autostart run. Installed from an MSIX
/// package, steambro's folder changes with each version, so they run the package's app execution
/// alias, `%LOCALAPPDATA%\Microsoft\WindowsApps\steambro.exe`, which stays in place.
pub fn steambro_exe() -> std::io::Result<PathBuf> {
    use windows::Win32::Foundation::APPMODEL_ERROR_NO_PACKAGE;
    use windows::Win32::Storage::Packaging::Appx::GetCurrentPackageFamilyName;
    let mut len = 0;
    let packaged = unsafe { GetCurrentPackageFamilyName(&mut len, None) } != APPMODEL_ERROR_NO_PACKAGE;
    match std::env::var_os("LOCALAPPDATA") {
        Some(dir) if packaged => Ok(PathBuf::from(dir).join(r"Microsoft\WindowsApps\steambro.exe")),
        _ => std::env::current_exe(),
    }
}

/// A thread that runs until it's dropped: dropping it sets the stop flag that the thread watches,
/// and waits for the thread to end.
pub struct Worker {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Worker {
    pub fn spawn(work: impl FnOnce(&AtomicBool) + Send + 'static) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let thread = std::thread::spawn({
            let stop = stop.clone();
            move || work(&stop)
        });
        Self { stop, thread: Some(thread) }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// A windowed app has no console; borrow the terminal's when started from one. Output that is
/// already redirected (a pipe or a file) keeps going there.
fn attach_console() {
    use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
    let _ = unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
}

/// Windows shows its "app starting" cursor after it starts a program, until the program reads its
/// first window message. A launch and the bridge's helper have no window to read messages for, so
/// they read one they send themselves.
fn end_startup_cursor() {
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::WindowsAndMessaging::{GetMessageW, MSG, PM_NOREMOVE, PeekMessageW, PostThreadMessageW, WM_NULL};
    let mut msg = MSG::default();
    unsafe {
        // The first look creates the thread's message queue.
        let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
        if PostThreadMessageW(GetCurrentThreadId(), WM_NULL, Default::default(), Default::default()).is_ok() {
            let _ = GetMessageW(&mut msg, None, 0, 0);
        }
    }
}

fn show_error(message: &str) {
    use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
    use windows::core::{HSTRING, w};
    eprintln!("error: {message}");
    log!("error shown: {message}");
    unsafe { MessageBoxW(None, &HSTRING::from(message), w!("steambro"), MB_OK | MB_ICONERROR) };
}

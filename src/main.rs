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
mod i18n;
mod launch;
mod library;
mod log;
mod overlay;
mod plan;
mod process;
mod prompt;
mod report;
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

/// steambro's version as people see it. The release tag, the installer, the settings and the
/// Microsoft Store all write the same four numbers: Cargo holds the first three, and the fourth
/// is the Store's, always 0.
pub const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), ".0");

/// Your Xbox, Game Pass, Battle.net, Epic, and GOG games in Steam, done properly. Without a command,
/// steambro runs in the tray.
#[derive(Parser)]
#[command(version = VERSION, about)]
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
    /// Write a report for a bug: versions, settings, what they make of each game, and the end of
    /// the logs, with your user name, PC name and Steam account IDs replaced. Prints its path.
    Report,
    /// Copies Steam's controllers for a game launch. `launch` runs this.
    #[command(hide = true)]
    BridgeHelper {
        /// The process ID of the launch; the helper ends with it.
        #[arg(long)]
        parent: u32,
        /// Steam's XInput slots to read, in the order of the launch's virtual controllers.
        #[arg(long, value_delimiter = ',')]
        slots: Vec<u32>,
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
            app::run(cli.background || started_at_sign_in(), cli.prompt);
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
                    show_error(&i18n::text("error.start_game", &[("error", &e.to_string())]));
                    ExitCode::FAILURE
                }
            }
        }
        Some(Command::Report) => {
            log::set_role("report");
            match report::write() {
                Ok(path) => {
                    println!("{}", path.display());
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some(Command::BridgeHelper { parent, slots }) => {
            log::set_role("bridge");
            end_startup_cursor();
            ExitCode::from(bridge::helper(parent, &slots) as u8)
        }
    }
}

/// steambro's own files: the state, the settings, the art cache, the log, and the files of running
/// launches (their questions and layout markers). `%LOCALAPPDATA%\steambro` for a download. With
/// the Microsoft Store package installed, the package's own folder (`package_data_dir`), for every
/// steambro on the PC: a shortcut that still runs a download's exe must read the same settings and
/// leave its questions where the package's tray app looks.
pub fn data_dir() -> Result<PathBuf> {
    // Tests never touch the user's real state.
    if cfg!(test) {
        return Ok(std::env::temp_dir().join("steambro-test").join("data"));
    }
    static DIR: std::sync::OnceLock<std::result::Result<PathBuf, String>> = std::sync::OnceLock::new();
    let found = DIR.get_or_init(|| {
        let download = download_data_dir().map_err(|e| e.to_string())?;
        let package = download.with_file_name("Packages").join(PACKAGE_FAMILY).join("LocalState").join("steambro");
        if packaged() {
            seed(&package, &download);
            Ok(package)
        } else if package.is_dir() {
            Ok(package)
        } else {
            Ok(download)
        }
    });
    found.clone().map_err(|e| format!("steambro has no folder for its files: {e}").into())
}

/// The Store package's family name: the manifest's `Name`, and a hash of its `Publisher`.
const PACKAGE_FAMILY: &str = "BiteMyByte.steambro_ns3jtm0aretew";

fn download_data_dir() -> Result<PathBuf> {
    let dir = std::env::var_os("LOCALAPPDATA").ok_or("LOCALAPPDATA isn't set")?;
    Ok(PathBuf::from(dir).join("steambro"))
}

/// An installed package keeps steambro's files in `steambro` in its local folder,
/// `%LOCALAPPDATA%\Packages\<package>\LocalState`. Windows shows a packaged app a private copy of
/// `%LOCALAPPDATA%` that Explorer and Notepad don't see, so the log and the reports couldn't be
/// opened from there; the package's folder is a real one. Windows keeps it across updates and
/// deletes it when the package is uninstalled or reset. While it doesn't exist, this makes it as a
/// copy of the download's folder, if the PC has one, so the settings and the games already in
/// Steam carry over (after a reinstall too). No log line here: the log's own path comes from
/// `data_dir`.
fn seed(package: &std::path::Path, download: &std::path::Path) {
    if !package.exists() && download.is_dir() {
        // Copied beside it and renamed, so another steambro process never sees half a folder. A
        // download's tray app may be saving a file at this moment, so more than one try.
        let copy = package.with_file_name(format!("steambro-copy-{}", std::process::id()));
        for _ in 0..3 {
            if package.exists() || copy_data(download, &copy).and_then(|()| std::fs::rename(&copy, package)).is_ok() {
                break;
            }
            let _ = std::fs::remove_dir_all(&copy);
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    }
    // A copy that a start left half done, for example at shutdown.
    if package.exists()
        && let Some(entries) = package.parent().and_then(|parent| std::fs::read_dir(parent).ok())
    {
        for entry in entries.flatten().filter(|e| e.file_name().to_string_lossy().starts_with("steambro-copy-")) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Copies steambro's files to a new folder, without what belongs to one run: the logs, the reports,
/// the files of running launches, and a file that is being saved (`<name>.new`). The files in the
/// folder itself (the settings, the state) must all copy; a file in a subfolder (the art cache)
/// that doesn't is left out, as steambro fetches artwork again.
fn copy_data(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    fn copy(from: &std::path::Path, to: &std::path::Path, top: bool) -> std::io::Result<()> {
        std::fs::create_dir_all(to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            let name = entry.file_name();
            let lower = name.to_string_lossy().to_lowercase();
            if lower.ends_with(".new") || top && (lower.ends_with(".log") || ["reports", "bridges", "prompts"].contains(&lower.as_str())) {
                continue;
            }
            if entry.file_type()?.is_dir() {
                let copied = copy(&entry.path(), &to.join(&name), false);
                if top {
                    continue;
                }
                copied?;
            } else if let Err(e) = std::fs::copy(entry.path(), to.join(&name))
                && top
                && e.kind() != std::io::ErrorKind::NotFound
            {
                return Err(e);
            }
        }
        Ok(())
    }
    copy(from, to, true)
}

/// Whether Windows started this process at sign-in, as the package's startup task
/// (`app::autostart`). A startup task takes no arguments, so this stands in for `--background`.
fn started_at_sign_in() -> bool {
    use windows::ApplicationModel::Activation::ActivationKind;
    use windows::ApplicationModel::AppInstance;
    packaged() && std::thread::spawn(|| AppInstance::GetActivatedEventArgs().and_then(|args| args.Kind()).is_ok_and(|kind| kind == ActivationKind::StartupTask)).join().unwrap_or(false)
}

/// The steambro.exe that Steam's shortcuts and a download's autostart run. Installed from an MSIX
/// package, steambro's folder changes with each version, so they run the package's app execution
/// alias, `%LOCALAPPDATA%\Microsoft\WindowsApps\steambro.exe`, which stays in place.
pub fn steambro_exe() -> std::io::Result<PathBuf> {
    match std::env::var_os("LOCALAPPDATA") {
        Some(dir) if packaged() => Ok(PathBuf::from(dir).join(r"Microsoft\WindowsApps\steambro.exe")),
        _ => std::env::current_exe(),
    }
}

/// Whether this process runs from an installed package (an MSIX, as from the Microsoft Store)
/// rather than from a folder. Windows gives a packaged process a package identity.
pub fn packaged() -> bool {
    use windows::Win32::Foundation::APPMODEL_ERROR_NO_PACKAGE;
    use windows::Win32::Storage::Packaging::Appx::GetCurrentPackageFamilyName;
    let mut len = 0;
    unsafe { GetCurrentPackageFamilyName(&mut len, None) != APPMODEL_ERROR_NO_PACKAGE }
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

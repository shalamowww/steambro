//! Processes: starting an exe as a child of `steambro launch`, the running processes (the Windows
//! process snapshot), and the job object that ends a game with the launch.

use std::collections::{HashMap, HashSet};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use windows::Win32::Foundation::{CloseHandle, E_ACCESSDENIED, HANDLE, WAIT_TIMEOUT};
use windows::Win32::Storage::Packaging::Appx::GetPackageFamilyName;
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob, JOB_OBJECT_LIMIT_BREAKAWAY_OK, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation, SetInformationJobObject, TerminateJobObject,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_QUOTA,
    PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, QueryFullProcessImageNameW, TerminateProcess, WaitForMultipleObjects, WaitForSingleObject,
};
use windows::Win32::UI::Shell::{SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, ShellExecuteExW};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{BOOL, HSTRING, PCWSTR, PWSTR};

/// Starts an exe as this process's child, so it's part of Steam's launch. An exe that needs
/// administrator rights, like an old game that Windows' compatibility settings mark so, fails that
/// way (error 740); it starts through Windows' prompt instead. Then it isn't a child, and Steam
/// can't see it. Returns whether it needed the prompt.
pub fn start(exe: &Path, args: &str, dir: &Path) -> crate::Result<bool> {
    let mut command = std::process::Command::new(exe);
    command.current_dir(dir);
    if !args.is_empty() {
        command.raw_arg(args);
    }
    match command.spawn() {
        Ok(_) => Ok(false),
        Err(e) if e.raw_os_error() == Some(740) => {
            crate::log!("{} needs administrator rights; Windows asks", exe.display());
            let (file, params, directory) = (HSTRING::from(exe.as_os_str()), HSTRING::from(args), HSTRING::from(dir.as_os_str()));
            let mut info = SHELLEXECUTEINFOW {
                cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
                fMask: SEE_MASK_NOASYNC,
                lpFile: PCWSTR(file.as_ptr()),
                lpParameters: PCWSTR(params.as_ptr()),
                lpDirectory: PCWSTR(directory.as_ptr()),
                nShow: SW_SHOWNORMAL.0,
                ..Default::default()
            };
            unsafe { ShellExecuteExW(&mut info) }.map_err(|e| format!("{} didn't get administrator rights: {e}", exe.display()))?;
            Ok(true)
        }
        Err(e) => Err(format!("couldn't start {}: {e}", exe.display()).into()),
    }
}

pub struct Process {
    pub pid: u32,
    /// The process that started it. Windows keeps the number after that process ends, and may give
    /// it to a new process.
    pub parent: u32,
    /// The executable's file name, e.g. `steam.exe`.
    pub name: String,
}

/// The running processes, from the Windows process snapshot.
pub fn list() -> Vec<Process> {
    let mut found = Vec::new();
    let Ok(snapshot) = (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }) else { return found };
    let mut entry = PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
    let mut more = unsafe { Process32FirstW(snapshot, &mut entry) }.is_ok();
    while more {
        let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
        found.push(Process { pid: entry.th32ProcessID, parent: entry.th32ParentProcessID, name: String::from_utf16_lossy(&entry.szExeFile[..len]) });
        more = unsafe { Process32NextW(snapshot, &mut entry) }.is_ok();
    }
    let _ = unsafe { CloseHandle(snapshot) };
    found
}

/// Whether a process with this executable file name runs, like `steam.exe`.
pub fn is_running(name: &str) -> bool {
    list().iter().any(|p| p.name.eq_ignore_ascii_case(name))
}

/// The full path of the process's executable. `None` when Windows doesn't let steambro look,
/// as for some system processes.
pub fn image_path(pid: u32) -> Option<PathBuf> {
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut buffer = [0u16; 1024];
    let mut len = buffer.len() as u32;
    let result = unsafe { QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, PWSTR(buffer.as_mut_ptr()), &mut len) };
    let _ = unsafe { CloseHandle(handle) };
    result.ok()?;
    Some(PathBuf::from(String::from_utf16_lossy(&buffer[..len as usize])))
}

/// The family name of the package whose identity the process has, like
/// `4DF9E0F8.Netflix_mcm4njqhnhss8`. `None` for a process without one, which is most desktop
/// programs. A web app from the Store runs as Edge's `pwahelper.exe` with its package's identity.
pub fn package_family(pid: u32) -> Option<String> {
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut buffer = [0u16; 256];
    let mut len = buffer.len() as u32;
    let result = unsafe { GetPackageFamilyName(handle, &mut len, Some(PWSTR(buffer.as_mut_ptr()))) };
    let _ = unsafe { CloseHandle(handle) };
    if result.is_err() || len == 0 {
        return None;
    }
    // The length counts the closing NUL.
    Some(String::from_utf16_lossy(&buffer[..len as usize - 1]))
}

/// Starts an exe the way Explorer starts it when the user opens it: the desktop's own shell runs
/// it (`IShellDispatch2::ShellExecute` on the desktop's folder view), so its parent is Explorer.
/// Steam counts every process that a process of a launch starts as part of the game, so this is
/// how a launch opens something that isn't the game, like a store's app after the game started
/// (`HandOver::Reopen`) or the tray app for a question (prompt.rs). It is outside the launch's job
/// too. `minimized` asks for its first window minimized and not in front; some apps don't follow.
pub fn start_outside_steam(exe: &Path, args: &str, minimized: bool) -> crate::Result<()> {
    use windows::Win32::System::Com::{CLSCTX_LOCAL_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, IDispatch, IServiceProvider};
    use windows::Win32::System::Variant::VARIANT;
    use windows::Win32::UI::Shell::{
        IShellBrowser, IShellDispatch2, IShellFolderViewDual, IShellWindows, SID_STopLevelBrowser, SVGIO_BACKGROUND, SWC_DESKTOP, SWFO_NEEDDISPATCH,
        ShellWindows,
    };
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWMINNOACTIVE;
    use windows::core::{BSTR, Interface};
    // Once per thread; a thread that has COM already keeps it as it is.
    let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    let shell: IShellDispatch2 = unsafe {
        let windows: IShellWindows = CoCreateInstance(&ShellWindows, None, CLSCTX_LOCAL_SERVER)?;
        let (desktop, root) = (VARIANT::from(0i32), VARIANT::default());
        let mut hwnd = 0;
        let found: IDispatch = windows.FindWindowSW(&desktop, &root, SWC_DESKTOP, &mut hwnd, SWFO_NEEDDISPATCH)?;
        let browser: IShellBrowser = found.cast::<IServiceProvider>()?.QueryService(&SID_STopLevelBrowser)?;
        let view: IDispatch = browser.QueryActiveShellView()?.GetItemObject(SVGIO_BACKGROUND)?;
        view.cast::<IShellFolderViewDual>()?.Application()?.cast()?
    };
    let dir = exe.parent().map(|d| d.as_os_str().to_owned()).unwrap_or_default();
    let show = if minimized { SW_SHOWMINNOACTIVE.0 } else { SW_SHOWNORMAL.0 };
    unsafe {
        shell.ShellExecute(
            &BSTR::from(exe.to_string_lossy().as_ref()),
            &VARIANT::from(BSTR::from(args)),
            &VARIANT::from(BSTR::from(dir.to_string_lossy().as_ref())),
            &VARIANT::from(BSTR::from("open")),
            &VARIANT::from(show),
        )
    }
    .map_err(|e| format!("Explorer didn't start {}: {e}", exe.display()))?;
    Ok(())
}

/// The processes of a launch as Steam counts them, as far as steambro can tell: the launch, and
/// every process that one of them started while it ran. Steam shows the game as running until all
/// of them have ended, so the ones left after the game tell why it still shows (launch.rs).
pub struct Family {
    /// Each member's executable file name and the time it started, by process ID. Windows gives
    /// the ID of a process that ended to a new one, and keeps a parent's ID in its children after
    /// the parent ended, so the ID alone doesn't tell a member or its child: the start time does.
    members: HashMap<u32, (String, u64)>,
}

impl Family {
    /// The family of this process.
    pub fn of_this_process() -> Self {
        let pid = std::process::id();
        Self { members: HashMap::from([(pid, ("steambro.exe".to_string(), started_at(pid).unwrap_or(0)))]) }
    }

    /// Adds the processes in `running` that a member started, and forgets the members that have
    /// ended. A process counts as a member's child only when it started after the member did: an
    /// older one has the ID of a parent long gone, which the member now has. A process whose
    /// parent ended before steambro saw it isn't found.
    pub fn update(&mut self, running: &[Process]) {
        let alive: HashSet<u32> = running.iter().map(|p| p.pid).collect();
        // A member's ID that another process has now: the member ended, and that one is no member.
        self.members.retain(|&pid, (_, started)| !alive.contains(&pid) || started_at(pid).is_none_or(|now| now == *started));
        let mut seen: HashSet<u32> = HashSet::new();
        loop {
            let mut new: Vec<(&Process, u64)> = Vec::new();
            for p in running {
                if self.members.contains_key(&p.pid) || seen.contains(&p.pid) {
                    continue;
                }
                let Some((_, parent_started)) = self.members.get(&p.parent) else { continue };
                // Looked at once: one that isn't the member's child stays out.
                seen.insert(p.pid);
                if let Some(started) = started_at(p.pid).filter(|started| started >= parent_started) {
                    new.push((p, started));
                }
            }
            if new.is_empty() {
                break;
            }
            for (p, started) in new {
                self.members.insert(p.pid, (p.name.clone(), started));
            }
        }
        // Not before: a member that just ended may still have started one that runs.
        self.members.retain(|pid, _| alive.contains(pid));
    }

    /// Takes a process in that steambro can't follow from its parents: the store's app, which its
    /// own starter may start again (the Epic Games launcher, through `EOSBootStrapper.exe`).
    pub fn take_in(&mut self, process: &Process) {
        if !self.members.contains_key(&process.pid)
            && let Some(started) = started_at(process.pid)
        {
            self.members.insert(process.pid, (process.name.clone(), started));
        }
    }

    pub fn pids(&self) -> Vec<u32> {
        self.members.keys().copied().collect()
    }

    /// The members other than this process, as `name (pid)`.
    pub fn others(&self) -> Vec<String> {
        let mut others: Vec<String> = self.members.iter().filter(|(pid, _)| **pid != std::process::id()).map(|(pid, (name, _))| format!("{name} ({pid})")).collect();
        others.sort();
        others
    }
}

/// When the process started, as Windows counts time. `None` for one that is gone or that Windows
/// doesn't let steambro look at.
pub fn started_at(pid: u32) -> Option<u64> {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::GetProcessTimes;
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let (mut created, mut exited, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    let result = unsafe { GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) };
    let _ = unsafe { CloseHandle(handle) };
    result.ok()?;
    Some(u64::from(created.dwHighDateTime) << 32 | u64::from(created.dwLowDateTime))
}

/// Handles to a game's processes, so the launch learns at once when one ends instead of at its
/// next look.
#[derive(Default)]
pub struct Exits {
    handles: HashMap<u32, HANDLE>,
}

impl Exits {
    /// Follows these processes from now on and lets go of the others. Returns the ones that still
    /// run: a process that has ended can stay in the process snapshot while a handle to it is open.
    pub fn follow(&mut self, pids: impl IntoIterator<Item = u32>) -> HashSet<u32> {
        let mut running = HashSet::new();
        for pid in pids {
            let handle = match self.handles.get(&pid) {
                Some(&handle) => handle,
                None => match unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) } {
                    Ok(handle) => *self.handles.entry(pid).or_insert(handle),
                    // Windows doesn't let steambro wait for it; it counts as running.
                    Err(_) => {
                        running.insert(pid);
                        continue;
                    }
                },
            };
            if unsafe { WaitForSingleObject(handle, 0) } == WAIT_TIMEOUT {
                running.insert(pid);
            }
        }
        self.handles.retain(|pid, handle| {
            let keep = running.contains(pid);
            if !keep {
                let _ = unsafe { CloseHandle(*handle) };
            }
            keep
        });
        running
    }

    /// Waits until one of the followed processes ends, at most `timeout`.
    pub fn wait(&self, timeout: Duration) {
        // Windows waits for at most 64 at once (MAXIMUM_WAIT_OBJECTS).
        let handles: Vec<HANDLE> = self.handles.values().copied().take(64).collect();
        if handles.is_empty() {
            std::thread::sleep(timeout);
        } else {
            unsafe { WaitForMultipleObjects(&handles, false, timeout.as_millis().try_into().unwrap_or(u32::MAX)) };
        }
    }
}

impl Drop for Exits {
    fn drop(&mut self) {
        for &handle in self.handles.values() {
            let _ = unsafe { CloseHandle(handle) };
        }
    }
}

/// A job object that Windows closes when steambro's process ends, and with it every process in
/// it. `steambro launch` joins it before the store starts the game, so everything the launch
/// starts from then on is in it, a store app too; the game's processes that Windows or a store app
/// outside the launch started are added. So when Steam ends the launch (Exit game, Stop), the
/// game ends too, as Steam's own games do. When the launch ends by itself, dropping the job lifts
/// its limit: a store app left open, or a crash reporter, keeps running.
pub struct Job {
    handle: HANDLE,
    /// Jobs of their own for game processes that Windows or a store already keeps in a job, as
    /// Windows does Xbox games and the Epic Games launcher its games, by process. Such a process can
    /// join only a job that is empty or in its own nest of jobs, and the launch is in the first one.
    others: Vec<(u32, HANDLE)>,
    /// Processes added, or that couldn't be added, by ID and start time (`started_at`), so that a
    /// new process with an old one's ID is tried too; each is tried once.
    tried: HashSet<(u32, u64)>,
}

impl Job {
    /// Creates the job and puts this process in it, so that every process it starts later is in
    /// the job too.
    pub fn new() -> crate::Result<Self> {
        let job = Self { handle: kill_on_close_job()?, others: Vec::new(), tried: HashSet::new() };
        unsafe { AssignProcessToJobObject(job.handle, GetCurrentProcess()) }?;
        Ok(job)
    }

    /// Adds the process, unless it's in the job already. `Ok(false)` when it was tried before.
    pub fn add(&mut self, pid: u32) -> crate::Result<bool> {
        Ok(self.try_add(pid)?.is_some())
    }

    /// Adds a process of the launch's family (`Family`) that left the job: one that started with
    /// `CREATE_BREAKAWAY_FROM_JOB`, as the Epic Games launcher starts from its link. Such a process
    /// would outlive a launch that Steam ends, with Steam's overlay in it from Steam's launch, and
    /// after Steam restarted, the Epic Games launcher's processes with it stalled Steam's start
    /// (2026-11-10). `Ok(true)` when it joined now.
    pub fn adopt(&mut self, pid: u32) -> crate::Result<bool> {
        Ok(self.try_add(pid)? == Some(true))
    }

    /// `None` when the process was tried before; else whether it joined now, rather than being in
    /// the job already.
    fn try_add(&mut self, pid: u32) -> crate::Result<Option<bool>> {
        if !self.tried.insert((pid, started_at(pid).unwrap_or(0))) {
            return Ok(None);
        }
        let process = unsafe { OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }?;
        let result = self.assign(pid, process);
        let _ = unsafe { CloseHandle(process) };
        Ok(Some(result?))
    }

    fn assign(&mut self, pid: u32, process: HANDLE) -> crate::Result<bool> {
        for job in self.jobs() {
            let mut inside = BOOL(0);
            unsafe { IsProcessInJob(process, Some(job), &mut inside) }?;
            if inside.as_bool() {
                return Ok(false);
            }
        }
        match unsafe { AssignProcessToJobObject(self.handle, process) } {
            Err(e) if e.code() == E_ACCESSDENIED => {
                let other = kill_on_close_job()?;
                if let Err(e) = unsafe { AssignProcessToJobObject(other, process) } {
                    let _ = unsafe { CloseHandle(other) };
                    return Err(e.into());
                }
                self.others.push((pid, other));
                Ok(true)
            }
            result => {
                result?;
                Ok(true)
            }
        }
    }

    fn jobs(&self) -> impl Iterator<Item = HANDLE> + '_ {
        std::iter::once(self.handle).chain(self.others.iter().map(|&(_, job)| job))
    }

    /// Ends these processes of the game and waits up to `END_WAIT` for them to end. Each ends
    /// directly, and one that lets no other program end it, as a game with an anti-cheat does,
    /// through the job of its own that it is in (`others`), which Windows ends anyway; that ends
    /// the processes it started too. Returns the ones that still run.
    pub fn end(&self, pids: &[u32]) -> Vec<u32> {
        for &pid in pids {
            let ended = unsafe { OpenProcess(PROCESS_TERMINATE, false, pid) }.and_then(|process| unsafe {
                let result = TerminateProcess(process, 1);
                let _ = CloseHandle(process);
                result
            });
            if ended.is_err()
                && let Some(job) = self.own_job(pid)
            {
                let _ = unsafe { TerminateJobObject(job, 1) };
            }
        }
        let deadline = std::time::Instant::now() + END_WAIT;
        loop {
            let left: Vec<u32> = pids.iter().copied().filter(|&pid| runs(pid)).collect();
            if left.is_empty() || std::time::Instant::now() >= deadline {
                return left;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// The job of its own that the process is in, if it is in one: the one made for it, or for
    /// the process that started it.
    fn own_job(&self, pid: u32) -> Option<HANDLE> {
        self.others.iter().map(|&(_, job)| job).find(|&job| in_job(pid, job))
    }

    /// Whether the process is in the launch's own job, which `end_launch` ends.
    pub fn holds(&self, pid: u32) -> bool {
        in_job(pid, self.handle)
    }

    /// Ends every process of the launch's jobs, this one too: the last way to end a game process
    /// that `end` couldn't.
    pub fn end_launch(&self) -> ! {
        for job in self.jobs().collect::<Vec<_>>().into_iter().rev() {
            let _ = unsafe { TerminateJobObject(job, 1) };
        }
        std::process::exit(1)
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        for job in self.jobs().collect::<Vec<_>>() {
            let _ = set_limits(job, false);
            let _ = unsafe { CloseHandle(job) };
        }
    }
}

/// How long `Job::end` waits for the processes to end.
const END_WAIT: Duration = Duration::from_secs(5);

fn in_job(pid: u32, job: HANDLE) -> bool {
    let Ok(process) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }) else { return false };
    let mut inside = BOOL(0);
    let ok = unsafe { IsProcessInJob(process, Some(job), &mut inside) }.is_ok();
    let _ = unsafe { CloseHandle(process) };
    ok && inside.as_bool()
}

/// Whether the process still runs. One that Windows doesn't let steambro wait for counts as
/// running; one that is gone, or has ended, doesn't.
fn runs(pid: u32) -> bool {
    match unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) } {
        Ok(process) => {
            let running = unsafe { WaitForSingleObject(process, 0) } == WAIT_TIMEOUT;
            let _ = unsafe { CloseHandle(process) };
            running
        }
        Err(e) => e.code() == E_ACCESSDENIED,
    }
}

fn kill_on_close_job() -> crate::Result<HANDLE> {
    let job = unsafe { CreateJobObjectW(None, PCWSTR::null()) }?;
    if let Err(e) = set_limits(job, true) {
        let _ = unsafe { CloseHandle(job) };
        return Err(e);
    }
    Ok(job)
}

/// A process that starts with `CREATE_BREAKAWAY_FROM_JOB`, as some launchers start their game,
/// leaves the job instead of failing to start; the launch adds the game again.
fn set_limits(job: HANDLE, kill_on_close: bool) -> crate::Result<()> {
    let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_BREAKAWAY_OK;
    if kill_on_close {
        info.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    }
    unsafe {
        SetInformationJobObject(job, JobObjectExtendedLimitInformation, (&raw const info).cast(), size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32)
    }?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ended_process_is_seen_at_once() {
        let mut child = std::process::Command::new("ping").args(["-n", "30", "127.0.0.1"]).stdout(std::process::Stdio::null()).spawn().unwrap();
        let mut exits = Exits::default();
        assert!(exits.follow([child.id()]).contains(&child.id()));
        let begun = std::time::Instant::now();
        let killer = std::thread::spawn({
            let pid = child.id();
            move || {
                std::thread::sleep(Duration::from_millis(200));
                let _ = std::process::Command::new("taskkill").args(["/F", "/PID", &pid.to_string()]).stdout(std::process::Stdio::null()).status();
            }
        });
        exits.wait(Duration::from_secs(10));
        let waited = begun.elapsed();
        killer.join().unwrap();
        let _ = child.wait();
        assert!(waited < Duration::from_secs(5), "waited {waited:?}");
        assert!(exits.follow([child.id()]).is_empty(), "the ended process still counts as running");
    }

    #[test]
    fn the_family_has_the_processes_its_members_start() {
        // cmd starts ping, a grandchild of the test.
        let mut child = std::process::Command::new("cmd").args(["/c", "ping -n 5 127.0.0.1 >nul"]).spawn().unwrap();
        std::thread::sleep(Duration::from_millis(500));
        let mut family = Family::of_this_process();
        family.update(&list());
        let others = family.others();
        let _ = std::process::Command::new("taskkill").args(["/F", "/T", "/PID", &child.id().to_string()]).stdout(std::process::Stdio::null()).status();
        let _ = child.wait();
        assert!(others.iter().any(|p| p.starts_with("cmd.exe")) && others.iter().any(|p| p.starts_with("PING.EXE")), "{others:?}");
        std::thread::sleep(Duration::from_millis(300));
        family.update(&list());
        assert!(!family.others().iter().any(|p| p.starts_with("PING.EXE")), "an ended process stays a member");
    }

    /// Windows gives the ID of a process that ended to a new one, so an old process can carry a
    /// member's ID as its parent's. The store's app is a process the launch can't follow from its
    /// parents; `take_in` makes it a member, and it stays one.
    #[test]
    fn a_process_older_than_a_member_isnt_its_child_but_can_be_taken_in() {
        let me = std::process::id();
        let running = list();
        // The process that started the test is older than the test.
        let older = running.iter().find(|p| p.pid == me).map(|p| p.parent).unwrap();
        let name = running.iter().find(|p| p.pid == older).map(|p| p.name.clone()).expect("the test's parent runs");
        let mut claimed: Vec<Process> = running.into_iter().filter(|p| p.pid != older).collect();
        claimed.push(Process { pid: older, parent: me, name: name.clone() });
        let mut family = Family::of_this_process();
        family.update(&claimed);
        assert!(!family.pids().contains(&older), "{name} counts as the test's child");
        family.take_in(&Process { pid: older, parent: 0, name });
        family.update(&claimed);
        assert!(family.pids().contains(&older), "a process taken in doesn't stay a member");
    }

    /// A member's ID that another process has now, by its start time: the member ended.
    #[test]
    fn a_member_whose_id_another_process_has_now_is_forgotten() {
        let me = std::process::id();
        // As if an older process with the test's ID had been the member.
        let mut family = Family { members: HashMap::from([(me, ("old.exe".to_string(), started_at(me).unwrap() - 1))]) };
        family.update(&list());
        assert!(family.pids().is_empty(), "the test counts as the member that had its ID");
    }

    /// A process that starts with `CREATE_BREAKAWAY_FROM_JOB`, as the Epic Games launcher starts
    /// from its link, leaves the launch's job; `adopt` puts it back, once.
    #[test]
    fn a_process_that_left_the_job_is_adopted_once() {
        let mut job = Job::new().unwrap();
        let mut child = std::process::Command::new("ping").args(["-n", "30", "127.0.0.1"]).stdout(std::process::Stdio::null()).creation_flags(0x0100_0000).spawn().unwrap();
        let left = !job.holds(child.id());
        let adopted = job.adopt(child.id());
        let held = job.holds(child.id());
        let again = job.adopt(child.id());
        drop(job);
        let _ = child.kill();
        let _ = child.wait();
        assert!(left, "the process didn't leave the job");
        assert!(adopted.unwrap() && held, "the process wasn't put back");
        assert!(!again.unwrap(), "a process is tried once");
    }

    /// Starts a console program through Explorer; its window opens minimized for a second.
    #[test]
    #[ignore = "opens a console window on the desktop"]
    fn a_process_started_outside_steam_is_explorers() {
        let exe = Path::new(r"C:\Windows\System32\timeout.exe");
        start_outside_steam(exe, "/t 10", true).unwrap();
        std::thread::sleep(Duration::from_secs(1));
        let running = list();
        let started = running.iter().find(|p| p.name.eq_ignore_ascii_case("timeout.exe")).expect("timeout.exe runs");
        let parent = running.iter().find(|p| p.pid == started.parent).map(|p| p.name.clone());
        let _ = std::process::Command::new("taskkill").args(["/F", "/PID", &started.pid.to_string()]).stdout(std::process::Stdio::null()).status();
        assert_eq!(parent.as_deref().map(str::to_lowercase).as_deref(), Some("explorer.exe"));
        let mut family = Family::of_this_process();
        family.update(&running);
        assert!(!family.others().iter().any(|p| p.starts_with("timeout.exe")), "it's in the test's family");
    }

    #[test]
    fn processes_outlive_a_dropped_job() {
        let job = Job::new().unwrap();
        let mut child = std::process::Command::new("ping").args(["-n", "5", "127.0.0.1"]).stdout(std::process::Stdio::null()).spawn().unwrap();
        drop(job);
        std::thread::sleep(std::time::Duration::from_millis(500));
        let running = child.try_wait().unwrap().is_none();
        let _ = child.kill();
        assert!(running, "the job ended the child when it was dropped");
    }

    #[test]
    fn a_process_in_another_job_is_added_too() {
        let mut child = std::process::Command::new("ping").args(["-n", "5", "127.0.0.1"]).stdout(std::process::Stdio::null()).spawn().unwrap();
        // Windows keeps an Xbox game in a job of its own.
        let windows_job = unsafe { CreateJobObjectW(None, PCWSTR::null()) }.unwrap();
        let process = unsafe { OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION, false, child.id()) }.unwrap();
        unsafe { AssignProcessToJobObject(windows_job, process) }.unwrap();
        let mut job = Job::new().unwrap();
        let added = job.add(child.id());
        let again = job.add(child.id());
        let inside = job.jobs().any(|j| {
            let mut inside = BOOL(0);
            unsafe { IsProcessInJob(process, Some(j), &mut inside) }.is_ok() && inside.as_bool()
        });
        drop(job);
        let _ = child.kill();
        let _ = child.wait();
        let _ = unsafe { CloseHandle(process) };
        let _ = unsafe { CloseHandle(windows_job) };
        assert!(added.unwrap(), "the process wasn't added");
        assert!(!again.unwrap(), "a process is tried once");
        assert!(inside, "the process isn't in the launch's jobs");
    }

    /// A game process in a job of Windows' or a store's gets a job of its own from the launch
    /// (`own_job`). `Job::end` ends it, here directly; the job of its own is for a process that
    /// Windows doesn't let steambro end, which a test can't make. The launch goes on.
    #[test]
    fn a_game_in_a_job_of_its_own_ends() {
        let mut child = std::process::Command::new("ping").args(["-n", "30", "127.0.0.1"]).stdout(std::process::Stdio::null()).spawn().unwrap();
        let store_job = unsafe { CreateJobObjectW(None, PCWSTR::null()) }.unwrap();
        let process = unsafe { OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION, false, child.id()) }.unwrap();
        unsafe { AssignProcessToJobObject(store_job, process) }.unwrap();
        let mut job = Job::new().unwrap();
        job.add(child.id()).unwrap();
        let own = job.own_job(child.id()).is_some();
        let left = job.end(&[child.id()]);
        let exited = child.try_wait().unwrap().is_some();
        let _ = child.kill();
        let _ = child.wait();
        let _ = unsafe { CloseHandle(process) };
        let _ = unsafe { CloseHandle(store_job) };
        assert!(own, "the process got no job of its own");
        assert!(left.is_empty() && exited, "the process still runs");
        assert!(!runs(child.id()), "an ended process counts as running");
    }
}

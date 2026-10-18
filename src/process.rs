//! Processes: starting an exe as a child of `steambro launch`, the running processes (the Windows
//! process snapshot), and the job object that ends a game with the launch.

use std::collections::HashSet;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{CloseHandle, E_ACCESSDENIED, HANDLE};
use windows::Win32::Storage::Packaging::Appx::GetPackageFamilyName;
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob, JOB_OBJECT_LIMIT_BREAKAWAY_OK, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation, SetInformationJobObject,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
    QueryFullProcessImageNameW,
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
        found.push(Process { pid: entry.th32ProcessID, name: String::from_utf16_lossy(&entry.szExeFile[..len]) });
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

/// A job object that Windows closes when steambro's process ends, and with it every process in
/// it. `steambro launch` joins it before the store starts the game, so everything the launch
/// starts from then on is in it, a store app too; the game's processes that Windows or a store app
/// outside the launch started are added. So when Steam ends the launch (Exit game, Stop), the
/// game ends too, as Steam's own games do. When the launch ends by itself, dropping the job lifts
/// its limit: a store app left open, or a crash reporter, keeps running.
pub struct Job {
    handle: HANDLE,
    /// Jobs of their own for game processes that Windows already keeps in a job, as it does Xbox
    /// games. Such a process can join only a job that is empty or in its own nest of jobs, and
    /// the launch is in the first one.
    others: Vec<HANDLE>,
    /// Processes added, or that couldn't be added; each is tried once.
    tried: HashSet<u32>,
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
        if !self.tried.insert(pid) {
            return Ok(false);
        }
        let process = unsafe { OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }?;
        let result = self.assign(process);
        let _ = unsafe { CloseHandle(process) };
        result?;
        Ok(true)
    }

    fn assign(&mut self, process: HANDLE) -> crate::Result<()> {
        for &job in std::iter::once(&self.handle).chain(&self.others) {
            let mut inside = BOOL(0);
            unsafe { IsProcessInJob(process, Some(job), &mut inside) }?;
            if inside.as_bool() {
                return Ok(());
            }
        }
        match unsafe { AssignProcessToJobObject(self.handle, process) } {
            Err(e) if e.code() == E_ACCESSDENIED => {
                let other = kill_on_close_job()?;
                // Kept even if the process can't join it, so that dropping the job closes it.
                self.others.push(other);
                unsafe { AssignProcessToJobObject(other, process) }?;
                Ok(())
            }
            result => Ok(result?),
        }
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        for &job in std::iter::once(&self.handle).chain(&self.others) {
            let _ = set_limits(job, false);
            let _ = unsafe { CloseHandle(job) };
        }
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
        let inside = std::iter::once(&job.handle).chain(&job.others).any(|&j| {
            let mut inside = BOOL(0);
            unsafe { IsProcessInJob(process, Some(j), &mut inside) }.is_ok() && inside.as_bool()
        });
        drop(job);
        let _ = child.kill();
        let _ = child.wait();
        let _ = unsafe { CloseHandle(process) };
        let _ = unsafe { CloseHandle(windows_job) };
        added.unwrap();
        assert!(inside, "the process isn't in the launch's jobs");
    }
}

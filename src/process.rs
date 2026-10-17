//! Running processes, through the Windows process snapshot, and a job object that ends a game
//! with `steambro launch`.

use std::collections::HashSet;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{CloseHandle, HANDLE};
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
/// way (error 740); it starts through Windows' prompt instead, and isn't a child then. Returns
/// whether it needed the prompt.
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
    /// Processes added, or that couldn't be added; each is tried once.
    tried: HashSet<u32>,
}

impl Job {
    pub fn new() -> crate::Result<Self> {
        let handle = unsafe { CreateJobObjectW(None, PCWSTR::null()) }?;
        let job = Self { handle, tried: HashSet::new() };
        job.set_kill_on_close(true)?;
        unsafe { AssignProcessToJobObject(handle, GetCurrentProcess()) }?;
        Ok(job)
    }

    /// Adds the process, unless it's in the job already. `Ok(false)` when it was tried before. A
    /// process can be in several nested jobs, so a game that Windows already keeps in one can
    /// still be added.
    pub fn add(&mut self, pid: u32) -> crate::Result<bool> {
        if !self.tried.insert(pid) {
            return Ok(false);
        }
        let process = unsafe { OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }?;
        let mut inside = BOOL(0);
        let result = unsafe { IsProcessInJob(process, Some(self.handle), &mut inside) }
            .and_then(|()| if inside.as_bool() { Ok(()) } else { unsafe { AssignProcessToJobObject(self.handle, process) } });
        let _ = unsafe { CloseHandle(process) };
        result?;
        Ok(true)
    }

    /// A process that starts with `CREATE_BREAKAWAY_FROM_JOB`, as some launchers start their
    /// game, leaves the job instead of failing to start; the launch adds the game again.
    fn set_kill_on_close(&self, on: bool) -> crate::Result<()> {
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_BREAKAWAY_OK;
        if on {
            info.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        }
        unsafe {
            SetInformationJobObject(
                self.handle,
                JobObjectExtendedLimitInformation,
                (&raw const info).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        }?;
        Ok(())
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        let _ = self.set_kill_on_close(false);
        let _ = unsafe { CloseHandle(self.handle) };
    }
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
}

//! Own a child process and its descendants, with OS cleanup on Windows parent exit.

use std::{
    io,
    ops::{Deref, DerefMut},
    process::{self, Command},
};

pub struct Child {
    inner: process::Child,
    #[cfg(windows)]
    job: std::os::windows::io::OwnedHandle,
}

impl Child {
    pub fn spawn(command: &mut Command) -> io::Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
            Ok(Self {
                inner: command.spawn()?,
            })
        }
        #[cfg(windows)]
        {
            windows::spawn(command)
        }
    }

    /// Stop the entire owned tree, rather than only its root process.
    pub fn kill(&mut self) -> io::Result<()> {
        #[cfg(unix)]
        {
            unsafe extern "C" {
                fn kill(pid: i32, signal: i32) -> i32;
            }
            // spawn creates a private process group whose id is the child's pid.
            // SIGKILL is 9 on supported Unix systems. A negative pid targets the group.
            if unsafe { kill(-(self.inner.id() as i32), 9) } == 0 {
                return Ok(());
            }
            self.inner.kill()
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::System::JobObjects::TerminateJobObject;
            // The handle belongs to this Child and remains valid for this call.
            if unsafe { TerminateJobObject(self.job.as_raw_handle(), 1) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }
    }
}

impl Deref for Child {
    type Target = process::Child;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl DerefMut for Child {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        // A successful child can also leave descendants holding its pipes open.
        let _ = self.kill();
        let _ = self.inner.wait();
    }
}

#[cfg(windows)]
mod windows {
    use super::*;
    use std::{
        mem::{size_of, zeroed},
        os::windows::{
            io::{AsRawHandle, FromRawHandle, OwnedHandle},
            process::CommandExt,
        },
        ptr::null,
    };
    use windows_sys::Win32::{
        Foundation::{HANDLE, INVALID_HANDLE_VALUE},
        System::{
            Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
            },
            JobObjects::{
                AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation, SetInformationJobObject,
            },
            Threading::{CREATE_SUSPENDED, OpenThread, ResumeThread, THREAD_SUSPEND_RESUME},
        },
    };

    fn owned(handle: HANDLE) -> io::Result<OwnedHandle> {
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        // The API returned a new owned handle; OwnedHandle closes it exactly once.
        Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
    }

    pub(super) fn spawn(command: &mut Command) -> io::Result<Child> {
        // Create/configure the job before spawning so any setup failure is harmless.
        let job = owned(unsafe { CreateJobObjectW(null(), null()) })?;
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if unsafe {
            SetInformationJobObject(
                job.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const _,
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        // The suspended root cannot spawn a child before joining our job.
        let mut inner = command.creation_flags(CREATE_SUSPENDED).spawn()?;
        let setup = (|| {
            if unsafe { AssignProcessToJobObject(job.as_raw_handle(), inner.as_raw_handle()) } == 0 {
                return Err(io::Error::last_os_error());
            }
            resume(inner.id())
        })();
        if let Err(error) = setup {
            let _ = inner.kill();
            let _ = inner.wait();
            return Err(error);
        }
        Ok(Child { inner, job })
    }

    fn resume(pid: u32) -> io::Result<()> {
        // std::process::Child exposes the process handle but not the primary thread.
        // The root is suspended and has one thread; find and resume that thread.
        let snapshot = owned(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) })?;
        let mut entry: THREADENTRY32 = unsafe { zeroed() };
        entry.dwSize = size_of::<THREADENTRY32>() as u32;
        let mut found = unsafe { Thread32First(snapshot.as_raw_handle(), &mut entry) };
        while found != 0 {
            if entry.th32OwnerProcessID == pid {
                let thread = owned(unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) })?;
                if unsafe { ResumeThread(thread.as_raw_handle()) } == u32::MAX {
                    return Err(io::Error::last_os_error());
                }
                return Ok(());
            }
            found = unsafe { Thread32Next(snapshot.as_raw_handle(), &mut entry) };
        }
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "child primary thread was not found",
        ))
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader},
        os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
        process::Stdio,
    };
    use windows_sys::Win32::{
        Foundation::WAIT_OBJECT_0,
        System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject},
    };

    #[test]
    fn killing_a_job_stops_its_descendants() {
        let script = "$child = Start-Process powershell.exe -ArgumentList '-NoProfile','-Command','Start-Sleep -Seconds 30' -PassThru; Write-Output $child.Id; Start-Sleep -Seconds 30";
        let mut child = Child::spawn(
            Command::new("powershell.exe")
                .args(["-NoProfile", "-NonInteractive", "-Command", script])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
        )
        .unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let pid: u32 = line.trim().parse().unwrap();
        // Open before termination so the test observes this process, even if its id is reused.
        let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
        assert!(!handle.is_null());
        let descendant = unsafe { OwnedHandle::from_raw_handle(handle) };
        child.kill().unwrap();
        assert_eq!(
            unsafe { WaitForSingleObject(descendant.as_raw_handle(), 5000) },
            WAIT_OBJECT_0
        );
        child.wait().unwrap();
    }
}

use std::io;
use tokio::process::{Child, Command};

#[cfg(windows)]
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};

// The OS closes the job handle even when QuickFetch exits unexpectedly.
pub(crate) struct ProcessTree {
    #[cfg(windows)]
    handle: OwnedHandle,
}

impl ProcessTree {
    pub(crate) fn spawn(command: &mut Command) -> io::Result<(Child, Self)> {
        let tree = Self::new()?;
        let mut child = command.kill_on_drop(true).spawn()?;
        if let Err(error) = tree.attach(&child) {
            let _ = child.start_kill();
            return Err(error);
        }
        Ok((child, tree))
    }

    #[cfg(windows)]
    fn new() -> io::Result<Self> {
        use windows_sys::Win32::System::JobObjects::{
            CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };

        // Null arguments create an unnamed, non-inheritable job handle.
        let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if raw.is_null() {
            return Err(io::Error::last_os_error());
        }
        // CreateJobObjectW returned a new handle owned by this process.
        let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // The information class, buffer and buffer length match the Win32 ABI.
        let configured = unsafe {
            SetInformationJobObject(
                handle.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of_val(&limits) as u32,
            )
        };
        if configured == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { handle })
    }

    #[cfg(windows)]
    fn attach(&self, child: &Child) -> io::Result<()> {
        use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;

        let process = child.raw_handle().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "Download process already exited")
        })?;
        // Both handles remain live for the duration of the call.
        if unsafe { AssignProcessToJobObject(self.handle.as_raw_handle(), process) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    #[cfg(not(windows))]
    fn new() -> io::Result<Self> {
        Ok(Self {})
    }

    #[cfg(not(windows))]
    fn attach(&self, _: &Child) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::{process::Stdio, time::Duration};
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

    #[tokio::test]
    async fn closing_job_ends_parent_and_descendant() {
        let mut command = Command::new("cmd.exe");
        command
            .args(["/d", "/c", "ping.exe -t 127.0.0.1"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        crate::hidden_tokio(&mut command);
        let (mut child, tree) = ProcessTree::spawn(&mut command).unwrap();
        let mut output = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        let bytes = tokio::time::timeout(Duration::from_secs(8), output.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        assert!(bytes > 0, "ping must start and inherit the pipe");
        assert!(
            child.try_wait().unwrap().is_none(),
            "parent must still be running"
        );
        drop(tree);
        tokio::time::timeout(Duration::from_secs(8), child.wait())
            .await
            .unwrap()
            .unwrap();
        // EOF proves the descendant also closed its inherited pipe.
        tokio::time::timeout(Duration::from_secs(8), output.read_to_end(&mut Vec::new()))
            .await
            .unwrap()
            .unwrap();
    }
}

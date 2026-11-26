//! Pinned process liveness for discovery fixtures, independent of their metadata.

#[cfg(target_os = "linux")]
mod native {
    use std::fs::File;
    use std::os::fd::OwnedFd;
    use std::os::unix::fs::FileExt;
    use std::path::Path;

    use super::super::linux::{open_pidfd, pidfd_is_alive};

    pub struct ProcessLiveness(Handle);

    enum Handle {
        PidFd(OwnedFd),
        ProcStat(File),
    }

    impl ProcessLiveness {
        pub fn open(pid: u32) -> Option<Self> {
            if let Some(fd) = open_pidfd(pid) {
                return Some(Self(Handle::PidFd(fd)));
            }
            // Older kernels and sandbox policies can deny pidfd_open. An
            // already opened proc file still refers to the original task;
            // reopening this path on each poll would follow a recycled PID.
            let path = Path::new("/proc").join(pid.to_string()).join("stat");
            File::open(path)
                .ok()
                .map(|file| Self(Handle::ProcStat(file)))
        }

        pub fn is_alive(&self) -> bool {
            match &self.0 {
                Handle::PidFd(fd) => pidfd_is_alive(fd),
                Handle::ProcStat(file) => proc_stat_is_alive(file),
            }
        }
    }

    fn proc_stat_is_alive(file: &File) -> bool {
        let mut stat = [0; 1024];
        let Ok(len) = file.read_at(&mut stat, 0) else {
            return false;
        };
        let stat = &stat[..len];
        // Field 3 follows the parenthesized comm, which may contain ')'.
        let Some(close) = stat.iter().rposition(|&byte| byte == b')') else {
            return false;
        };
        stat.get(close + 2)
            .is_some_and(|&state| state != b'Z' && state != b'X')
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn retained_stat_does_not_follow_a_reused_pid() {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("stat");
            std::fs::write(&path, b"42 (fixture) S\n").unwrap();
            let original = ProcessLiveness(Handle::ProcStat(File::open(&path).unwrap()));
            assert!(original.is_alive());

            std::fs::write(&path, b"42 (fixture) Z\n").unwrap();
            assert!(!original.is_alive());
            std::fs::remove_file(&path).unwrap();
            std::fs::write(&path, b"42 (fixture) S\n").unwrap();
            let replacement = ProcessLiveness(Handle::ProcStat(File::open(&path).unwrap()));
            assert!(replacement.is_alive());
            assert!(!original.is_alive());
        }
    }
}

#[cfg(windows)]
mod native {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::{
        Foundation::WAIT_TIMEOUT,
        System::Threading::{OpenProcess, WaitForSingleObject},
    };

    /// The standard access right to wait on the process object.
    const SYNCHRONIZE: u32 = 0x0010_0000;

    pub struct ProcessLiveness(OwnedHandle);

    impl ProcessLiveness {
        pub fn open(pid: u32) -> Option<Self> {
            // SAFETY: no pointers are passed, and inheritance is disabled.
            let raw = unsafe { OpenProcess(SYNCHRONIZE, 0, pid) };
            if raw.is_null() {
                return None;
            }
            // SAFETY: OpenProcess returned a uniquely owned process handle.
            Some(Self(unsafe { OwnedHandle::from_raw_handle(raw) }))
        }

        pub fn is_alive(&self) -> bool {
            // SAFETY: the retained handle has SYNCHRONIZE access and the wait
            // is nonblocking. A signaled process has exited, even with code 259.
            (unsafe { WaitForSingleObject(self.0.as_raw_handle(), 0) }) == WAIT_TIMEOUT
        }
    }
}

pub use native::ProcessLiveness;

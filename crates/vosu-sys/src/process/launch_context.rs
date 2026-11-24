//! Bounded, selected-process launch facts; never inherits daemon context.
use super::MemoryError;
use std::{ffi::OsString, path::PathBuf};

pub const MAX_ENVIRONMENT_BYTES: usize = 1024 * 1024;
#[derive(Debug, Clone)]
pub struct LaunchContext {
    pub cwd: PathBuf,
    pub environment: Vec<(OsString, OsString)>,
}
fn invalid() -> MemoryError {
    MemoryError::Corrupt {
        context: "process launch context",
        address: 0,
        message: "missing, malformed or oversized launch context",
    }
}
pub fn launch_context(pid: u32) -> Result<LaunchContext, MemoryError> {
    if pid == 0 {
        return Err(invalid());
    }
    #[cfg(target_os = "linux")]
    {
        use std::{io::Read, os::unix::ffi::OsStringExt};
        let root = PathBuf::from("/proc").join(pid.to_string());
        let cwd = std::fs::read_link(root.join("cwd")).map_err(|_| invalid())?;
        let mut bytes = Vec::new();
        std::fs::File::open(root.join("environ"))
            .map_err(|_| invalid())?
            .take((MAX_ENVIRONMENT_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| invalid())?;
        if bytes.is_empty()
            || bytes.len() > MAX_ENVIRONMENT_BYTES
            || bytes.last() != Some(&0)
            || !cwd.is_absolute()
        {
            return Err(invalid());
        }
        let environment = bytes[..bytes.len() - 1]
            .split(|b| *b == 0)
            .map(|entry| {
                let split = entry
                    .iter()
                    .position(|b| *b == b'=')
                    .filter(|p| *p > 0)
                    .ok_or_else(invalid)?;
                Ok((
                    OsString::from_vec(entry[..split].to_vec()),
                    OsString::from_vec(entry[split + 1..].to_vec()),
                ))
            })
            .collect::<Result<Vec<_>, MemoryError>>()?;
        Ok(LaunchContext { cwd, environment })
    }
    #[cfg(windows)]
    {
        super::windows::launch_context(pid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader, Write},
        process::{Command, Stdio},
        sync::mpsc,
        time::Duration,
    };
    #[test]
    fn captures_selected_child_context_and_rejects_missing_pid() {
        let root = tempfile::tempdir().unwrap();
        #[cfg(target_os = "linux")]
        let mut command = {
            let mut c = Command::new("/bin/sh");
            c.args(["-c", "echo ready; read answer"]);
            c
        };
        #[cfg(windows)]
        let mut command = {
            let mut c = Command::new("cmd.exe");
            c.args(["/D", "/Q", "/C", "echo ready & set /p answer="]);
            c
        };
        let mut child = command
            .current_dir(root.path())
            .env("VOSU_CONTEXT_SENTINEL", "selected-child-7319")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::sync_channel(1);
        let reader = std::thread::spawn(move || {
            let mut line = String::new();
            let result = BufReader::new(stdout).read_line(&mut line);
            let _ = tx.try_send((result, line));
        });
        let ready = rx.recv_timeout(Duration::from_secs(5));
        let captured = launch_context(child.id());
        let _ = child.stdin.take().unwrap().write_all(b"done\n");
        let _ = child.kill();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while child.try_wait().unwrap().is_none() {
            assert!(std::time::Instant::now() < deadline, "child did not exit");
            std::thread::yield_now();
        }
        while !reader.is_finished() {
            assert!(std::time::Instant::now() < deadline, "reader did not exit");
            std::thread::yield_now();
        }
        reader.join().unwrap();
        let (result, line) = ready.unwrap();
        result.unwrap();
        assert!(line.contains("ready"));
        let context = captured.unwrap();
        assert_eq!(
            std::fs::canonicalize(context.cwd).unwrap(),
            std::fs::canonicalize(root.path()).unwrap()
        );
        assert!(
            context
                .environment
                .contains(&("VOSU_CONTEXT_SENTINEL".into(), "selected-child-7319".into()))
        );
        assert!(launch_context(u32::MAX).is_err());
        assert!(launch_context(0).is_err());
    }
}

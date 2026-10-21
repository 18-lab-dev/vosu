use super::{Entry, ToolHost};
use std::{
    io,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
};
use tokio_util::sync::CancellationToken;

pub fn executable(root: &Path, command: &str, dev: bool) -> PathBuf {
    let name = PathBuf::from(command);
    #[cfg(windows)]
    let name = {
        let mut name = name;
        if name.extension().is_none() {
            name.set_extension("exe");
        }
        name
    };
    if !command.contains(['/', '\\']) {
        return name;
    }
    let direct = root.join(&name);
    if direct.is_file() || !dev {
        return direct;
    }
    let file = name.file_name().unwrap_or_default();
    for ancestor in root.ancestors() {
        if ancestor.join("Cargo.toml").is_file() {
            let target = std::env::var_os("CARGO_TARGET_DIR")
                .map(PathBuf::from)
                .map(|p| if p.is_absolute() { p } else { ancestor.join(p) })
                .unwrap_or_else(|| ancestor.join("target"));
            for profile in ["debug", "release"] {
                let candidate = target.join(profile).join(file);
                if candidate.is_file() {
                    return candidate;
                }
            }
        }
    }
    direct
}

#[cfg(windows)]
struct Job(windows_sys::Win32::Foundation::HANDLE);
#[cfg(windows)]
unsafe impl Send for Job {}
#[cfg(windows)]
impl Job {
    fn assign(child: &tokio::process::Child) -> io::Result<Self> {
        use windows_sys::Win32::System::JobObjects::*;
        // SAFETY: all pointers refer to live initialized structures; the job owns
        // its handle, while the child process handle is borrowed until assignment.
        unsafe {
            let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if handle.is_null() {
                return Err(io::Error::last_os_error());
            }
            let job = Self(handle);
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of_val(&limits) as u32,
            ) == 0
                || AssignProcessToJobObject(
                    handle,
                    child
                        .raw_handle()
                        .ok_or_else(|| io::Error::other("missing child handle"))?
                        as _,
                ) == 0
            {
                drop(job);
                return Err(io::Error::last_os_error());
            }
            Ok(job)
        }
    }
}
#[cfg(windows)]
impl Drop for Job {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

pub async fn supervise(host: Arc<ToolHost>, entry: Arc<Entry>, stop: CancellationToken) {
    let mut crashes = std::collections::VecDeque::new();
    let mut attempt = 0_u32;
    loop {
        if stop.is_cancelled() {
            break;
        }
        match launch(&host, &entry, &stop).await {
            Ok(true) => break,
            result => {
                entry.log("host", &format!("service exited unexpectedly: {result:?}"));
                let now = std::time::Instant::now();
                crashes.retain(|at| now.duration_since(*at) < Duration::from_secs(300));
                crashes.push_back(now);
                attempt += 1;
                {
                    let mut state = entry.state.lock();
                    state.port = None;
                    state.token = None;
                    state.pid = None;
                    state.restarts += 1;
                    state.status = if crashes.len() >= 5 {
                        "failed"
                    } else {
                        "starting"
                    }
                    .into();
                    state.detail = Some("service crashed".into());
                }
                entry.ready.notify_waiters();
                if crashes.len() >= 5 {
                    return;
                }
                tokio::select! { () = stop.cancelled() => break, () = tokio::time::sleep(Duration::from_secs(1 << (attempt - 1).min(4))) => {} }
            }
        }
    }
    let mut state = entry.state.lock();
    state.port = None;
    state.token = None;
    state.pid = None;
    state.status = "stopped".into();
    entry.ready.notify_waiters();
}

async fn launch(host: &ToolHost, entry: &Arc<Entry>, stop: &CancellationToken) -> io::Result<bool> {
    let manifest = entry
        .manifest
        .as_ref()
        .ok_or_else(|| io::Error::other("invalid manifest"))?;
    let service = manifest
        .service
        .as_ref()
        .ok_or_else(|| io::Error::other("no service"))?;
    let token = super::secret()?;
    std::fs::create_dir_all(&entry.data_dir)?;
    std::fs::create_dir_all(&entry.cache_dir)?;
    let mut command = Command::new(executable(
        &entry.root,
        &service.command[0],
        entry.source == "dev",
    ));
    command
        .args(&service.command[1..])
        .current_dir(&entry.root)
        .env("VOSU_TOOL_PROTOCOL", "1")
        .env("VOSU_TOOL_INTERFACE", "service")
        .env("VOSU_TOOL_ID", &manifest.id)
        .env("VOSU_TOOL_TOKEN", &token)
        .env("VOSU_URL", host.overlay_url())
        .env("VOSU_TOOL_DATA_DIR", &entry.data_dir)
        .env("VOSU_TOOL_CACHE_DIR", &entry.cache_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(target_os = "linux")]
    unsafe {
        // SAFETY: this post-fork closure calls only async-signal-safe libc calls.
        // Recheck parent after prctl to close the parent-death-before-prctl race.
        let parent = libc::getpid();
        command.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) == -1 {
                return Err(io::Error::last_os_error());
            }
            if libc::getppid() != parent {
                libc::raise(libc::SIGKILL);
            }
            Ok(())
        });
    }
    let mut child = command.spawn()?;
    #[cfg(windows)]
    let _job = Job::assign(&child)?;
    {
        let mut state = entry.state.lock();
        state.pid = child.id();
        state.token = Some(token.clone());
        state.started_at = Some(chrono::Utc::now().to_rfc3339());
    }
    let mut stdin = child.stdin.take();
    let (ready_tx, mut ready_rx) = tokio::sync::mpsc::channel(1);
    let stdout = tokio::spawn(capture(
        child.stdout.take().unwrap(),
        entry.clone(),
        "stdout",
        Some(ready_tx),
    ));
    let stderr = tokio::spawn(capture(
        child.stderr.take().unwrap(),
        entry.clone(),
        "stderr",
        None,
    ));
    let ready = tokio::select! {
        () = stop.cancelled() => None,
        result = tokio::time::timeout(Duration::from_secs(20), ready_rx.recv()) => result.ok().flatten(),
        _ = child.wait() => None,
    };
    let healthy = if let Some(port) = ready {
        let result = host
            .http
            .get(format!("http://127.0.0.1:{port}/healthz"))
            .header("x-vosu-tool-token", &token)
            .timeout(Duration::from_secs(3))
            .send()
            .await;
        if result.is_ok_and(|r| r.status().is_success()) {
            let mut state = entry.state.lock();
            state.port = Some(port);
            state.status = "running".into();
            state.detail = None;
            entry.ready.notify_waiters();
            true
        } else {
            false
        }
    } else {
        false
    };
    let orderly = if healthy {
        tokio::select! { () = stop.cancelled() => true, _ = child.wait() => false }
    } else {
        stop.is_cancelled()
    };
    stdin.take();
    if tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .is_err()
    {
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
    stdout.abort();
    stderr.abort();
    let _ = stdout.await;
    let _ = stderr.await;
    if !healthy && !orderly {
        return Err(io::Error::other("readiness handshake or /healthz failed"));
    }
    Ok(orderly)
}

async fn capture<R: AsyncRead + Unpin>(
    mut reader: R,
    entry: Arc<Entry>,
    stream: &'static str,
    ready: Option<tokio::sync::mpsc::Sender<u16>>,
) {
    let mut chunk = [0_u8; 4096];
    let mut line = Vec::new();
    let mut truncated = false;
    while let Ok(n) = reader.read(&mut chunk).await {
        if n == 0 {
            break;
        }
        for byte in &chunk[..n] {
            if *byte == b'\n' {
                let text = String::from_utf8_lossy(&line);
                if !truncated
                    && let Some(port) = text
                        .strip_prefix("vosu-tool-ready ")
                        .and_then(|v| v.trim().parse::<u16>().ok())
                        .filter(|p| *p != 0)
                    && let Some(tx) = &ready
                {
                    let _ = tx.try_send(port);
                } else {
                    entry.log(stream, &text);
                }
                line.clear();
                truncated = false;
            } else if line.len() < 16 * 1024 {
                line.push(*byte);
            } else {
                truncated = true;
            }
        }
    }
    if !line.is_empty() {
        entry.log(stream, &String::from_utf8_lossy(&line));
    }
}

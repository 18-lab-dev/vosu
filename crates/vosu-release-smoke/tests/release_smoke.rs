#![expect(
    clippy::disallowed_methods,
    reason = "test fixtures write scratch files and sleep"
)]

use std::{
    fs,
    io::{Read as _, Write as _},
    net::{Ipv4Addr, SocketAddrV4, TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use vosu_proptest::bounded;

use tempfile::TempDir;

/// The test-harness adapter capture endpoint (GOLDEN_CONTRACT.md §5), which
/// `build.rs` keeps out of every release build.
const HARNESS_PATH: &str = "/__vosu_test/v1/adapter-capture?schema=v2";
const HARNESS_HEADER: &str = "x-vosu-test-capture";
const HARNESS_CAPABILITY_ENV: &str = "VOSU_TEST_ADAPTER_CAPTURE";
const HARNESS_CAPABILITY: &str = "3e1f0a9b8c7d6e5f4a3b2c1d0e9f8a7b6c5d4e3f2a1b0c9d8e7f6a5b4c3d2e1f";

/// Exact production bundled plugin set (vosu 2.0); `vosu.testkit.echo` must
/// never appear in a production release binary.
const EXPECTED_BUNDLED_PLUGIN_IDS: [&str; 7] = [
    "vosu.osu",
    "vosu.adapter.upstream",
    "vosu.adapter.gosumemory",
    "vosu.adapter.streamcompanion",
    "vosu.dashboard",
    "vosu.overlayhost",
    "vosu.toolhost",
];

/// `vosu plugin-host` exit status when invoked without valid bootstrap args
/// (`vosu_core::plugin::process::control::EXIT_ARGUMENTS`).
const PLUGIN_HOST_EXIT_ARGUMENTS: i32 = 70;

fn assert_no_test_harness_endpoint(executable: &[u8]) {
    for marker in ["/__vosu_test/", HARNESS_HEADER, HARNESS_CAPABILITY_ENV] {
        assert!(
            !executable
                .windows(marker.len())
                .any(|window| window == marker.as_bytes()),
            "release executable contains {marker}"
        );
    }
}

struct ChildGuard {
    child: Option<Child>,
    #[cfg(windows)]
    _job: Option<windows_process::SmokeJob>,
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            #[cfg(unix)]
            {
                let pgid = child.id() as libc::pid_t;
                // SAFETY: `libc::kill` with a negative process group id and
                // `SIGKILL` has no memory safety preconditions; the daemon was
                // spawned in its own process group via `process_group(0)`.
                let _ = unsafe { libc::kill(-pgid, libc::SIGKILL) };
            }
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn available_port() -> u16 {
    TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn available_port_excluding(excluded: u16) -> u16 {
    loop {
        let port = available_port();
        if port != excluded {
            return port;
        }
    }
}

fn legacy_config(overlay_port: u16, dashboard_port: u16) -> String {
    format!(
        "ENABLE_AUTOUPDATE=false\nOPEN_DASHBOARD_ON_STARTUP=false\nSTART_AT_LOGIN=false\nDEBUG_LOG=false\nSERVER_IP=127.0.0.1\nSERVER_PORT={overlay_port}\nDASHBOARD_IP=127.0.0.1\nDASHBOARD_PORT={dashboard_port}\nALLOWED_IPS=127.0.0.1,localhost,absolute\n",
    )
}

fn toml_config(overlay_port: u16, dashboard_port: u16) -> String {
    format!(
        "ENABLE_AUTOUPDATE = false\nOPEN_DASHBOARD_ON_STARTUP = false\nSTART_AT_LOGIN = false\nDEBUG_LOG = false\nSERVER_IP = \"127.0.0.1\"\nSERVER_PORT = {overlay_port}\nDASHBOARD_IP = \"127.0.0.1\"\nDASHBOARD_PORT = {dashboard_port}\nALLOWED_IPS = \"127.0.0.1,localhost,absolute\"\n"
    )
}

/// The release's own version. The release workflow validates the tag against
/// this root manifest and builds the packaged executable from the same tree.
fn release_version() -> String {
    let manifest: toml::Table =
        toml::from_str(include_str!("../../../Cargo.toml")).expect("root Cargo manifest");
    manifest["package"]["version"]
        .as_str()
        .expect("root package version")
        .to_owned()
}

/// Every regular file directly in `directory` with its bytes. A prerelease's
/// own `prerelease/` subdirectory is not a file and so is not compared.
fn config_files(directory: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut files: Vec<_> = fs::read_dir(directory)
        .expect("list normal config root")
        .map(|entry| entry.expect("normal config root entry").path())
        .filter(|path| path.is_file())
        .map(|path| {
            let bytes = fs::read(&path).expect("read normal config root file");
            (path, bytes)
        })
        .collect();
    files.sort();
    files
}

fn apply_isolated_env(command: &mut Command, roots: &Path) {
    command
        .env("XDG_CONFIG_HOME", roots.join("config"))
        .env("XDG_DATA_HOME", roots.join("data"))
        .env("XDG_CACHE_HOME", roots.join("cache"))
        .env("XDG_RUNTIME_DIR", roots.join("runtime"))
        .env("HOME", roots)
        .env("LOCALAPPDATA", roots.join("local"))
        // Avoid the Windows archive/OneDrive guards mistaking a CI unpack root
        // for the user's configured TEMP or OneDrive tree.
        .env("TEMP", roots.join("runtime-temp-sentinel"))
        .env("OneDrive", roots.join("onedrive-sentinel"))
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("DBUS_SESSION_BUS_ADDRESS");
}

async fn wait_get(client: &reqwest::Client, url: &str) -> reqwest::Response {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match bounded!(client.get(url).send()) {
            Ok(response) => return response,
            Err(error) if Instant::now() < deadline => {
                let _ = error;
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(error) => panic!("release daemon did not become reachable at {url}: {error}"),
        }
    }
}

#[cfg(unix)]
fn configure_daemon_process(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

#[cfg(windows)]
fn configure_daemon_process(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    // ADR 0024: graphical Windows launches run console-less without relying on
    // console attachment or CTRL_BREAK delivery.
    command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
}

#[cfg(windows)]
mod windows_process {
    use std::{
        io,
        os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
        process::Child,
        time::{Duration, Instant},
    };
    use windows_sys::Win32::{
        Foundation::{ERROR_MORE_DATA, HANDLE},
        System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectBasicProcessIdList,
            JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
        },
    };

    const LISTED_IDS: usize = 64;

    #[repr(C)]
    struct IdList {
        assigned: u32,
        listed: u32,
        ids: [usize; LISTED_IDS],
    }

    pub struct SmokeJob {
        handle: OwnedHandle,
    }

    impl SmokeJob {
        pub fn attach(child: &Child) -> io::Result<Self> {
            // SAFETY: null security attributes and unnamed job object have no
            // memory safety preconditions.
            let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if raw.is_null() {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: `raw` is a fresh handle owned exclusively by this struct.
            let handle = unsafe { OwnedHandle::from_raw_handle(raw.cast()) };
            // SAFETY: all-zero `JOBOBJECT_EXTENDED_LIMIT_INFORMATION` is valid.
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            // SAFETY: `handle` is live and `limits` is correctly sized.
            let ok = unsafe {
                SetInformationJobObject(
                    handle.as_raw_handle() as HANDLE,
                    JobObjectExtendedLimitInformation,
                    (&raw const limits).cast(),
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: both `handle` and `child` are live process/job handles.
            let assigned = unsafe {
                AssignProcessToJobObject(
                    handle.as_raw_handle() as HANDLE,
                    child.as_raw_handle() as HANDLE,
                )
            };
            if assigned == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self { handle })
        }

        pub fn assigned_processes(&self) -> io::Result<u32> {
            let mut list = IdList {
                assigned: 0,
                listed: 0,
                ids: [0; LISTED_IDS],
            };
            // SAFETY: `self.handle` is live and `list` is a writable
            // `JOBOBJECT_BASIC_PROCESS_ID_LIST`-compatible buffer.
            let ok = unsafe {
                QueryInformationJobObject(
                    self.handle.as_raw_handle() as HANDLE,
                    JobObjectBasicProcessIdList,
                    (&raw mut list).cast(),
                    size_of::<IdList>() as u32,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(ERROR_MORE_DATA as i32) {
                    return Err(error);
                }
            }
            Ok(list.assigned)
        }
    }

    pub async fn assert_no_job_orphans(job: &SmokeJob) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let remaining = job
                .assigned_processes()
                .expect("query Windows smoke job process list");
            if remaining == 0 {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "release daemon left {remaining} orphan child process(es) in Windows job"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

#[cfg(unix)]
async fn shutdown_and_verify_no_orphans(guard: &mut ChildGuard) {
    let pid = guard.child.as_ref().expect("running daemon").id() as libc::pid_t;
    // SAFETY: `libc::kill` with a valid pid and `SIGTERM` has no memory safety
    // preconditions; `pid` is this test's own spawned daemon process.
    let result = unsafe { libc::kill(pid, libc::SIGTERM) };
    assert_eq!(result, 0, "send release daemon SIGTERM");

    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(status) = guard.child.as_mut().unwrap().try_wait().unwrap() {
            assert!(
                status.success(),
                "release daemon exited unsuccessfully after SIGTERM: {status}"
            );
            guard.child.take();
            break;
        }
        assert!(
            Instant::now() < deadline,
            "release daemon failed to drain after SIGTERM"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let orphan_deadline = Instant::now() + Duration::from_secs(5);
    loop {
        // SAFETY: signal 0 probes whether any process remains in the daemon's
        // dedicated process group without delivering a signal.
        let probe = unsafe { libc::kill(-pid, 0) };
        if probe == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
            return;
        }
        assert!(
            Instant::now() < orphan_deadline,
            "release daemon left orphan child process(es) in process group {pid}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[cfg(windows)]
async fn shutdown_and_verify_no_orphans(guard: &mut ChildGuard) {
    // ADR 0024: the console-less daemon cannot rely on CTRL_BREAK console
    // attachment. Terminate the daemon process directly and verify via the
    // Windows Job Object that zero child processes remain orphaned.
    let child = guard.child.as_mut().expect("running daemon");
    child
        .kill()
        .expect("terminate console-less Windows release daemon");
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if guard.child.as_mut().unwrap().try_wait().unwrap().is_some() {
            guard.child.take();
            break;
        }
        assert!(
            Instant::now() < deadline,
            "Windows release daemon did not exit after controlled termination"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    if let Some(job) = guard._job.as_ref() {
        windows_process::assert_no_job_orphans(job).await;
    }
}

fn assert_packaged_cli_preflight(binary: &Path, roots: &Path) {
    let mut version_cmd = Command::new(binary);
    apply_isolated_env(&mut version_cmd, roots);
    let version = version_cmd
        .arg("--version")
        .output()
        .expect("run packaged release --version");
    assert!(
        version.status.success(),
        "--version failed with {}",
        version.status
    );
    assert_eq!(
        String::from_utf8_lossy(&version.stdout).replace("\r\n", "\n"),
        format!("vosu {}\n", release_version()),
        "packaged executable is not this release's version"
    );

    let mut help_cmd = Command::new(binary);
    apply_isolated_env(&mut help_cmd, roots);
    let help = help_cmd
        .arg("--help")
        .output()
        .expect("run packaged release --help");
    let help_stdout = String::from_utf8_lossy(&help.stdout);
    let help_stderr = String::from_utf8_lossy(&help.stderr);
    assert!(
        help.status.success(),
        "packaged release --help failed with {}\nstdout:\n{help_stdout}\nstderr:\n{help_stderr}",
        help.status,
    );
    assert!(
        help_stdout.contains("vosu logs"),
        "packaged release --help omitted vosu logs\nstdout:\n{help_stdout}\nstderr:\n{help_stderr}",
    );
    #[cfg(target_os = "linux")]
    assert!(
        help_stdout.contains("-s, --setup"),
        "packaged Linux release --help omitted -s, --setup\nstdout:\n{help_stdout}\nstderr:\n{help_stderr}",
    );

    let mut logs_cmd = Command::new(binary);
    apply_isolated_env(&mut logs_cmd, roots);
    let logs = logs_cmd
        .arg("logs")
        .output()
        .expect("run packaged release logs");
    assert!(
        logs.status.success(),
        "packaged release `vosu logs` failed with {}\nstdout:\n{}\nstderr:\n{}",
        logs.status,
        String::from_utf8_lossy(&logs.stdout),
        String::from_utf8_lossy(&logs.stderr),
    );

    let mut plugin_host_cmd = Command::new(binary);
    apply_isolated_env(&mut plugin_host_cmd, roots);
    let plugin_host = plugin_host_cmd
        .arg("plugin-host")
        .output()
        .expect("run packaged release plugin-host");
    assert_eq!(
        plugin_host.status.code(),
        Some(PLUGIN_HOST_EXIT_ARGUMENTS),
        "packaged `vosu plugin-host` without bootstrap args must refuse with exit code {PLUGIN_HOST_EXIT_ARGUMENTS}, got {:?}",
        plugin_host.status
    );

    #[cfg(target_os = "linux")]
    for probe in [
        "--vosu-internal-probe-backend=ebpf",
        "--vosu-internal-probe-backend=cn-proc",
    ] {
        let output = Command::new(binary)
            .arg(probe)
            .output()
            .unwrap_or_else(|error| panic!("run packaged Linux release probe {probe}: {error}"));
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let combined = format!("{stdout}\n{stderr}");
        assert!(
            !combined.contains("this vosu build does not include Linux process-event backends"),
            "packaged Linux release probe {probe} reached a featureless backend path with {}\nstdout:\n{stdout}\nstderr:\n{stderr}",
            output.status,
        );
    }
}

fn assert_removed_ingame_headers(label: &str, headers: &[(String, String)]) {
    for forbidden in ["upgrade", "location", "deprecation", "link"] {
        assert!(
            !headers
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case(forbidden)),
            "{label} /api/ingame unexpectedly returned forbidden header `{forbidden}`: {headers:?}"
        );
    }
}

/// 2026-11-25 owner decision (ADR 0021 amendment): `/api/ingame` on the overlay
/// listener is a removed route that returns the ordinary 404 for GET, HEAD,
/// POST, and WebSocket upgrade requests, with no upgrade, redirect,
/// `Deprecation`, or `Link` header.
async fn assert_removed_ingame_route(client: &reqwest::Client, overlay_port: u16) {
    let url = format!("http://127.0.0.1:{overlay_port}/api/ingame");
    for (label, builder) in [
        ("GET", client.get(&url)),
        ("HEAD", client.head(&url)),
        ("POST", client.post(&url)),
    ] {
        let response = bounded!(builder.send()).expect("send /api/ingame probe");
        assert_eq!(
            response.status().as_u16(),
            404,
            "{label} /api/ingame must return 404"
        );
        let headers: Vec<(String, String)> = response
            .headers()
            .iter()
            .map(|(k, v)| (k.as_str().to_owned(), v.to_str().unwrap_or("").to_owned()))
            .collect();
        assert_removed_ingame_headers(label, &headers);
    }

    // Raw HTTP/1.1 WebSocket upgrade probe so reqwest does not alter Upgrade /
    // Connection headers.
    let mut stream = TcpStream::connect(SocketAddrV4::new(Ipv4Addr::LOCALHOST, overlay_port))
        .expect("connect overlay listener for /api/ingame upgrade probe");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let request = format!(
        "GET /api/ingame HTTP/1.1\r\n\
         Host: 127.0.0.1:{overlay_port}\r\n\
         Connection: Upgrade\r\n\
         Upgrade: websocket\r\n\
         Sec-WebSocket-Version: 13\r\n\
         Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
         \r\n"
    );
    stream
        .write_all(request.as_bytes())
        .expect("write /api/ingame upgrade request");
    let mut raw = Vec::new();
    let mut chunk = [0u8; 1024];
    while !raw.windows(4).any(|window| window == b"\r\n\r\n") {
        let read = stream
            .read(&mut chunk)
            .expect("read /api/ingame upgrade response");
        assert!(
            read > 0,
            "connection closed before HTTP response headers on /api/ingame upgrade"
        );
        raw.extend_from_slice(&chunk[..read]);
        assert!(raw.len() < 65_536, "oversized /api/ingame response headers");
    }
    let header_end = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .unwrap();
    let head = std::str::from_utf8(&raw[..header_end]).expect("UTF-8 HTTP response head");
    let mut lines = head.split("\r\n");
    let status_line = lines.next().expect("HTTP status line");
    let status_code: u16 = status_line
        .split_whitespace()
        .nth(1)
        .expect("HTTP status code")
        .parse()
        .expect("numeric HTTP status code");
    assert_eq!(
        status_code, 404,
        "WebSocket upgrade to /api/ingame must return 404 (never 101), got: {status_line}"
    );
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            Some((name.trim().to_owned(), value.trim().to_owned()))
        })
        .collect();
    assert_removed_ingame_headers("WS upgrade", &headers);
}

fn assert_no_update_artifacts(root: &Path) {
    if !root.exists() {
        return;
    }
    let mut stack = vec![root.to_path_buf()];
    let mut suspicious = Vec::<PathBuf>::new();
    while let Some(directory) = stack.pop() {
        for entry in fs::read_dir(directory).unwrap().filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path.clone());
            }
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            if name.contains(".vosu-update-helper-")
                || name.contains("install-record")
                || name.ends_with(".part")
                || name.starts_with(".vosu-backup-")
            {
                suspicious.push(path);
            }
        }
    }
    assert!(
        suspicious.is_empty(),
        "release left updater artifacts: {suspicious:?}"
    );
}

#[tokio::test]
#[ignore = "requires a packaged release binary"]
async fn packaged_release_obeys_production_policy_and_shuts_down_cleanly() {
    let binary = std::env::var_os("VOSU_RELEASE_SMOKE_BINARY")
        .expect("VOSU_RELEASE_SMOKE_BINARY must name the unpacked native release executable");
    let binary = fs::canonicalize(binary).expect("canonical release executable path");
    assert!(
        binary.is_file(),
        "release smoke path is not a file: {}",
        binary.display()
    );
    assert_no_test_harness_endpoint(&fs::read(&binary).expect("read release executable"));

    let roots = TempDir::new().expect("temporary release roots");
    fs::create_dir_all(roots.path().join("runtime")).unwrap();
    fs::create_dir_all(roots.path().join("local")).unwrap();

    // Stage the executable inside the temp root on Windows so portable config,
    // data, logs, and cache stay completely isolated from the unpacked folder.
    #[cfg(windows)]
    let staged_binary = {
        let bin_dir = roots.path().join("bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let target = bin_dir.join(binary.file_name().unwrap());
        if fs::hard_link(&binary, &target).is_err() {
            fs::copy(&binary, &target).expect("stage packaged Windows release executable");
        }
        target
    };
    #[cfg(unix)]
    let staged_binary = binary.clone();

    assert_packaged_cli_preflight(&staged_binary, roots.path());

    let prerelease = release_version().contains('-');
    #[cfg(unix)]
    let (config_dir, data_dir, cache_root) = (
        roots.path().join("config/vosu"),
        roots.path().join("data/vosu"),
        roots.path().join("cache/vosu"),
    );
    #[cfg(windows)]
    let (config_dir, data_dir, cache_root) = {
        let program_dir = staged_binary.parent().expect("staged binary parent");
        (
            program_dir.to_path_buf(),
            program_dir.to_path_buf(),
            program_dir.join(".cache"),
        )
    };
    fs::create_dir_all(&config_dir).unwrap();
    let effective_data_dir = if prerelease {
        data_dir.join("prerelease")
    } else {
        data_dir.clone()
    };
    let static_root = effective_data_dir.join("static");
    let seeded = static_root.join("seeded-counter");
    fs::create_dir_all(&seeded).unwrap();
    fs::write(
        seeded.join("index.html"),
        "<!doctype html><title>Seeded</title>",
    )
    .unwrap();
    fs::write(
        seeded.join("metadata.txt"),
        "Name: Seeded Counter\nAuthor: CI\nVersion: 1.0\n640x480\n",
    )
    .unwrap();

    let first_port = available_port();
    let dashboard_port = available_port_excluding(first_port);
    let config_path = config_dir.join("vosu.toml");
    // A valid current TOML file is authoritative over both legacy env inputs.
    fs::write(&config_path, toml_config(first_port, dashboard_port)).unwrap();
    fs::write(
        config_dir.join("vosu.env"),
        legacy_config(1, dashboard_port),
    )
    .unwrap();
    fs::write(
        config_dir.join("tosu.env"),
        legacy_config(2, dashboard_port),
    )
    .unwrap();
    // A prerelease build must keep these normal-root files byte-identical.
    let normal_dir = config_dir.clone();
    let normal_files = config_files(&normal_dir);

    let mut command = Command::new(&staged_binary);
    command.arg("--update=false");
    apply_isolated_env(&mut command, roots.path());
    // A release build has no harness endpoint to mount, even when offered a
    // well-formed capability.
    command
        .env(HARNESS_CAPABILITY_ENV, HARNESS_CAPABILITY)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_daemon_process(&mut command);
    let spawned = command.spawn().expect("spawn unpacked release executable");
    #[cfg(windows)]
    let job = Some(
        windows_process::SmokeJob::attach(&spawned)
            .expect("attach console-less Windows daemon to smoke job"),
    );
    let mut child = ChildGuard {
        child: Some(spawned),
        #[cfg(windows)]
        _job: job,
    };

    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap();
    let base = format!("http://127.0.0.1:{first_port}");
    let dashboard = bounded!(wait_get(&client, &format!("{base}/")));
    assert_eq!(dashboard.status(), 200);
    let management = dashboard.url().origin().ascii_serialization();
    assert_eq!(
        management,
        format!("http://127.0.0.1:{dashboard_port}"),
        "dashboard listener must bind configured DASHBOARD_PORT"
    );
    let dashboard = bounded!(dashboard.text()).unwrap();
    assert!(dashboard.contains("vosu"));
    let csrf = dashboard
        .split("\"csrf\":\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap();

    // Verify vosu 2.0 API v1 sessions endpoint on the overlay listener.
    let sessions = bounded!(client.get(format!("{base}/api/v1/sessions")).send())
        .expect("GET /api/v1/sessions");
    assert_eq!(sessions.status(), 200);
    let sessions_json: serde_json::Value =
        bounded!(sessions.json()).expect("/api/v1/sessions must return valid JSON");
    assert!(
        sessions_json["sessions"].is_array(),
        "/api/v1/sessions payload missing `sessions` array: {sessions_json}"
    );

    // Verify exact 7 bundled plugins on the dashboard listener (no testkit echo).
    let plugins_resp =
        bounded!(client.get(format!("{management}/api/plugins")).send()).expect("GET /api/plugins");
    assert_eq!(plugins_resp.status(), 200);
    let plugins_json: serde_json::Value =
        bounded!(plugins_resp.json()).expect("/api/plugins must return valid JSON");
    let plugins = plugins_json["plugins"]
        .as_array()
        .expect("/api/plugins missing `plugins` array");
    let plugin_ids: Vec<&str> = plugins
        .iter()
        .map(|entry| entry["id"].as_str().expect("plugin entry id"))
        .collect();
    assert_eq!(
        plugin_ids, EXPECTED_BUNDLED_PLUGIN_IDS,
        "packaged production binary must expose exactly the 7 bundled plugins"
    );
    assert!(
        !plugin_ids.contains(&"vosu.testkit.echo"),
        "production binary must never include vosu.testkit.echo"
    );
    assert!(
        plugins.iter().all(|entry| entry["state"] == "running"),
        "all bundled plugins must be running: {plugins_json}"
    );

    // Verify 2026-11-25 removed `/api/ingame` contract on the overlay listener.
    assert_removed_ingame_route(&client, first_port).await;

    // A prerelease runs on its own `prerelease/` roots: it imports the 1.x
    // config once, read-only, and never writes the normal tree.
    let (config_dir, config_path) = if prerelease {
        let alpha_dir = normal_dir.join("prerelease");
        let alpha_path = alpha_dir.join("vosu.toml");
        assert!(
            alpha_path.is_file(),
            "prerelease must import config into prerelease/vosu.toml"
        );
        assert_eq!(
            config_files(&normal_dir),
            normal_files,
            "prerelease import wrote the normal config root"
        );
        (alpha_dir, alpha_path)
    } else {
        (config_dir, config_path)
    };
    let canonical: toml::Table =
        toml::from_str(&fs::read_to_string(&config_path).expect("read canonical packaged config"))
            .expect("packaged daemon canonicalizes current TOML");
    assert_eq!(
        canonical.get("SERVER_PORT").and_then(toml::Value::as_float),
        Some(f64::from(first_port))
    );
    assert!(!config_dir.join("vosu.env").exists());
    assert!(!config_dir.join("tosu.env").exists());
    let asset = bounded!(client.get(format!("{base}/assets/tokens.css")).send()).unwrap();
    assert_eq!(asset.status(), 200);
    assert!(!bounded!(asset.bytes()).unwrap().is_empty());

    let mut replies = Vec::new();
    for (path, capability) in [
        ("/__vosu_test/v1/unknown-sibling?schema=v2", None),
        (HARNESS_PATH, Some(HARNESS_CAPABILITY)),
        (HARNESS_PATH, None),
    ] {
        let mut request = client.get(format!("{base}{path}"));
        if let Some(capability) = capability {
            request = request.header(HARNESS_HEADER, capability);
        }
        let response = bounded!(request.send()).unwrap();
        replies.push((response.status(), bounded!(response.bytes()).unwrap()));
    }
    assert_eq!(replies[0].0, 404);
    assert!(
        replies.iter().all(|reply| reply == &replies[0]),
        "release build answered the harness endpoint: {replies:?}"
    );

    let state = bounded!(client.get(format!("{base}/json")).send()).unwrap();
    let state_status = state.status();
    assert!(
        state_status == 200 || state_status == 500,
        "unexpected state response: {state_status}"
    );
    if state_status == 500 {
        assert_eq!(
            bounded!(state.text()).unwrap(),
            r#"{"error":"osu is not ready/running"}"#
        );
    }

    let counters = bounded!(
        client
            .get(format!("{management}/api/counters/search/seeded"))
            .send()
    )
    .unwrap();
    assert_eq!(counters.status(), 200);
    assert!(
        bounded!(counters.text())
            .unwrap()
            .contains("Seeded Counter")
    );

    let rejected = bounded!(client.post(format!(
        "{management}/api/counters/download/https%3A%2F%2F127.0.0.1%3A9%2Fcounter.zip?name=blocked&update=false"
    )).header(reqwest::header::ORIGIN, &management).header("X-Vosu-CSRF", csrf).send()).unwrap();
    assert_eq!(rejected.status(), 200);
    assert!(bounded!(rejected.text()).unwrap().contains("error"));
    assert!(!static_root.join("blocked").exists());

    let report = bounded!(
        client
            .post(format!("{management}/api/generateReport"))
            .header(reqwest::header::ORIGIN, &management)
            .header("X-Vosu-CSRF", csrf)
            .send()
    )
    .unwrap();
    assert_eq!(report.status(), 200);
    let disposition = report
        .headers()
        .get(reqwest::header::CONTENT_DISPOSITION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    assert!(disposition.contains("vosu-report-") && disposition.contains(".html"));
    assert!(bounded!(report.text()).unwrap().contains("vosu"));

    let second_port = available_port_excluding(dashboard_port);
    fs::write(
        &config_path,
        format!("SERVER_PORT = {second_port}\nBROKEN = [\n"),
    )
    .unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;
    let last_good_client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap();
    let last_good = bounded!(last_good_client.get(format!("{base}/")).send())
        .expect("malformed TOML must retain the packaged listener");
    assert_eq!(last_good.status(), 200);

    fs::write(&config_path, toml_config(second_port, dashboard_port)).unwrap();
    let second_base = format!("http://127.0.0.1:{second_port}");
    let rebound = bounded!(wait_get(&client, &format!("{second_base}/")));
    assert_eq!(rebound.status(), 200);

    let saved = bounded!(
        client
            .post(format!("{management}/api/settingsSave"))
            .header(reqwest::header::ORIGIN, &management)
            .header("X-Vosu-CSRF", csrf)
            .json(&serde_json::json!({"DEBUG_LOG": true}))
            .send()
    )
    .expect("save packaged core settings");
    assert_eq!(saved.status(), 200);
    assert_eq!(
        bounded!(saved.json::<serde_json::Value>()).unwrap(),
        serde_json::json!({"status":"updated"})
    );
    let persisted: toml::Table =
        toml::from_str(&fs::read_to_string(&config_path).expect("read packaged settingsSave TOML"))
            .expect("packaged settingsSave persists typed TOML");
    assert_eq!(
        persisted.get("DEBUG_LOG").and_then(toml::Value::as_bool),
        Some(true)
    );

    let deleted = bounded!(
        client
            .post(format!("{management}/api/counters/delete/seeded-counter"))
            .header(reqwest::header::ORIGIN, &management)
            .header("X-Vosu-CSRF", csrf)
            .send()
    )
    .unwrap();
    assert_eq!(deleted.status(), 200);
    assert!(!seeded.exists());
    assert!(config_path.is_file());
    assert!(!config_dir.join("vosu.env").exists());
    assert!(!config_dir.join("tosu.env").exists());

    shutdown_and_verify_no_orphans(&mut child).await;

    let mut post_logs_cmd = Command::new(&staged_binary);
    apply_isolated_env(&mut post_logs_cmd, roots.path());
    let post_logs = post_logs_cmd
        .args(["logs", "-n", "20"])
        .output()
        .expect("run packaged release `vosu logs -n 20` after shutdown");
    assert!(
        post_logs.status.success(),
        "`vosu logs -n 20` failed after shutdown with {}",
        post_logs.status
    );
    #[cfg(unix)]
    assert!(
        String::from_utf8_lossy(&post_logs.stdout).contains("Overlays and API:"),
        "post-shutdown `vosu logs -n 20` omitted daemon startup lines:\n{}",
        String::from_utf8_lossy(&post_logs.stdout)
    );

    if prerelease {
        assert_eq!(
            config_files(&normal_dir),
            normal_files,
            "prerelease wrote the normal config root"
        );
        #[cfg(unix)]
        {
            assert!(roots.path().join("data/vosu/prerelease/static").is_dir());
            assert!(!roots.path().join("data/vosu/static").exists());
        }
    } else {
        assert!(!normal_dir.join("prerelease").exists());
    }

    assert_no_update_artifacts(&cache_root);
}

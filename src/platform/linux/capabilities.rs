use std::ffi::OsStr;
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Output};

const PTRACE_CAPABILITY_SET: &str = "cap_sys_ptrace=ep";
#[cfg(feature = "linux-process-events")]
const BPF_CAPABILITY_SET: &str = "cap_sys_ptrace,cap_sys_resource,cap_perfmon,cap_bpf=ep";
const MAXIMUM_CAPABILITY_SET: &str = if cfg!(feature = "linux-process-events") {
    "cap_sys_ptrace,cap_sys_resource,cap_perfmon,cap_bpf=ep"
} else {
    PTRACE_CAPABILITY_SET
};
const PKEXEC_CANDIDATES: &[&str] = &[
    "/usr/bin/pkexec",
    "/bin/pkexec",
    "/usr/sbin/pkexec",
    "/sbin/pkexec",
];
const SETCAP_CANDIDATES: &[&str] = &[
    "/usr/sbin/setcap",
    "/usr/bin/setcap",
    "/sbin/setcap",
    "/bin/setcap",
];
const SETUP_ARGUMENT: &str = "--setup";
#[cfg(feature = "linux-process-events")]
const EBPF_PROBE_ARGUMENT: &str = "--vosu-internal-probe-backend=ebpf";
#[cfg(feature = "linux-process-events")]
const CN_PROC_PROBE_ARGUMENT: &str = "--vosu-internal-probe-backend=cn-proc";

pub(super) fn install(executable: &Path) -> io::Result<()> {
    ensure_executable(executable)?;
    let mut tools = NativeToolProvider;
    let mut runner = NativeCommandRunner;
    if let Some(report) =
        install_with_privilege(executable, effective_user_id(), &mut tools, &mut runner)?
    {
        report.write_to(&mut io::stdout().lock(), executable)?;
    }
    Ok(())
}

fn effective_user_id() -> libc::uid_t {
    // SAFETY: geteuid has no memory-safety preconditions.
    unsafe { libc::geteuid() }
}

fn install_with_privilege(
    executable: &Path,
    effective_user_id: libc::uid_t,
    tools: &mut impl ToolProvider,
    runner: &mut impl CommandRunner,
) -> io::Result<Option<InstallReport>> {
    if effective_user_id == 0 {
        let setcap = tools.setcap()?;
        return configure_capabilities(&setcap, executable, runner).map(Some);
    }

    let pkexec = tools.pkexec().map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "cannot request operating-system authentication for setup: {error}; \
                 no setup changes were made"
            ),
        )
    })?;
    elevate_setup(&pkexec, executable, runner)?;
    Ok(None)
}

fn elevate_setup(
    pkexec: &Path,
    executable: &Path,
    runner: &mut impl CommandRunner,
) -> io::Result<()> {
    let arguments = [executable.as_os_str(), OsStr::new(SETUP_ARGUMENT)];
    let status = runner.status(pkexec, &arguments).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "cannot start the operating-system polkit authentication prompt with \
                 {pkexec:?}: {error}; no setup changes were made"
            ),
        )
    })?;
    if status.success() {
        return Ok(());
    }

    let (kind, message) = match status.code() {
        Some(126) => (
            io::ErrorKind::PermissionDenied,
            "setup authentication was cancelled; no setup changes were made".to_owned(),
        ),
        Some(127) => (
            io::ErrorKind::PermissionDenied,
            "setup authentication failed, was unavailable, or could not start the elevated \
             setup; no setup changes were made"
                .to_owned(),
        ),
        _ => (
            io::ErrorKind::Other,
            format!("elevated setup failed ({status}); see the preceding error for details"),
        ),
    };
    Err(io::Error::new(kind, message))
}

fn ensure_executable(executable: &Path) -> io::Result<()> {
    if !executable.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("vosu executable path is not absolute: {executable:?}"),
        ));
    }

    let metadata = fs::metadata(executable).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("cannot inspect vosu executable {executable:?}: {error}"),
        )
    })?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("vosu executable is not an executable file: {executable:?}"),
        ));
    }

    Ok(())
}

trait ToolProvider {
    fn pkexec(&mut self) -> io::Result<PathBuf>;
    fn setcap(&mut self) -> io::Result<PathBuf>;
}

struct NativeToolProvider;

impl ToolProvider for NativeToolProvider {
    fn pkexec(&mut self) -> io::Result<PathBuf> {
        trusted_tool(PKEXEC_CANDIDATES, "pkexec")
    }

    fn setcap(&mut self) -> io::Result<PathBuf> {
        trusted_tool(SETCAP_CANDIDATES, "setcap")
    }
}

fn trusted_tool(candidates: &[&str], name: &str) -> io::Result<PathBuf> {
    for candidate in candidates {
        let Ok(path) = fs::canonicalize(candidate) else {
            continue;
        };
        let Ok(metadata) = fs::metadata(&path) else {
            continue;
        };
        if metadata.is_file()
            && metadata.permissions().mode() & 0o111 != 0
            && metadata.uid() == 0
            && metadata.permissions().mode() & 0o022 == 0
        {
            return Ok(path);
        }
    }

    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!("could not find a root-owned, non-writable {name} in standard system paths"),
    ))
}

trait CommandRunner {
    fn output(&mut self, program: &Path, arguments: &[&OsStr]) -> io::Result<Output>;
    fn status(&mut self, program: &Path, arguments: &[&OsStr]) -> io::Result<ExitStatus>;
}

struct NativeCommandRunner;

impl CommandRunner for NativeCommandRunner {
    fn output(&mut self, program: &Path, arguments: &[&OsStr]) -> io::Result<Output> {
        Command::new(program).args(arguments).output()
    }

    fn status(&mut self, program: &Path, arguments: &[&OsStr]) -> io::Result<ExitStatus> {
        Command::new(program).args(arguments).status()
    }
}

fn configure_capabilities(
    setcap: &Path,
    executable: &Path,
    runner: &mut impl CommandRunner,
) -> io::Result<InstallReport> {
    #[cfg(not(feature = "linux-process-events"))]
    if let Err(error) = apply_capability_set(
        setcap,
        executable,
        MAXIMUM_CAPABILITY_SET,
        "maximum compiled capability set",
        runner,
    ) {
        return Err(rollback_after(error, setcap, executable, runner));
    }

    #[cfg(feature = "linux-process-events")]
    let maximum_error = apply_capability_set(
        setcap,
        executable,
        MAXIMUM_CAPABILITY_SET,
        "maximum compiled capability set",
        runner,
    )
    .err();

    #[cfg(not(feature = "linux-process-events"))]
    {
        Ok(InstallReport {
            final_capabilities: PTRACE_CAPABILITY_SET,
        })
    }

    #[cfg(feature = "linux-process-events")]
    {
        let (ebpf, maximum_applied) = match maximum_error {
            None => (probe_backend(executable, EBPF_PROBE_ARGUMENT, runner), true),
            Some(maximum_error) => {
                if let Err(ptrace_error) = apply_capability_set(
                    setcap,
                    executable,
                    PTRACE_CAPABILITY_SET,
                    "ptrace fallback capability set",
                    runner,
                ) {
                    let error = io::Error::new(
                        ptrace_error.kind(),
                        format!("{maximum_error}; ptrace fallback also failed: {ptrace_error}"),
                    );
                    return Err(rollback_after(error, setcap, executable, runner));
                }
                (
                    ProbeOutcome::Unavailable(format!(
                        "unavailable: maximum capability set could not be applied: {maximum_error}"
                    )),
                    false,
                )
            }
        };
        let cn_proc = probe_backend(executable, CN_PROC_PROBE_ARGUMENT, runner);
        let final_capabilities = final_capability_set(ebpf.is_supported());

        if maximum_applied
            && let Err(error) = apply_capability_set(
                setcap,
                executable,
                final_capabilities,
                "final capability set",
                runner,
            )
        {
            return Err(rollback_after(error, setcap, executable, runner));
        }

        Ok(InstallReport {
            ebpf,
            cn_proc,
            final_capabilities,
        })
    }
}

fn apply_capability_set(
    setcap: &Path,
    executable: &Path,
    capabilities: &str,
    operation: &str,
    runner: &mut impl CommandRunner,
) -> io::Result<()> {
    let arguments = [OsStr::new(capabilities), executable.as_os_str()];
    let output = runner.output(setcap, &arguments).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "cannot run trusted setcap to apply the {operation} to {executable:?}: {error}. \
                 The privileged setup process needs authority to set file capabilities"
            ),
        )
    })?;
    if output.status.success() {
        return Ok(());
    }

    Err(io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!(
            "setcap could not apply the {operation} ({capabilities}) to {executable:?} ({}). \
             The privileged setup process needs authority to set file capabilities. \
             Do not run the vosu daemon itself as root",
            output_failure(&output.status, &output.stderr)
        ),
    ))
}

fn rollback_after(
    error: io::Error,
    setcap: &Path,
    executable: &Path,
    runner: &mut impl CommandRunner,
) -> io::Error {
    let arguments = [OsStr::new("-r"), executable.as_os_str()];
    match runner.output(setcap, &arguments) {
        Ok(output) if output.status.success() => error,
        Ok(output) => io::Error::new(
            error.kind(),
            format!(
                "{error}; additionally, setcap could not remove transient capabilities during \
                 rollback ({}), so verify {executable:?} before running it",
                output_failure(&output.status, &output.stderr)
            ),
        ),
        Err(rollback_error) => io::Error::new(
            error.kind(),
            format!(
                "{error}; additionally, setcap rollback could not be started: {rollback_error}; \
                 verify that transient capabilities were removed from {executable:?}"
            ),
        ),
    }
}

fn output_failure(status: &ExitStatus, stderr: &[u8]) -> String {
    let stderr = String::from_utf8_lossy(stderr);
    let stderr = stderr.trim();
    if stderr.is_empty() {
        status.to_string()
    } else {
        format!("{status}: {stderr}")
    }
}

#[cfg(feature = "linux-process-events")]
fn final_capability_set(ebpf_supported: bool) -> &'static str {
    if ebpf_supported {
        BPF_CAPABILITY_SET
    } else {
        PTRACE_CAPABILITY_SET
    }
}

#[cfg(feature = "linux-process-events")]
#[derive(Debug)]
enum ProbeOutcome {
    Supported,
    Unavailable(String),
}

#[cfg(feature = "linux-process-events")]
impl ProbeOutcome {
    fn is_supported(&self) -> bool {
        matches!(self, Self::Supported)
    }

    fn description(&self) -> &str {
        match self {
            Self::Supported => "supported",
            Self::Unavailable(reason) => reason,
        }
    }
}

#[cfg(feature = "linux-process-events")]
fn probe_backend(
    executable: &Path,
    argument: &'static str,
    runner: &mut impl CommandRunner,
) -> ProbeOutcome {
    let arguments = [OsStr::new(argument)];
    match runner.output(executable, &arguments) {
        Ok(output) if output.status.success() => ProbeOutcome::Supported,
        Ok(output) => ProbeOutcome::Unavailable(format!(
            "unavailable: probe {}",
            output_failure(&output.status, &output.stderr)
        )),
        Err(error) => {
            ProbeOutcome::Unavailable(format!("unavailable: probe could not start: {error}"))
        }
    }
}

#[derive(Debug)]
struct InstallReport {
    #[cfg(feature = "linux-process-events")]
    ebpf: ProbeOutcome,
    #[cfg(feature = "linux-process-events")]
    cn_proc: ProbeOutcome,
    final_capabilities: &'static str,
}

impl InstallReport {
    fn write_to(&self, writer: &mut impl Write, executable: &Path) -> io::Result<()> {
        #[cfg(not(feature = "linux-process-events"))]
        writeln!(
            writer,
            "Linux process-event backend: polling (optional backends are not compiled into this build)."
        )?;

        #[cfg(feature = "linux-process-events")]
        {
            writeln!(
                writer,
                "eBPF process-event backend: {}.",
                self.ebpf.description()
            )?;
            writeln!(
                writer,
                "CN_PROC process-event backend: {}.",
                self.cn_proc.description()
            )?;
            let selected = if self.ebpf.is_supported() {
                "eBPF"
            } else if self.cn_proc.is_supported() {
                "CN_PROC fallback"
            } else {
                "polling fallback"
            };
            writeln!(
                writer,
                "Preferred supported Linux process-event backend: {selected}."
            )?;
        }

        writeln!(
            writer,
            "Applied {} to {executable:?}. Restart vosu without sudo.",
            self.final_capabilities
        )
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::ffi::OsString;
    use std::os::unix::process::ExitStatusExt;

    use super::*;

    #[derive(Default)]
    struct RecordingToolProvider {
        pkexec: Option<PathBuf>,
        setcap: Option<PathBuf>,
        lookups: Vec<&'static str>,
    }

    impl ToolProvider for RecordingToolProvider {
        fn pkexec(&mut self) -> io::Result<PathBuf> {
            self.lookups.push("pkexec");
            self.pkexec
                .clone()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "pkexec unavailable"))
        }

        fn setcap(&mut self) -> io::Result<PathBuf> {
            self.lookups.push("setcap");
            self.setcap
                .clone()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "setcap unavailable"))
        }
    }

    #[derive(Default)]
    struct RecordingRunner {
        responses: VecDeque<io::Result<Output>>,
        status_responses: VecDeque<io::Result<ExitStatus>>,
        calls: Vec<(PathBuf, Vec<OsString>)>,
        status_calls: Vec<(PathBuf, Vec<OsString>)>,
    }

    impl RecordingRunner {
        fn with_responses(responses: impl IntoIterator<Item = io::Result<Output>>) -> Self {
            Self {
                responses: responses.into_iter().collect(),
                ..Self::default()
            }
        }

        fn with_status_responses(
            responses: impl IntoIterator<Item = io::Result<ExitStatus>>,
        ) -> Self {
            Self {
                status_responses: responses.into_iter().collect(),
                ..Self::default()
            }
        }
    }

    impl CommandRunner for RecordingRunner {
        fn output(&mut self, program: &Path, arguments: &[&OsStr]) -> io::Result<Output> {
            self.calls.push((
                program.to_path_buf(),
                arguments
                    .iter()
                    .map(|argument| argument.to_os_string())
                    .collect(),
            ));
            self.responses
                .pop_front()
                .expect("a response for every recorded command")
        }

        fn status(&mut self, program: &Path, arguments: &[&OsStr]) -> io::Result<ExitStatus> {
            self.status_calls.push((
                program.to_path_buf(),
                arguments
                    .iter()
                    .map(|argument| argument.to_os_string())
                    .collect(),
            ));
            self.status_responses
                .pop_front()
                .expect("a response for every recorded status command")
        }
    }

    fn successful_output() -> io::Result<Output> {
        Ok(Output {
            status: ExitStatus::from_raw(0),
            stdout: Vec::new(),
            stderr: Vec::new(),
        })
    }

    fn failed_output(stderr: &str) -> io::Result<Output> {
        Ok(Output {
            status: ExitStatus::from_raw(1 << 8),
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
        })
    }

    fn status_with_code(code: i32) -> io::Result<ExitStatus> {
        Ok(ExitStatus::from_raw(code << 8))
    }

    #[cfg(not(feature = "linux-process-events"))]
    #[test]
    fn standard_build_applies_ptrace_and_reports_polling() {
        let setcap = Path::new("/usr/sbin/setcap");
        let executable = Path::new("/opt/vosu/vosu");
        let mut runner = RecordingRunner::with_responses([successful_output()]);

        let report = configure_capabilities(setcap, executable, &mut runner)
            .expect("configure standard capabilities");
        let mut rendered = Vec::new();
        report
            .write_to(&mut rendered, executable)
            .expect("render install report");

        assert_eq!(
            runner.calls,
            vec![(
                setcap.to_path_buf(),
                vec![
                    OsString::from(PTRACE_CAPABILITY_SET),
                    executable.as_os_str().to_os_string(),
                ],
            )]
        );
        assert_eq!(report.final_capabilities, PTRACE_CAPABILITY_SET);
        assert!(
            String::from_utf8(rendered)
                .expect("UTF-8 report")
                .contains("backend: polling")
        );
    }

    #[cfg(feature = "linux-process-events")]
    #[test]
    fn feature_build_probes_exact_arguments_and_narrows_without_ebpf() {
        let setcap = Path::new("/usr/sbin/setcap");
        let executable = Path::new("/opt/vosu/vosu");
        let mut runner = RecordingRunner::with_responses([
            successful_output(),
            failed_output("eBPF unavailable"),
            successful_output(),
            successful_output(),
        ]);

        let report = configure_capabilities(setcap, executable, &mut runner)
            .expect("configure probed capabilities");

        assert_eq!(
            runner.calls,
            vec![
                (
                    setcap.to_path_buf(),
                    vec![
                        OsString::from(BPF_CAPABILITY_SET),
                        executable.as_os_str().to_os_string(),
                    ],
                ),
                (
                    executable.to_path_buf(),
                    vec![OsString::from(EBPF_PROBE_ARGUMENT)],
                ),
                (
                    executable.to_path_buf(),
                    vec![OsString::from(CN_PROC_PROBE_ARGUMENT)],
                ),
                (
                    setcap.to_path_buf(),
                    vec![
                        OsString::from(PTRACE_CAPABILITY_SET),
                        executable.as_os_str().to_os_string(),
                    ],
                ),
            ]
        );
        assert_eq!(report.final_capabilities, PTRACE_CAPABILITY_SET);
        assert!(!report.ebpf.is_supported());
        assert!(report.cn_proc.is_supported());
        let mut rendered = Vec::new();
        report
            .write_to(&mut rendered, executable)
            .expect("render install report");
        let rendered = String::from_utf8(rendered).expect("UTF-8 report");
        assert!(rendered.contains("eBPF process-event backend: unavailable"));
        assert!(rendered.contains("CN_PROC process-event backend: supported"));
        assert!(
            rendered.contains("Preferred supported Linux process-event backend: CN_PROC fallback")
        );
        assert!(rendered.contains("Applied cap_sys_ptrace=ep"));
    }

    #[cfg(feature = "linux-process-events")]
    #[test]
    fn feature_build_keeps_bpf_capabilities_only_after_successful_ebpf_probe() {
        let setcap = Path::new("/usr/sbin/setcap");
        let executable = Path::new("/opt/vosu/vosu");
        let mut runner = RecordingRunner::with_responses([
            successful_output(),
            successful_output(),
            failed_output("CN_PROC unavailable"),
            successful_output(),
        ]);

        let report = configure_capabilities(setcap, executable, &mut runner)
            .expect("configure eBPF capabilities");

        assert_eq!(final_capability_set(false), PTRACE_CAPABILITY_SET);
        assert_eq!(final_capability_set(true), BPF_CAPABILITY_SET);
        assert_eq!(report.final_capabilities, BPF_CAPABILITY_SET);
        assert!(report.ebpf.is_supported());
        assert!(!report.cn_proc.is_supported());
    }

    #[cfg(feature = "linux-process-events")]
    #[test]
    fn feature_build_falls_back_to_ptrace_when_maximum_capabilities_cannot_apply() {
        let setcap = Path::new("/usr/sbin/setcap");
        let executable = Path::new("/opt/vosu/vosu");
        let mut runner = RecordingRunner::with_responses([
            failed_output("cap_bpf is unsupported"),
            successful_output(),
            successful_output(),
        ]);

        let report = configure_capabilities(setcap, executable, &mut runner)
            .expect("configure ptrace fallback capabilities");

        assert_eq!(
            runner.calls,
            vec![
                (
                    setcap.to_path_buf(),
                    vec![
                        OsString::from(BPF_CAPABILITY_SET),
                        executable.as_os_str().to_os_string(),
                    ],
                ),
                (
                    setcap.to_path_buf(),
                    vec![
                        OsString::from(PTRACE_CAPABILITY_SET),
                        executable.as_os_str().to_os_string(),
                    ],
                ),
                (
                    executable.to_path_buf(),
                    vec![OsString::from(CN_PROC_PROBE_ARGUMENT)],
                ),
            ]
        );
        assert_eq!(report.final_capabilities, PTRACE_CAPABILITY_SET);
        assert!(!report.ebpf.is_supported());
        assert!(report.cn_proc.is_supported());
        let mut rendered = Vec::new();
        report
            .write_to(&mut rendered, executable)
            .expect("render fallback report");
        let rendered = String::from_utf8(rendered).expect("UTF-8 report");
        assert!(rendered.contains("maximum capability set could not be applied"));
        assert!(
            rendered.contains("Preferred supported Linux process-event backend: CN_PROC fallback")
        );
    }

    #[cfg(feature = "linux-process-events")]
    #[test]
    fn feature_build_reports_privilege_error_only_when_ptrace_fallback_also_fails() {
        let setcap = Path::new("/usr/sbin/setcap");
        let executable = Path::new("/opt/vosu/vosu");
        let mut runner = RecordingRunner::with_responses([
            failed_output("cap_bpf is unsupported"),
            failed_output("Operation not permitted"),
            successful_output(),
        ]);

        let error = configure_capabilities(setcap, executable, &mut runner)
            .expect_err("ptrace fallback failure must fail setup");

        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(error.to_string().contains("ptrace fallback also failed"));
        assert_eq!(
            runner.calls,
            vec![
                (
                    setcap.to_path_buf(),
                    vec![
                        OsString::from(BPF_CAPABILITY_SET),
                        executable.as_os_str().to_os_string(),
                    ],
                ),
                (
                    setcap.to_path_buf(),
                    vec![
                        OsString::from(PTRACE_CAPABILITY_SET),
                        executable.as_os_str().to_os_string(),
                    ],
                ),
                (
                    setcap.to_path_buf(),
                    vec![OsString::from("-r"), executable.as_os_str().to_os_string()],
                ),
            ]
        );
    }

    #[cfg(not(feature = "linux-process-events"))]
    #[test]
    fn nonzero_setcap_is_a_privilege_error_and_attempts_rollback() {
        let setcap = Path::new("/usr/sbin/setcap");
        let executable = Path::new("/opt/vosu/vosu");
        let mut runner = RecordingRunner::with_responses([
            failed_output("Operation not permitted"),
            successful_output(),
        ]);

        let error = configure_capabilities(setcap, executable, &mut runner)
            .expect_err("setcap failure must fail setup");

        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(error.to_string().contains("Operation not permitted"));
        assert!(
            error
                .to_string()
                .contains("privileged setup process needs authority")
        );
        assert_eq!(
            runner.calls,
            vec![
                (
                    setcap.to_path_buf(),
                    vec![
                        OsString::from(MAXIMUM_CAPABILITY_SET),
                        executable.as_os_str().to_os_string(),
                    ],
                ),
                (
                    setcap.to_path_buf(),
                    vec![OsString::from("-r"), executable.as_os_str().to_os_string(),],
                ),
            ]
        );
    }

    #[cfg(feature = "linux-process-events")]
    #[test]
    fn failed_final_narrowing_removes_transient_capabilities() {
        let setcap = Path::new("/usr/sbin/setcap");
        let executable = Path::new("/opt/vosu/vosu");
        let mut runner = RecordingRunner::with_responses([
            successful_output(),
            failed_output("eBPF unavailable"),
            failed_output("CN_PROC unavailable"),
            failed_output("narrowing denied"),
            successful_output(),
        ]);

        let error = configure_capabilities(setcap, executable, &mut runner)
            .expect_err("final narrowing must fail setup");

        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(
            runner.calls.last(),
            Some(&(
                setcap.to_path_buf(),
                vec![OsString::from("-r"), executable.as_os_str().to_os_string(),],
            ))
        );
    }
    #[test]
    fn unprivileged_setup_uses_only_polkit_pkexec_and_propagates_success() {
        let executable = Path::new("/opt/vosu/vosu");
        let pkexec = PathBuf::from("/usr/bin/pkexec");
        let mut tools = RecordingToolProvider {
            pkexec: Some(pkexec.clone()),
            setcap: Some(PathBuf::from("/usr/sbin/setcap")),
            ..RecordingToolProvider::default()
        };
        let mut runner = RecordingRunner::with_status_responses([status_with_code(0)]);

        let report = install_with_privilege(executable, 1000, &mut tools, &mut runner)
            .expect("successful polkit elevation");

        assert!(report.is_none());
        assert_eq!(tools.lookups, vec!["pkexec"]);
        assert!(runner.calls.is_empty(), "setcap ran before elevation");
        assert_eq!(
            runner.status_calls,
            vec![(
                pkexec,
                vec![
                    executable.as_os_str().to_os_string(),
                    OsString::from("--setup"),
                ],
            )]
        );
        assert!(
            runner
                .status_calls
                .iter()
                .all(|(program, _)| program.file_name() != Some(OsStr::new("sudo"))),
            "sudo must never be selected as the elevation program"
        );
    }

    #[test]
    fn authentication_cancellation_fails_before_any_setup_change() {
        let mut tools = RecordingToolProvider {
            pkexec: Some(PathBuf::from("/usr/bin/pkexec")),
            ..RecordingToolProvider::default()
        };
        let mut runner = RecordingRunner::with_status_responses([status_with_code(126)]);

        let error =
            install_with_privilege(Path::new("/opt/vosu/vosu"), 1000, &mut tools, &mut runner)
                .expect_err("cancelled authentication must fail");

        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(error.to_string().contains("authentication was cancelled"));
        assert!(error.to_string().contains("no setup changes were made"));
        assert_eq!(tools.lookups, vec!["pkexec"]);
        assert!(runner.calls.is_empty(), "setcap ran after cancellation");
    }

    #[test]
    fn unavailable_or_failed_authentication_fails_before_any_setup_change() {
        let executable = Path::new("/opt/vosu/vosu");

        let mut missing_tools = RecordingToolProvider::default();
        let mut missing_runner = RecordingRunner::default();
        let missing_error =
            install_with_privilege(executable, 1000, &mut missing_tools, &mut missing_runner)
                .expect_err("missing pkexec must fail");
        assert_eq!(missing_error.kind(), io::ErrorKind::NotFound);
        assert!(
            missing_error
                .to_string()
                .contains("cannot request operating-system authentication")
        );
        assert!(
            missing_error
                .to_string()
                .contains("no setup changes were made")
        );
        assert!(missing_runner.calls.is_empty());
        assert!(missing_runner.status_calls.is_empty());

        let mut denied_tools = RecordingToolProvider {
            pkexec: Some(PathBuf::from("/usr/bin/pkexec")),
            ..RecordingToolProvider::default()
        };
        let mut denied_runner = RecordingRunner::with_status_responses([status_with_code(127)]);
        let denied_error =
            install_with_privilege(executable, 1000, &mut denied_tools, &mut denied_runner)
                .expect_err("failed authentication must fail");
        assert_eq!(denied_error.kind(), io::ErrorKind::PermissionDenied);
        assert!(
            denied_error
                .to_string()
                .contains("authentication failed, was unavailable")
        );
        assert!(
            denied_error
                .to_string()
                .contains("no setup changes were made")
        );
        assert!(
            denied_runner.calls.is_empty(),
            "setcap ran without authentication"
        );
    }

    #[test]
    fn already_root_setup_skips_authentication_and_runs_capability_transaction() {
        let setcap = PathBuf::from("/usr/sbin/setcap");
        let mut tools = RecordingToolProvider {
            pkexec: Some(PathBuf::from("/usr/bin/pkexec")),
            setcap: Some(setcap),
            ..RecordingToolProvider::default()
        };
        #[cfg(feature = "linux-process-events")]
        let responses = [
            successful_output(),
            failed_output("eBPF unavailable"),
            failed_output("CN_PROC unavailable"),
            successful_output(),
        ];
        #[cfg(not(feature = "linux-process-events"))]
        let responses = [successful_output()];
        let mut runner = RecordingRunner::with_responses(responses);

        let report =
            install_with_privilege(Path::new("/opt/vosu/vosu"), 0, &mut tools, &mut runner)
                .expect("root setup")
                .expect("root setup report");

        assert_eq!(tools.lookups, vec!["setcap"]);
        assert!(
            runner.status_calls.is_empty(),
            "root setup requested authentication"
        );
        assert_eq!(report.final_capabilities, PTRACE_CAPABILITY_SET);
    }
}

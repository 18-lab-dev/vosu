use std::{
    ffi::{OsStr, OsString},
    io,
};

#[cfg(target_os = "linux")]
use std::env;

use super::Bundle;

pub(super) const HELP_TEXT: &str = "Usage: vosu [OPTION]\n       vosu logs [--follow] [-n N]\n\nOptions:\n  -h, --help     Print this help\n  -s, --setup    Authenticate and install supported Linux capabilities\n  -V, --version  Print the version\n  --tool <dir>   Load a trusted development tool (repeatable)\n  --plugin <dir> Load a development plugin (repeatable; debug and prerelease builds).\n                 A process plugin runs only after you approve it on the dashboard\n\nLogs:\n  --follow      Follow live daemon logs (Ctrl-C to stop)\n  -n N          Print the last N lines (default: 50)\n";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CliCommand {
    Help,
    Setup,
    Version,
    Logs {
        lines: usize,
        follow: bool,
        window: bool,
    },
    #[cfg(target_os = "linux")]
    ProbeBackend(BackendProbe),
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum BackendProbe {
    Ebpf,
    CnProc,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CommandInvocation {
    RunDaemon,
    Command(CliCommand),
    InvalidInput,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ArgumentKind {
    Other,
    Command(CliCommand),
    InvalidCommand,
}

pub(super) fn classify_command_invocation(raw_args: &[OsString]) -> CommandInvocation {
    if raw_args.get(1).is_some_and(|arg| arg == "logs") {
        return parse_logs(&raw_args[2..])
            .map(CommandInvocation::Command)
            .unwrap_or(CommandInvocation::InvalidInput);
    }
    let mut command = None;
    for argument in raw_args.iter().skip(1) {
        match classify_argument(argument) {
            ArgumentKind::Other => {}
            ArgumentKind::Command(candidate) => {
                if command.replace(candidate).is_some() {
                    return CommandInvocation::InvalidInput;
                }
            }
            ArgumentKind::InvalidCommand => return CommandInvocation::InvalidInput,
        }
    }

    match command {
        None => CommandInvocation::RunDaemon,
        Some(command) if raw_args.len() == 2 => CommandInvocation::Command(command),
        Some(_) => CommandInvocation::InvalidInput,
    }
}

fn parse_logs(arguments: &[OsString]) -> Option<CliCommand> {
    let mut lines = 50;
    let mut follow = false;
    let mut window = false;
    let mut count_seen = false;
    let mut arguments = arguments.iter();
    while let Some(argument) = arguments.next() {
        match argument.to_str()? {
            "--follow" if !follow => follow = true,
            "--window" if !window => window = true,
            "-n" if !count_seen => {
                let value = arguments.next()?.to_str()?;
                if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
                    return None;
                }
                lines = value.parse().ok()?;
                count_seen = true;
            }
            _ => return None,
        }
    }
    Some(CliCommand::Logs {
        lines,
        follow,
        window,
    })
}

fn classify_argument(argument: &OsStr) -> ArgumentKind {
    match argument.to_str() {
        Some("-h" | "--help") => ArgumentKind::Command(CliCommand::Help),
        Some("-s" | "--setup") => ArgumentKind::Command(CliCommand::Setup),
        Some("-V" | "--version") => ArgumentKind::Command(CliCommand::Version),
        Some("-g" | "--generate-offsets") => ArgumentKind::InvalidCommand,
        #[cfg(target_os = "linux")]
        Some("--vosu-internal-probe-backend=ebpf") => {
            ArgumentKind::Command(CliCommand::ProbeBackend(BackendProbe::Ebpf))
        }
        #[cfg(target_os = "linux")]
        Some("--vosu-internal-probe-backend=cn-proc") => {
            ArgumentKind::Command(CliCommand::ProbeBackend(BackendProbe::CnProc))
        }
        Some("--setcap") => ArgumentKind::InvalidCommand,
        Some(argument) if argument.starts_with("--setcap=") => ArgumentKind::InvalidCommand,
        Some(argument) if malformed_public_command(argument) => ArgumentKind::InvalidCommand,
        #[cfg(target_os = "linux")]
        Some(argument) if malformed_backend_probe(argument) => ArgumentKind::InvalidCommand,
        Some(_) | None => ArgumentKind::Other,
    }
}

fn malformed_public_command(argument: &str) -> bool {
    argument.starts_with("--help=")
        || argument.starts_with("--setup=")
        || argument.starts_with("--version=")
        || argument.starts_with("--generate-offsets=")
        || (argument.len() > 2
            && argument.starts_with('-')
            && matches!(argument.as_bytes()[1], b'h' | b's' | b'V' | b'g'))
}

#[cfg(target_os = "linux")]
fn malformed_backend_probe(argument: &str) -> bool {
    argument == "--vosu-internal-probe-backend"
        || argument.starts_with("--vosu-internal-probe-backend=")
}

pub(super) async fn run_command(command: CliCommand, bundle: &Bundle) -> io::Result<()> {
    match command {
        CliCommand::Help => {
            print!("{HELP_TEXT}");
            Ok(())
        }
        CliCommand::Setup => run_setup(),
        CliCommand::Version => {
            println!("vosu {}", bundle.version);
            Ok(())
        }
        CliCommand::Logs {
            lines,
            follow,
            window,
        } => {
            #[cfg(windows)]
            if window {
                vosu_sys::platform::open_log_console()?;
            }
            #[cfg(not(windows))]
            let _ = window;
            let channel = vosu_sys::platform::BuildChannel::from_version(bundle.version)
                .map_err(io::Error::other)?;
            let logs_dir = crate::logging::system_logs_dir(channel)?;
            let cancellation = tokio_util::sync::CancellationToken::new();
            let signal = cancellation.clone();
            let interrupt = tokio::spawn(async move {
                if tokio::signal::ctrl_c().await.is_ok() {
                    signal.cancel();
                }
            });
            let result =
                crate::logging::print_logs(&logs_dir, lines, follow, cancellation).await;
            interrupt.abort();
            result
        }
        #[cfg(target_os = "linux")]
        CliCommand::ProbeBackend(backend) => run_backend_probe(backend, bundle),
    }
}

#[cfg(target_os = "linux")]
fn run_setup() -> io::Result<()> {
    vosu_sys::platform::install_supported_capabilities(&env::current_exe()?)
}

#[cfg(not(target_os = "linux"))]
fn run_setup() -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "--setup is supported only on Linux",
    ))
}

#[cfg(all(target_os = "linux", feature = "linux-process-events"))]
fn run_backend_probe(backend: BackendProbe, bundle: &Bundle) -> io::Result<()> {
    use vosu_sys::events::{LinuxProcessEvents, ProcessEventMode};

    let mode = match backend {
        BackendProbe::Ebpf => ProcessEventMode::Ebpf,
        BackendProbe::CnProc => ProcessEventMode::CnProc,
    };
    let comm_hints = backend_probe_hints(bundle)?;
    let _events = LinuxProcessEvents::open(mode, &comm_hints)?;
    Ok(())
}

/// The CN_PROC/eBPF probe's comm hints: the unified name prefilter of every
/// bundled plugin manifest that carries a process matcher.
#[cfg(all(target_os = "linux", feature = "linux-process-events"))]
pub(super) fn backend_probe_hints(bundle: &Bundle) -> io::Result<vosu_sys::events::CommHints> {
    use crate::plugin::{MatchPlatform, MatcherEngine, Prefilter};
    let mut engine = MatcherEngine::new(MatchPlatform::Linux);
    let mut matchers = 0;
    for entry in bundle.plugins {
        let manifest = vosu_plugin_api::manifest::PackageManifest::parse(entry.manifest)
            .and_then(vosu_plugin_api::manifest::PackageManifest::into_plugin)
            .map_err(io::Error::other)?;
        if let Some(matcher) = manifest.matcher {
            engine
                .register(manifest.id, matcher)
                .map_err(io::Error::other)?;
            matchers += 1;
        }
    }
    if matchers == 0 {
        return Err(io::Error::other("no bundled plugin has a process matcher"));
    }
    let Prefilter::Names(names) = engine.prefilter() else {
        return Err(io::Error::other(
            "the backend probe requires a bounded name filter",
        ));
    };
    vosu_sys::events::CommHints::new(names.iter().map(String::as_str)).map_err(io::Error::other)
}

#[cfg(all(target_os = "linux", not(feature = "linux-process-events")))]
fn run_backend_probe(_backend: BackendProbe, _bundle: &Bundle) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "this vosu build does not include Linux process-event backends",
    ))
}

use std::{
    ffi::{OsStr, OsString},
    io,
};

#[cfg(target_os = "linux")]
use std::env;

pub(super) const HELP_TEXT: &str = "Usage: vosu [OPTION]\n\nOptions:\n  -h, --help              Print this help\n  -s, --setup             Install supported Linux capabilities\n  -g, --generate-offsets  Generate offsets for the running osu!lazer client to the vosu offset cache\n";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CliCommand {
    Help,
    Setup,
    GenerateOffsets,
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

fn classify_argument(argument: &OsStr) -> ArgumentKind {
    match argument.to_str() {
        Some("-h" | "--help") => ArgumentKind::Command(CliCommand::Help),
        Some("-s" | "--setup") => ArgumentKind::Command(CliCommand::Setup),
        Some("-g" | "--generate-offsets") => ArgumentKind::Command(CliCommand::GenerateOffsets),
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
        || argument.starts_with("--generate-offsets=")
        || (argument.len() > 2
            && argument.starts_with('-')
            && matches!(argument.as_bytes()[1], b'h' | b's' | b'g'))
}

#[cfg(target_os = "linux")]
fn malformed_backend_probe(argument: &str) -> bool {
    argument == "--vosu-internal-probe-backend"
        || argument.starts_with("--vosu-internal-probe-backend=")
}

pub(super) async fn run_command(command: CliCommand) -> io::Result<()> {
    match command {
        CliCommand::Help => {
            print!("{HELP_TEXT}");
            Ok(())
        }
        CliCommand::Setup => run_setup(),
        CliCommand::GenerateOffsets => {
            let destination = super::offset_generator::generate().await?;
            println!("Generated lazer offsets to {}", destination.display());
            Ok(())
        }
        #[cfg(target_os = "linux")]
        CliCommand::ProbeBackend(backend) => run_backend_probe(backend),
    }
}

#[cfg(target_os = "linux")]
fn run_setup() -> io::Result<()> {
    crate::platform::install_supported_capabilities(&env::current_exe()?)
}

#[cfg(not(target_os = "linux"))]
fn run_setup() -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "--setup is supported only on Linux",
    ))
}

#[cfg(all(target_os = "linux", feature = "linux-process-events"))]
fn run_backend_probe(backend: BackendProbe) -> io::Result<()> {
    use crate::instance::{LinuxProcessEvents, ProcessEventMode};

    let mode = match backend {
        BackendProbe::Ebpf => ProcessEventMode::Ebpf,
        BackendProbe::CnProc => ProcessEventMode::CnProc,
    };
    let _events = LinuxProcessEvents::open(mode)?;
    Ok(())
}

#[cfg(all(target_os = "linux", not(feature = "linux-process-events")))]
fn run_backend_probe(_backend: BackendProbe) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "this vosu build does not include Linux process-event backends",
    ))
}

pub(super) fn validate_generated_offsets(bytes: &[u8], expected_version: &str) -> io::Result<()> {
    let offsets = crate::memory::lazer_offsets::LazerOffsets::from_json(bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    if offsets.osu_version != expected_version {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "lazer offset generator returned osu! version {}, expected {}",
                offsets.osu_version, expected_version
            ),
        ));
    }
    Ok(())
}

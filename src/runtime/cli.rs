use std::{
    ffi::{OsStr, OsString},
    io,
    path::{Path, PathBuf},
};

#[cfg(target_os = "linux")]
use std::env;

#[cfg(target_os = "linux")]
use std::io::Write;

#[cfg(target_os = "linux")]
use std::process::Command;

#[cfg(target_os = "linux")]
use crate::assets::DaemonAssets;
pub(super) const HELP_TEXT: &str = "Usage: vosu [OPTION]\n\nOptions:\n  -h, --help              Print this help\n  -s, --setup             Install supported Linux capabilities\n  -g, --generate-offsets  Generate offsets for the running osu!lazer client to the vosu offset cache\n";
#[cfg(target_os = "linux")]
const OFFSETS_FILE_NAME: &str = "offsets.json";

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

pub(super) fn run_command(command: CliCommand) -> io::Result<()> {
    match command {
        CliCommand::Help => {
            print!("{HELP_TEXT}");
            Ok(())
        }
        CliCommand::Setup => run_setup(),
        CliCommand::GenerateOffsets => {
            let output_directory = crate::platform::Paths::system()?.cache().join("offsets");
            std::fs::create_dir_all(&output_directory)?;
            let destination = generate_offsets_to(&output_directory)?;
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

#[cfg(target_os = "linux")]
const OFFSET_GENERATOR_ENV: &str = "VOSU_LAZER_OFFSET_GENERATOR";
#[cfg(target_os = "linux")]
const OFFSET_DAC_ENV: &str = "VOSU_LAZER_OFFSET_DAC";

#[cfg(target_os = "linux")]
fn generate_offsets_to(directory: &Path) -> io::Result<PathBuf> {
    let (pid, version, client_root) = current_lazer_client()?;
    let generator = resolve_offset_generator()?;
    let dac = resolve_offset_dac(&client_root)?;
    let template_asset = DaemonAssets::get(OFFSETS_FILE_NAME).ok_or_else(|| {
        io::Error::new(io::ErrorKind::NotFound, "embedded offsets.json is missing")
    })?;

    let mut template = tempfile::Builder::new()
        .prefix(".offset-template-")
        .suffix(".json")
        .tempfile_in(directory)?;
    template.write_all(template_asset.data.as_ref())?;
    template.as_file_mut().sync_all()?;

    let temporary_output = tempfile::Builder::new()
        .prefix(".offsets-generated-")
        .suffix(".tmp")
        .tempfile_in(directory)?
        .into_temp_path();
    let output = Command::new(&generator)
        .arg("--pid")
        .arg(pid.to_string())
        .arg("--dac")
        .arg(&dac)
        .arg("--template")
        .arg(template.path())
        .arg("--output")
        .arg(&temporary_output)
        .output()
        .map_err(|error| {
            io::Error::other(format!(
                "could not run lazer offset generator '{}': {error}",
                generator.display()
            ))
        })?;
    if !output.status.success() {
        let diagnostic = String::from_utf8_lossy(&output.stderr);
        return Err(io::Error::other(format!(
            "lazer offset generator failed for osu!lazer {version}: {}",
            diagnostic.trim()
        )));
    }

    let generated = std::fs::read(&temporary_output)?;
    validate_generated_offsets(&generated, &version)?;
    let destination = directory.join(format!("{version}.json"));
    crate::platform::atomic_replace(&temporary_output, &destination)?;
    Ok(destination)
}

#[cfg(not(target_os = "linux"))]
fn generate_offsets_to(_directory: &Path) -> io::Result<PathBuf> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "--generate-offsets requires the Linux lazer offset generator",
    ))
}

#[cfg(target_os = "linux")]
fn current_lazer_client() -> io::Result<(u32, String, PathBuf)> {
    use crate::instance::{NativeProcessProvider, ProcessProvider};

    let provider = {
        #[cfg(feature = "linux-process-events")]
        {
            NativeProcessProvider::default()
        }
        #[cfg(not(feature = "linux-process-events"))]
        {
            NativeProcessProvider
        }
    };
    let processes = provider.enumerate().map_err(|error| {
        io::Error::other(format!("could not enumerate osu! processes: {error}"))
    })?;
    let mut matches = Vec::new();
    for process in processes {
        if process.bitness != crate::process::ProcessBitness::X64 {
            continue;
        }
        let Ok(root) = crate::memory::lazer_version::process_executable_root(process.pid) else {
            continue;
        };
        let Ok(version) = crate::memory::lazer_version::discover_from_root(&root) else {
            continue;
        };
        matches.push((process.pid, version, root));
    }

    match matches.as_slice() {
        [] => Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no running 64-bit osu!lazer client with osu!.deps.json was found",
        )),
        [(pid, version, root)] => Ok((*pid, version.clone(), root.clone())),
        _ => {
            let pids = matches
                .iter()
                .map(|(pid, _, _)| pid.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("multiple running osu!lazer clients found (PIDs: {pids})"),
            ))
        }
    }
}

#[cfg(target_os = "linux")]
fn resolve_offset_generator() -> io::Result<PathBuf> {
    if let Some(path) = env::var_os(OFFSET_GENERATOR_ENV).filter(|path| !path.is_empty()) {
        return Ok(PathBuf::from(path));
    }

    let current_directory = env::current_dir()?;
    let executable_directory = env::current_exe()?
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| io::Error::other("vosu executable has no parent directory"))?;
    let candidates = [
        executable_directory.join("lazer-offset-generator"),
        current_directory
            .join("tools/lazer-offset-generator/bin/Release/net8.0/lazer-offset-generator"),
        current_directory
            .join("tools/lazer-offset-generator/bin/Debug/net8.0/lazer-offset-generator"),
    ];
    candidates
        .into_iter()
        .find(|path| path.is_file())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "lazer offset generator was not found; build it or set {OFFSET_GENERATOR_ENV}"
                ),
            )
        })
}

#[cfg(target_os = "linux")]
fn resolve_offset_dac(client_root: &Path) -> io::Result<PathBuf> {
    if let Some(path) = env::var_os(OFFSET_DAC_ENV).filter(|path| !path.is_empty()) {
        return Ok(PathBuf::from(path));
    }

    let candidates = [
        client_root.join("libmscordaccore.so"),
        client_root.join("usr/lib/libmscordaccore.so"),
        client_root.join("usr/bin/libmscordaccore.so"),
    ];
    candidates
        .into_iter()
        .find(|path| path.is_file())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "matching libmscordaccore.so was not found beside the current osu!lazer client; set {OFFSET_DAC_ENV}"
                ),
            )
        })
}

#[cfg(target_os = "linux")]
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

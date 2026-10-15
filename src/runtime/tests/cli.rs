use super::cli::{CliCommand, CommandInvocation, classify_command_invocation};
#[cfg(target_os = "linux")]
use super::cli::BackendProbe;

fn command_line(arguments: &[&str]) -> Vec<OsString> {
    std::iter::once(OsString::from("vosu"))
        .chain(arguments.iter().map(OsString::from))
        .collect()
}

#[test]
fn public_short_and_long_commands_are_equivalent() {
    for flag in ["-h", "--help"] {
        assert_eq!(
            classify_command_invocation(&command_line(&[flag])),
            CommandInvocation::Command(CliCommand::Help)
        );
    }
    for flag in ["-s", "--setup"] {
        assert_eq!(
            classify_command_invocation(&command_line(&[flag])),
            CommandInvocation::Command(CliCommand::Setup)
        );
    }
    for flag in ["-V", "--version"] {
        assert_eq!(
            classify_command_invocation(&command_line(&[flag])),
            CommandInvocation::Command(CliCommand::Version)
        );
    }
}

#[test]
fn public_commands_reject_extra_arguments() {
    for flag in ["-h", "--help", "-s", "--setup", "-V", "--version"] {
        assert_eq!(
            classify_command_invocation(&command_line(&[flag, "--update=false"])),
            CommandInvocation::InvalidInput,
            "{flag} accepted an extra argument"
        );
        assert_eq!(
            classify_command_invocation(&command_line(&["--update=false", flag])),
            CommandInvocation::InvalidInput,
            "{flag} accepted a preceding argument"
        );
    }
}

#[test]
fn removed_generation_commands_are_invalid_not_daemon_arguments() {
    for arguments in [
        vec!["-g"],
        vec!["--generate-offsets"],
        vec!["-g", "--update=false"],
        vec!["--update=false", "--generate-offsets"],
    ] {
        assert_eq!(
            classify_command_invocation(&command_line(&arguments)),
            CommandInvocation::InvalidInput,
            "removed command was accepted: {arguments:?}"
        );
    }
}

#[test]
fn public_commands_reject_values_and_combined_short_forms() {
    for argument in [
        "--help=true",
        "--setup=no",
        "--version=1",
        "-Vx",
        "--generate-offsets=elsewhere.json",
        "-hvalue",
        "-sg",
        "-g=elsewhere.json",
        "--setcap",
        "--setcap=true",
    ] {
        assert_eq!(
            classify_command_invocation(&command_line(&[argument])),
            CommandInvocation::InvalidInput,
            "{argument} was not rejected"
        );
    }
}

#[test]
fn unrecognized_and_legacy_arguments_remain_daemon_arguments() {
    for arguments in [
        vec!["--update=false"],
        vec!["--helpful"],
        vec!["--versions"],
        vec!["--setup-mode=automatic"],
        vec!["--generate-offsets-later"],
        vec!["--update-helper", "--update-token=token"],
    ] {
        assert_eq!(
            classify_command_invocation(&command_line(&arguments)),
            CommandInvocation::RunDaemon,
            "unrecognized arguments were intercepted: {arguments:?}"
        );
    }
}

#[cfg(unix)]
#[test]
fn non_utf8_arguments_remain_daemon_arguments() {
    use std::os::unix::ffi::OsStringExt;

    let arguments = vec![OsString::from("vosu"), OsString::from_vec(vec![0xff])];
    assert_eq!(
        classify_command_invocation(&arguments),
        CommandInvocation::RunDaemon
    );
}

#[cfg(target_os = "linux")]
#[test]
fn linux_backend_probes_require_exact_standalone_values() {
    assert_eq!(
        classify_command_invocation(&command_line(&[
            "--vosu-internal-probe-backend=ebpf"
        ])),
        CommandInvocation::Command(CliCommand::ProbeBackend(BackendProbe::Ebpf))
    );
    assert_eq!(
        classify_command_invocation(&command_line(&[
            "--vosu-internal-probe-backend=cn-proc"
        ])),
        CommandInvocation::Command(CliCommand::ProbeBackend(BackendProbe::CnProc))
    );

    for arguments in [
        vec!["--vosu-internal-probe-backend"],
        vec!["--vosu-internal-probe-backend=auto"],
        vec!["--vosu-internal-probe-backend=ebpf", "extra"],
    ] {
        assert_eq!(
            classify_command_invocation(&command_line(&arguments)),
            CommandInvocation::InvalidInput,
            "malformed probe was not rejected: {arguments:?}"
        );
    }
}

#[cfg(not(target_os = "linux"))]
#[test]
fn backend_probe_arguments_are_not_commands_off_linux() {
    assert_eq!(
        classify_command_invocation(&command_line(&[
            "--vosu-internal-probe-backend=ebpf"
        ])),
        CommandInvocation::RunDaemon
    );
}

#[cfg(all(target_os = "linux", not(feature = "linux-process-events")))]
#[tokio::test]
async fn backend_probe_is_unsupported_without_process_event_feature() {
    let error = super::cli::run_command(CliCommand::ProbeBackend(BackendProbe::Ebpf))
        .await
        .expect_err("featureless probe unexpectedly succeeded");
    assert_eq!(error.kind(), io::ErrorKind::Unsupported);
}

#[cfg(not(target_os = "linux"))]
#[tokio::test]
async fn setup_is_unsupported_off_linux() {
    let error = super::cli::run_command(CliCommand::Setup)
        .await
        .expect_err("non-Linux setup unexpectedly succeeded");
    assert_eq!(error.kind(), io::ErrorKind::Unsupported);
}

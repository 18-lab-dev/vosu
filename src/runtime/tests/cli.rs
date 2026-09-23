use super::cli::{CliCommand, CommandInvocation, HELP_TEXT, classify_command_invocation};
#[cfg(target_os = "linux")]
use super::cli::validate_generated_offsets;
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
    for flag in ["-g", "--generate-offsets"] {
        assert_eq!(
            classify_command_invocation(&command_line(&[flag])),
            CommandInvocation::Command(CliCommand::GenerateOffsets)
        );
    }
}

#[test]
fn public_commands_reject_extra_arguments() {
    for flag in [
        "-h",
        "--help",
        "-s",
        "--setup",
        "-g",
        "--generate-offsets",
    ] {
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
fn public_commands_reject_values_and_combined_short_forms() {
    for argument in [
        "--help=true",
        "--setup=no",
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

#[test]
fn help_lists_every_public_short_and_long_form() {
    for forms in ["-h, --help", "-s, --setup", "-g, --generate-offsets"] {
        assert!(HELP_TEXT.contains(forms), "help omitted {forms}");
    }
    assert!(
        HELP_TEXT.contains("vosu offset cache"),
        "help omitted generated-offset destination"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn generated_offsets_must_match_the_running_client_version() {
    let embedded = DaemonAssets::get("offsets.json").expect("embedded offsets");
    let offsets =
        crate::memory::lazer_offsets::LazerOffsets::from_json(&embedded.data).expect("offsets");

    validate_generated_offsets(&embedded.data, &offsets.osu_version)
        .expect("embedded offsets should satisfy generated-output validation");
    let error = validate_generated_offsets(&embedded.data, "0.0.0")
        .expect_err("a different client version was accepted");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
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

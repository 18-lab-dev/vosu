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
    let error = bounded!(super::cli::run_command(CliCommand::ProbeBackend(BackendProbe::Ebpf), &VERSION_FIXTURE))
        .expect_err("featureless probe unexpectedly succeeded");
    assert_eq!(error.kind(), io::ErrorKind::Unsupported);
}

#[cfg(not(target_os = "linux"))]
#[tokio::test]
async fn setup_is_unsupported_off_linux() {
    let error = bounded!(super::cli::run_command(CliCommand::Setup, &VERSION_FIXTURE))
        .expect_err("non-Linux setup unexpectedly succeeded");
    assert_eq!(error.kind(), io::ErrorKind::Unsupported);
}

#[cfg(all(target_os = "linux", feature = "linux-process-events"))]
#[test]
fn backend_probe_uses_the_injected_matcher_prefilter() {
    static PLUGINS: [crate::plugin::BundledPlugin<'static>; 1] = [crate::plugin::BundledPlugin {
        manifest: "manifest_version=1\nkind='plugin'\nid='fixture.game'\nname='Probe fixture'\nversion='1.0.0'\nhost='>=2.0.0-alpha.1'\nabi_version=1\nruntime='in-process'\n[[provides]]\nid='game:fixture'\nversion='*'\n[match]\nprocess_names=['osu!','osulazer.exe']\ncomm_hints=['osu!','osulazer.exe']\n",
        settings_schema: None, namespace_schemas: &[], action_schemas: &[],
        vtable: vosu_plugin_testkit::echo::vosu_plugin_vtable,
    }];
    let bundle = Bundle { plugins: &PLUGINS, ..VERSION_FIXTURE };
    let hints = super::cli::backend_probe_hints(&bundle).unwrap();
    assert!(!hints.is_empty());
    assert!(hints.iter().any(|hint| hint == b"osu!"));
    assert!(hints.iter().any(|hint| hint == b"osulazer.exe"));
}

#[test]
fn logs_arguments_are_strict_and_do_not_start_the_daemon() {
    for (args, lines, follow, window) in [
        (vec!["logs"], 50, false, false),
        (vec!["logs", "--follow", "-n", "5"], 5, true, false),
        (vec!["logs", "-n", "0", "--window"], 0, false, true),
        (vec!["logs", "--window", "--follow", "-n", "123"], 123, true, true),
    ] {
        assert_eq!(
            classify_command_invocation(&command_line(&args)),
            CommandInvocation::Command(CliCommand::Logs { lines, follow, window }),
        );
    }
    for args in [
        vec!["logs", "-n"], vec!["logs", "-n", "-1"],
        vec!["logs", "-n", "+1"], vec!["logs", "-n", "five"],
        vec!["logs", "--follow", "--follow"], vec!["logs", "--version"],
        vec!["logs", "-n", "1", "-n", "2"], vec!["logs", "--unknown"],
    ] {
        assert_eq!(
            classify_command_invocation(&command_line(&args)),
            CommandInvocation::InvalidInput,
            "{args:?}",
        );
    }
}

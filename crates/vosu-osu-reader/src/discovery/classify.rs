//! osu!-specific classification predicates and launch-argument decoding,
//! moved from `src/instance/discovery.rs` and `src/instance.rs` (F8). Reused
//! by the root crate's generic discovery today and by `vosu-osu`'s own
//! plugin classification (P2).

use std::ffi::OsStr;

use vosu_osu_model::enums::ClientType;
use vosu_osu_model::launch::InstanceSettings;
use vosu_sys::process::ProcessBitness;

use super::StartupArgs;

/// Linux candidate pre-filter: a process whose kernel `comm` contains
/// `osu!` or `osulazer.exe`, matching 1.x's broad name rule
/// (`src/instance/discovery.rs:45-48`).
#[must_use]
pub fn is_linux_candidate_name(name: &OsStr) -> bool {
    let name = name.to_string_lossy();
    name.contains("osu!") || name.contains("osulazer.exe")
}

/// Linux named (stable) pass: a candidate whose `comm` contains
/// `osu!.exe` or `osulazer.exe` is stable even when the broad `osu!`
/// lazer fallback would otherwise apply (1.x quirk,
/// `src/instance/discovery.rs:82`).
#[must_use]
pub fn is_linux_named_name(name: &OsStr) -> bool {
    let name = name.to_string_lossy();
    name.contains("osu!.exe") || name.contains("osulazer.exe")
}

/// Windows bitness rule: an `osu!.exe`/`osulazer.exe` candidate is lazer
/// iff it is x64, else stable (1.x `src/instance/discovery.rs:63-71`).
#[must_use]
pub fn windows_client(bitness: ProcessBitness) -> ClientType {
    if bitness == ProcessBitness::X64 {
        ClientType::Lazer
    } else {
        ClientType::Stable
    }
}

/// Reproduces 1.x's `should_skip`: a startup command line requesting
/// tournament mode, or a truthy `debug-client-id`, is never tracked
/// (moved from `src/instance/discovery.rs:118-124`).
#[must_use]
pub fn should_skip(arguments: &StartupArgs) -> bool {
    arguments.get("tournament").is_some()
        || arguments
            .get("debug-client-id")
            .is_some_and(|value| value.is_truthy())
}

/// Decodes the client's own launch arguments into `InstanceSettings`
/// (moved from `src/instance.rs:66-83`, `InstanceSettings::from_arguments`).
#[must_use]
pub fn instance_settings(arguments: &StartupArgs) -> InstanceSettings {
    let tournament_ipc_id = arguments
        .get("spectateclient")
        .and_then(|value| value.as_number());
    let devserver_endpoint = arguments
        .get("devserver")
        .filter(|value| value.is_truthy())
        .and_then(|value| value.as_os_str())
        .filter(|value| !value.is_empty())
        .map(std::ffi::OsString::from);
    InstanceSettings {
        tournament_spectator: tournament_ipc_id.is_some(),
        tournament_ipc_id,
        devserver_endpoint,
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;

    #[test]
    fn command_filters_follow_presence_and_truthiness() {
        assert!(should_skip(&StartupArgs::from_array([OsString::from(
            "--tournament="
        )])));
        assert!(!should_skip(&StartupArgs::from_array([OsString::from(
            "--debug-client-id=false"
        )])));
        assert!(!should_skip(&StartupArgs::from_array([OsString::from(
            "--debug-client-id=0"
        )])));
        assert!(should_skip(&StartupArgs::from_array([OsString::from(
            "--debug-client-id=1"
        )])));
        assert!(should_skip(&StartupArgs::from_array([OsString::from(
            "--debug-client-id=x"
        )])));
        assert!(!should_skip(&StartupArgs::parse([OsString::from(
            "/opt/osu!/osu!"
        )])));
        assert!(should_skip(&StartupArgs::parse([
            OsString::from("/opt/osu!/osu!"),
            OsString::from("--tournament"),
        ])));
    }

    #[test]
    fn spectator_and_devserver_settings_are_typed() {
        let arguments = StartupArgs::from_array([
            OsString::from("--spectateclient=12"),
            OsString::from("--devserver=https://example.test"),
        ]);
        let settings = instance_settings(&arguments);
        assert_eq!(settings.tournament_ipc_id, Some(12.0));
        assert!(settings.tournament_spectator);
        assert_eq!(
            settings.devserver_endpoint.as_deref(),
            Some(OsStr::new("https://example.test"))
        );
    }

    #[test]
    fn linux_named_pass_requires_the_exact_executable_names() {
        assert!(is_linux_named_name(OsStr::new("osu!.exe")));
        assert!(is_linux_named_name(OsStr::new("osulazer.exe")));
        assert!(!is_linux_named_name(OsStr::new("osu!")));
        assert!(is_linux_candidate_name(OsStr::new("osu!")));
    }

    #[test]
    fn windows_bitness_selects_the_client() {
        assert_eq!(windows_client(ProcessBitness::X64), ClientType::Lazer);
        assert_eq!(windows_client(ProcessBitness::X86), ClientType::Stable);
    }
}

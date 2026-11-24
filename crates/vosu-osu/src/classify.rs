//! Attach-time classification: `SESSION_DECLINE` (D6), the stable/lazer
//! variant, and root resolution (R19). Reuses F8's
//! `vosu_osu_reader::discovery` predicates (`should_skip`,
//! `instance_settings`, `is_linux_candidate_name`, `is_linux_named_name`,
//! `windows_client`) rather than re-deriving the 1.x name rules; only the
//! settle timing (DF/R3) and the facts/root plumbing are new here.
//!
//! [`decide`] is a pure function of [`AttachFacts`] so every row of §7 this
//! unit owns (1, 4, 5, 6) is testable on either host OS: `platform` is a
//! runtime value, not a `cfg`, and `AttachFacts` can be built by hand in a
//! test without touching the real `#[cfg(windows)]`/`#[cfg(target_os =
//! "linux")]` root-resolution code `from_attached` runs in production.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use vosu_osu_model::enums::ClientType;
use vosu_osu_model::launch::InstanceSettings;
use vosu_osu_reader::discovery::{
    StartupArgs, instance_settings, is_linux_candidate_name, is_linux_named_name, should_skip,
    windows_client,
};
use vosu_osu_reader::process::ProcessBitness;
use vosu_plugin_api::{Bitness, SessionAttached};

/// Which OS this attach's facts describe. A runtime value (not a `cfg`) so
/// [`decide`] stays testable for both OSes regardless of the host building
/// this crate (plan §7 rows 1, 4, 5, 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Windows,
    Linux,
}

/// Process facts carried by `SESSION_ATTACHED` (A1b/DA), decoded into the
/// shapes this plugin's classification needs, plus the resolved `root`/
/// `executable_root` (R19). `name`/`path`/`args` are themselves `Option`:
/// absent means the host did not send that fact (a pre-amendment host, or
/// Windows never sending `argv`), never an empty value standing in for it.
/// [`decide`] identifies conservatively rather than declining when a fact a
/// decision would consult is missing — see its doc comment.
#[derive(Debug, Clone)]
pub struct AttachFacts {
    pub pid: u32,
    pub generation: u64,
    pub handle: u64,
    pub bitness: Bitness,
    /// Windows: the image file name. Linux: the kernel `comm` value.
    pub name: Option<String>,
    pub path: Option<PathBuf>,
    /// Decoded startup arguments: Linux from `argv` (`StartupArgs::parse`,
    /// which itself skips `argv[0]`), Windows from `command_line`
    /// (`StartupArgs::parse_command_line`). `None` only when the host sent
    /// neither field.
    pub args: Option<StartupArgs>,
    /// The osu! data root (R19): Linux `resolve_osu_path(pid)` falling back
    /// to `path`'s parent; Windows `VOSU_OSU_PATH` or `command_line`'s
    /// `argv[0]` parent. Empty when neither `path` nor `command_line` was
    /// sent.
    pub root: PathBuf,
    /// `path`'s parent (R19); empty when `path` is absent.
    pub executable_root: PathBuf,
}
impl AttachFacts {
    #[must_use]
    pub fn from_attached(message: &SessionAttached) -> Self {
        Self::from_attached_with(message, resolve_root)
    }

    /// Like [`Self::from_attached`], with an injectable root resolver
    /// (P3 review #7): a test uses this to avoid the real `/proc`/env-var
    /// reads [`resolve_root`] performs. Production code (`OsuPlugin::new`)
    /// goes through this directly with [`resolve_root`], not through
    /// [`Self::from_attached`], which has no caller of its own.
    #[must_use]
    pub fn from_attached_with(message: &SessionAttached, root_resolver: RootResolver) -> Self {
        let path = message.path.as_ref().map(PathBuf::from);
        let args = if let Some(argv) = &message.argv {
            Some(StartupArgs::parse(argv.iter().cloned().map(OsString::from)))
        } else {
            message
                .command_line
                .as_deref()
                .map(StartupArgs::parse_command_line)
        };
        let root = root_resolver(
            message.pid,
            path.as_deref(),
            message.command_line.as_deref(),
        );
        let executable_root = parent_or_default(path.as_deref());
        Self {
            pid: message.pid,
            generation: message.generation,
            handle: message.handle,
            bitness: message.bitness,
            name: message.name.clone(),
            path,
            args,
            root,
            executable_root,
        }
    }

    fn name_os(&self) -> Option<OsString> {
        self.name.as_deref().map(OsString::from)
    }
}

/// A root resolver function: see [`AttachFacts::from_attached_with`] and
/// [`resolve_root`] (the real, default one).
pub type RootResolver = fn(u32, Option<&Path>, Option<&str>) -> PathBuf;

fn parent_or_default(path: Option<&Path>) -> PathBuf {
    path.and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_default()
}

fn to_process_bitness(bitness: Bitness) -> ProcessBitness {
    match bitness {
        Bitness::X86 => ProcessBitness::X86,
        Bitness::X64 => ProcessBitness::X64,
    }
}

/// The resolved facts a Linux fallback candidate carries into its settle
/// timer, so `TIMER_FIRED` need not re-derive them from `AttachFacts`.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingIdentify {
    pub settings: InstanceSettings,
    pub root: PathBuf,
    pub executable_root: PathBuf,
    pub pid: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Identified {
    pub client: ClientType,
    pub settings: InstanceSettings,
    pub root: PathBuf,
    pub executable_root: PathBuf,
    pub pid: u32,
    /// Whether this identification came from the Linux "named" rule
    /// (`is_linux_named_name`: the name contains `osu!.exe`/
    /// `osulazer.exe`). Only a named identification counts as a sibling
    /// for a later fallback's settle check (R3; P2 review #2) — not a
    /// settled fallback that itself resolved to stable because a sibling
    /// was already present, a Windows client (chosen by bitness, not
    /// name), or a Linux attach with no name fact at all.
    pub named: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AttachDecision {
    /// `should_skip` (D6): `SESSION_DECLINE`, nothing else.
    Decline,
    Identify(Identified),
    /// A Linux fallback candidate (`osu!`, but neither `osu!.exe` nor
    /// `osulazer.exe`): the variant is undecided until `TIMER_FIRED`
    /// (DF/R3) re-checks `named_sibling_attached`.
    Settle(PendingIdentify),
}

fn default_settings() -> InstanceSettings {
    InstanceSettings {
        tournament_spectator: false,
        tournament_ipc_id: None,
        devserver_endpoint: None,
    }
}

/// Classifies one attach (plan §7 rows 1, 4, 5, 6). `named_sibling_attached`
/// is only consulted for a Linux fallback candidate: `true` means a named
/// (`osu!.exe`/`osulazer.exe`) sibling is already attached, so the fallback
/// resolves to stable immediately instead of waiting; `false` defers to
/// `TIMER_FIRED` (`AttachDecision::Settle`).
///
/// Conservative identification on missing facts (never declines on them):
/// `args` absent (no `argv`, no `command_line`) skips `should_skip`
/// entirely — a decline needs positive evidence of `--tournament` or a
/// truthy `debug-client-id`, not "we can't tell" — and yields
/// `InstanceSettings::default`-shaped settings (R10/owner note: an older
/// host simply cannot express spectator/devserver flags). `name` absent on
/// Linux (no `comm` fact) cannot be told apart from a lone `osu!` fallback
/// or a named stable client, so it identifies as stable immediately rather
/// than settling forever on a sibling signal it has no way to observe.
#[must_use]
pub fn decide(
    facts: &AttachFacts,
    platform: Platform,
    named_sibling_attached: bool,
) -> AttachDecision {
    if facts.args.as_ref().is_some_and(should_skip) {
        return AttachDecision::Decline;
    }
    let settings = facts
        .args
        .as_ref()
        .map_or_else(default_settings, instance_settings);
    let (client, named) = match platform {
        Platform::Windows => (windows_client(to_process_bitness(facts.bitness)), false),
        Platform::Linux => match facts.name_os() {
            Some(name) if is_linux_named_name(&name) => (ClientType::Stable, true),
            Some(name) if is_linux_candidate_name(&name) => {
                if named_sibling_attached {
                    (ClientType::Stable, false)
                } else {
                    return AttachDecision::Settle(PendingIdentify {
                        settings,
                        root: facts.root.clone(),
                        executable_root: facts.executable_root.clone(),
                        pid: facts.pid,
                    });
                }
            }
            // No `name` fact at all (pre-amendment host): conservative
            // identify, not an endless settle (see doc comment above).
            _ => (ClientType::Stable, false),
        },
    };
    AttachDecision::Identify(Identified {
        client,
        settings,
        root: facts.root.clone(),
        executable_root: facts.executable_root.clone(),
        pid: facts.pid,
        named,
    })
}

/// R19: Linux `resolve_osu_path(pid)` (a real `/proc` read, so gated to the
/// one target it can run on) falling back to `path`'s parent; Windows
/// `VOSU_OSU_PATH` or `command_line`'s `argv[0]` parent, via
/// [`windows_root_from_command_line`] (the "cfg(windows) root rule" the
/// native vm-test exercises directly, without the env var, to stay
/// deterministic under parallel tests).
pub(crate) fn resolve_root(pid: u32, path: Option<&Path>, command_line: Option<&str>) -> PathBuf {
    #[cfg(target_os = "linux")]
    {
        let _ = command_line;
        vosu_osu_reader::discovery::resolve_osu_path(pid)
            .unwrap_or_else(|_| parent_or_default(path))
    }
    #[cfg(windows)]
    {
        let _ = pid;
        if let Some(value) = std::env::var_os("VOSU_OSU_PATH").filter(|value| !value.is_empty()) {
            return PathBuf::from(value);
        }
        command_line
            .map(windows_root_from_command_line)
            .unwrap_or_else(|| parent_or_default(path))
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = (pid, command_line);
        parent_or_default(path)
    }
}

/// Windows root rule (R19, `src/instance/discovery.rs:300-314`): the first
/// whitespace-delimited token, or the quoted span after a leading `"`, of
/// the raw OS command line is `argv[0]`; its parent directory is the root.
/// Pure string handling — deliberately does not read `VOSU_OSU_PATH` itself
/// (that stays in [`resolve_root`]) so this one function can be exercised
/// directly and deterministically by the native `cfg(windows)` test.
#[cfg_attr(
    not(windows),
    expect(dead_code, reason = "exercised only by the windows-native test")
)]
fn windows_root_from_command_line(command_line: &str) -> PathBuf {
    let command_line = command_line.trim_start();
    let executable = if let Some(quoted) = command_line.strip_prefix('"') {
        quoted.split_once('"').map_or(quoted, |(path, _)| path)
    } else {
        command_line.split_whitespace().next().unwrap_or_default()
    };
    Path::new(executable)
        .parent()
        .unwrap_or_else(|| Path::new(""))
        .to_path_buf()
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;

    fn facts(name: Option<&str>, bitness: Bitness, args: Option<StartupArgs>) -> AttachFacts {
        AttachFacts {
            pid: 123,
            generation: 1,
            handle: 1,
            bitness,
            name: name.map(str::to_owned),
            path: None,
            args,
            root: PathBuf::from("/osu"),
            executable_root: PathBuf::from("/osu"),
        }
    }

    fn no_args() -> Option<StartupArgs> {
        None
    }

    fn args(flags: &[&str]) -> Option<StartupArgs> {
        Some(StartupArgs::from_array(
            flags.iter().map(|flag| OsString::from(*flag)),
        ))
    }

    /// §7 row 1: `windows_names_and_bitness_classify_and_btree_orders` —
    /// this unit's share is "variants lazer/stable by bitness" (matcher
    /// acceptance and PID ordering are P7/host concerns).
    #[test]
    fn windows_bitness_selects_the_variant() {
        let lazer = facts(Some("OSULAZER.EXE"), Bitness::X64, no_args());
        let AttachDecision::Identify(identified) = decide(&lazer, Platform::Windows, false) else {
            panic!("expected Identify");
        };
        assert_eq!(identified.client, ClientType::Lazer);

        let stable = facts(Some("osu!.exe"), Bitness::X86, no_args());
        let AttachDecision::Identify(identified) = decide(&stable, Platform::Windows, false) else {
            panic!("expected Identify");
        };
        assert_eq!(identified.client, ClientType::Stable);
        // Windows identifications are never "named" (review #2): bitness
        // decides the variant, not a name match.
        assert!(!identified.named);
    }

    /// §7 row 4: `linux_named_first_pass_is_stable_and_fallback_is_lazer`.
    #[test]
    fn linux_named_first_pass_is_stable_and_fallback_is_lazer() {
        let named = facts(Some("wine-osu!.exe"), Bitness::X86, no_args());
        let AttachDecision::Identify(identified) = decide(&named, Platform::Linux, false) else {
            panic!("expected Identify");
        };
        assert_eq!(identified.client, ClientType::Stable);
        assert!(identified.named);

        let lone = facts(Some("osu!"), Bitness::X86, no_args());
        assert_eq!(
            decide(&lone, Platform::Linux, false),
            AttachDecision::Settle(PendingIdentify {
                settings: default_settings(),
                root: PathBuf::from("/osu"),
                executable_root: PathBuf::from("/osu"),
                pid: 123,
            })
        );

        // A named sibling already attached: no need to wait — but this
        // resolution is a settled fallback, not a "named" identification
        // (review #2): it must not itself count as a sibling later.
        let AttachDecision::Identify(identified) = decide(&lone, Platform::Linux, true) else {
            panic!("expected Identify");
        };
        assert_eq!(identified.client, ClientType::Stable);
        assert!(!identified.named);
    }

    /// §7 row 5: `command_filters_follow_presence_and_truthiness`.
    #[test]
    fn command_filters_decline_before_classification() {
        let tournament = facts(Some("osu!.exe"), Bitness::X86, args(&["--tournament="]));
        assert_eq!(
            decide(&tournament, Platform::Windows, false),
            AttachDecision::Decline
        );

        let truthy_debug = facts(
            Some("osu!.exe"),
            Bitness::X86,
            args(&["--debug-client-id=1"]),
        );
        assert_eq!(
            decide(&truthy_debug, Platform::Windows, false),
            AttachDecision::Decline
        );

        let falsy_debug = facts(
            Some("osu!.exe"),
            Bitness::X86,
            args(&["--debug-client-id=0"]),
        );
        assert!(matches!(
            decide(&falsy_debug, Platform::Windows, false),
            AttachDecision::Identify(_)
        ));

        // The literal string `"false"` (not just a falsy number) is also
        // not truthy: `=false` never declines either (review #7 gap).
        let falsy_debug_literal = facts(
            Some("osu!.exe"),
            Bitness::X86,
            args(&["--debug-client-id=false"]),
        );
        assert!(matches!(
            decide(&falsy_debug_literal, Platform::Windows, false),
            AttachDecision::Identify(_)
        ));
    }

    /// §7 row 6: `spectator_and_devserver_settings_are_typed`.
    #[test]
    fn spectator_and_devserver_settings_are_typed() {
        let attach = facts(
            Some("osu!.exe"),
            Bitness::X86,
            args(&["--spectateclient=12", "--devserver=https://example.test"]),
        );
        let AttachDecision::Identify(identified) = decide(&attach, Platform::Windows, false) else {
            panic!("expected Identify (not declined)");
        };
        assert_eq!(identified.settings.tournament_ipc_id, Some(12.0));
        assert!(identified.settings.tournament_spectator);
        assert_eq!(
            identified.settings.devserver_endpoint.as_deref(),
            Some(std::ffi::OsStr::new("https://example.test"))
        );
    }

    /// Missing `argv`/`command_line` never declines (brief: identify
    /// conservatively on absent facts).
    #[test]
    fn missing_args_never_declines() {
        let attach = facts(Some("osu!.exe"), Bitness::X86, no_args());
        let decision = decide(&attach, Platform::Windows, false);
        assert!(matches!(decision, AttachDecision::Identify(_)));
    }

    /// Missing `name` on Linux identifies immediately as stable rather than
    /// settling on a sibling signal it cannot observe.
    #[test]
    fn missing_name_on_linux_identifies_conservatively() {
        let attach = facts(None, Bitness::X86, no_args());
        let AttachDecision::Identify(identified) = decide(&attach, Platform::Linux, false) else {
            panic!("expected Identify, not an unresolvable Settle");
        };
        assert_eq!(identified.client, ClientType::Stable);
    }

    #[cfg(windows)]
    #[test]
    fn windows_root_rule_reads_argv0_from_the_raw_command_line() {
        assert_eq!(
            windows_root_from_command_line(r#""C:\Games\osu!\osu!.exe" --tournament"#),
            PathBuf::from(r"C:\Games\osu!")
        );
        assert_eq!(
            windows_root_from_command_line(r"C:\Games\osu!\osulazer.exe --devserver=x"),
            PathBuf::from(r"C:\Games\osu!")
        );
        // No parent (bare filename): falls back to an empty path, exactly
        // `Path::new("").to_path_buf()` (1.x `windows_instance_root`).
        assert_eq!(
            windows_root_from_command_line("osu!.exe"),
            PathBuf::from("")
        );
    }
}

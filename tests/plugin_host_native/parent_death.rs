//! M5 N0: core-only death, on Linux and native Windows
//! (`docs/v2/plugin-abi.md` §7.3 "Launch" and "Scope", §7.4).
//!
//! Each case kills **only** core and reads the plugin host's end from its
//! own pidfd or handle, never from PID absence ([`support::tracked`]):
//!
//! - while the plugin host is live and published;
//! - during its bootstrap: `create` is still blocked, so it never reached
//!   `READY`;
//! - while one of its sends is blocked on a full plugin → host ring, with
//!   core stopped so nothing drains it. The plugin host's own full-ring exit
//!   would come 2 s after that send started; it must be gone sooner.
//!
//! On Windows the plugin's own descendant dies with it (the plugin host's
//! kill-on-close job). On Linux the parent-death signal ends only the plugin
//! host: a descendant the plugin started outlives core, which is the
//! residual §7.3 "Scope" documents and N0's cgroup evaluation keeps
//! (`docs/testing/verification.md`, "M5 N0 unit evidence"). The Linux cases
//! assert that it survives, so a later cleanup mechanism must update them.
//!
//! Every case runs against a core helper process running production
//! `PluginHost`/`ProcessHostLauncher` code ([`CoreKind::Helper`]). The
//! `_daemon` variants run the same assertions against the real `vosu`
//! daemon with an approved `--plugin` ([`CoreKind::Daemon`]).
//!
//! [`support::tracked`]: super::support::tracked

use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::support::tree::{CoreKind, DEATH_BOUND, Scenario, Tree, run_role};

/// The plugin host's full-ring window (`FULL_RING_WAIT`, §7.2): after it
/// a blocked send ends the plugin host on its own (exit 75).
const FULL_RING_WAIT: Duration = Duration::from_secs(2);
/// The plugin host's exit after its channel was lost (§7.6, exit 75).
const EXIT_CHANNEL_LOST: u32 = 75;

/// Not a test of its own: the entry of every N0 helper process (core,
/// descendant and outside control; `support::tree`). Returns at once in an
/// ordinary run.
#[test]
fn helper_process_entry() {
    run_role();
}

pub(crate) fn daemon() -> CoreKind {
    CoreKind::Daemon(PathBuf::from(env!("CARGO_BIN_EXE_vosu")))
}

fn core_name(kind: &CoreKind) -> &'static str {
    match kind {
        CoreKind::Helper => "helper",
        CoreKind::Daemon(_) => "daemon",
    }
}

const LIVE: Scenario = Scenario {
    name: "live",
    fault: "none",
    descendant: true,
    refuse_seccomp: false,
};

const BOOTSTRAP: Scenario = Scenario {
    name: "bootstrap",
    fault: "create_block",
    descendant: true,
    refuse_seccomp: false,
};

const BLOCKED_WRITE: Scenario = Scenario {
    name: "blocked_write",
    fault: "write_block",
    descendant: false,
    refuse_seccomp: false,
};

/// Kills only core and asserts the tree's fate: the plugin host ends,
/// read from its own pidfd or handle; the plugin's descendant ends on
/// Windows and outlives core on Linux; the outside control survives.
/// Returns the kill and the moment the plugin host was seen gone.
pub(crate) fn kill_core_and_assert(tree: &mut Tree, descendant: bool) -> (Instant, Instant) {
    let host = tree.plugin_host().pid;
    if descendant {
        tree.descendant();
    }
    let killed = tree.kill_core_only();
    tree.plugin_host()
        .wait_exit(DEATH_BOUND)
        .expect("core-only death ends the plugin host");
    let gone = Instant::now();
    let host_gone = gone - killed;
    let exit = tree.plugin_host().exit_code();
    if descendant {
        let (process, _) = tree.descendant();
        #[cfg(windows)]
        assert!(
            process.wait_exit(DEATH_BOUND).is_some(),
            "the plugin host's job ends its descendant with core"
        );
        #[cfg(target_os = "linux")]
        assert!(
            process
                .wait_exit(super::support::tree::SURVIVAL_WINDOW)
                .is_none(),
            "a plugin-started descendant outlives core on Linux (the §7.3 residual); \
             if a cleanup mechanism now ends it, update this case and the residual"
        );
        let descendant_exit = process.exit_code();
        println!(
            "RECORD n0 scenario={} core={} host={host} host_gone_ms={} host_exit={exit:?} descendant_exit={descendant_exit:?}",
            tree.scenario.name,
            core_name(&tree.core.kind),
            host_gone.as_millis()
        );
        #[cfg(target_os = "linux")]
        tree.kill_descendant();
    } else {
        println!(
            "RECORD n0 scenario={} core={} host={host} host_gone_ms={} host_exit={exit:?}",
            tree.scenario.name,
            core_name(&tree.core.kind),
            host_gone.as_millis()
        );
    }
    #[cfg(target_os = "linux")]
    {
        let reaped = tree.plugin_host().reaped();
        println!(
            "RECORD n0 scenario={} host_reaped={reaped}",
            tree.scenario.name
        );
    }
    tree.assert_control_survives();
    (killed, gone)
}

fn live(kind: CoreKind) {
    let mut tree = Tree::launch(kind, LIVE);
    tree.wait_ready();
    #[cfg(target_os = "linux")]
    {
        // §7.3 "Scope": a plugin's helpers inherit no_new_privs and the
        // filter, and hold no capability.
        let (_, marker) = tree.descendant();
        assert_eq!(marker["NoNewPrivs"], "1", "{marker:?}");
        assert_eq!(marker["Seccomp"], "2", "{marker:?}");
        assert_eq!(marker["CapEff"], "0000000000000000", "{marker:?}");
        println!("RECORD n0 descendant cgroup={}", marker["cgroup"]);
    }
    kill_core_and_assert(&mut tree, true);
}

fn bootstrap(kind: CoreKind) {
    let daemon = matches!(kind, CoreKind::Daemon(_));
    let mut tree = Tree::launch(kind, BOOTSTRAP);
    let host = tree.plugin_host().pid;
    tree.descendant();
    // Still bootstrapping: `create` has not returned, so no READY.
    assert!(tree.fixture.receipts("create_released").is_empty());
    if daemon {
        let id = tree.scenario.plugin_id();
        let view = tree.core.daemon.as_ref().unwrap().plugin(&id);
        assert_eq!(
            view.as_ref().map(|view| view["state"].clone()),
            Some("starting".into()),
            "{view:?}"
        );
    } else {
        assert!(tree.created().is_none(), "create already returned");
    }
    kill_core_and_assert(&mut tree, true);
    assert!(
        tree.fixture.receipts("create_released").is_empty(),
        "plugin host {host} finished create after core died"
    );
}

fn blocked_write(kind: CoreKind) {
    let mut tree = Tree::launch(kind, BLOCKED_WRITE);
    let host = tree.plugin_host().pid;
    tree.wait_ready();
    // Nothing drains the plugin → host ring once core is stopped.
    tree.suspend_core();
    let released = Instant::now();
    tree.fixture.release(host);
    let blocked = tree
        .fixture
        .wait("write_blocked", Duration::from_secs(20))
        .expect("a flood send blocked on the full ring");
    assert_eq!(blocked.pid, host);
    let (killed, gone) = kill_core_and_assert(&mut tree, false);
    // The blocked send started after the release, so the plugin host's own
    // full-ring exit could come no sooner than FULL_RING_WAIT after it.
    let since_release = gone - released;
    assert!(
        since_release < FULL_RING_WAIT,
        "the plugin host was seen gone {since_release:?} after the flood was released \
         ({:?} after the kill): not attributable to core's death",
        gone - killed
    );
    for probe in ["write_returned", "write_refused"] {
        assert!(
            tree.fixture.receipts(probe).is_empty(),
            "the blocked send returned: {probe}"
        );
    }
    assert_ne!(
        tree.plugin_host().exit_code(),
        Some(EXIT_CHANNEL_LOST),
        "the plugin host gave up on its own channel instead of dying with core"
    );
    println!(
        "RECORD n0 scenario=blocked_write blocked_seq={} waited_ms={} gone_after_release_ms={}",
        blocked.get("seq"),
        blocked.get("waited_ms"),
        since_release.as_millis()
    );
}

#[test]
fn parent_death_core_kill_while_live_ends_the_plugin_host() {
    live(CoreKind::Helper);
}

#[test]
fn parent_death_core_kill_during_bootstrap_ends_the_plugin_host() {
    bootstrap(CoreKind::Helper);
}

#[test]
fn parent_death_core_kill_during_a_blocked_write_ends_the_plugin_host() {
    blocked_write(CoreKind::Helper);
}

#[test]
#[ignore = "needs M5 U0 integrated: the daemon's approval-gated --plugin launch"]
fn parent_death_daemon_kill_while_live_ends_the_plugin_host() {
    live(daemon());
}

#[test]
#[ignore = "needs M5 U0 integrated: the daemon's approval-gated --plugin launch"]
fn parent_death_daemon_kill_during_bootstrap_ends_the_plugin_host() {
    bootstrap(daemon());
}

#[test]
#[ignore = "needs M5 U0 integrated: the daemon's approval-gated --plugin launch"]
fn parent_death_daemon_kill_during_a_blocked_write_ends_the_plugin_host() {
    blocked_write(daemon());
}

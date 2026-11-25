//! M5 N0: the Linux plugin host's security postcondition
//! (`docs/v2/plugin-abi.md` §7.3) and the frozen packaged procedure
//! (`docs/testing/verification.md`, "N0 packaged Linux drop procedure").
//!
//! The `linux_packaged_capability_drop*` cases are `#[ignore]`d. They run
//! only after an explicitly authorized `vosu --setup` gave the packaged
//! executable named by `VOSU_PLUGIN_HOST_PACKAGED_BINARY` its file
//! capabilities, and they perform exactly the six frozen steps:
//!
//! - [`linux_packaged_capability_drop`]: steps 1–4;
//! - [`linux_packaged_capability_drop_refusal_control`]: steps 1 and 5;
//! - [`linux_packaged_capability_drop_core_only_death`]: steps 1 and 6.
//!
//! They never pass against an uncapable binary: step 1 fails first.
//!
//! The two ordinary cases run the same readback and refusal assertions
//! against a core helper process running production
//! `PluginHost`/`ProcessHostLauncher` code. That core holds no capability,
//! so they prove the sequence and its fail-closed path, not the packaged
//! drop; they never stand in for the ignored cases.

use std::path::PathBuf;
use std::time::Duration;

use super::support::fixture::Receipt;
use super::support::tracked::children_of;
use super::support::tree::{CoreKind, DEATH_BOUND, STARTUP_BOUND, Scenario, Tree};

const PACKAGED: &str = "VOSU_PLUGIN_HOST_PACKAGED_BINARY";
const EMPTY_SET: &str = "0000000000000000";

const READBACK: Scenario = Scenario {
    name: "readback",
    fault: "none",
    descendant: false,
    refuse_seccomp: false,
};

const REFUSAL: Scenario = Scenario {
    name: "refusal",
    fault: "none",
    descendant: false,
    refuse_seccomp: true,
};

/// Asserts §7.3's postcondition on one testkit readback (`constructor` or
/// `security`) and records `CapBnd` without asserting it.
fn assert_postcondition(receipt: &Receipt, when: &str) {
    for set in ["capinh", "capprm", "capeff", "capamb"] {
        assert_eq!(receipt.get(set), EMPTY_SET, "{set} {when}");
    }
    assert_eq!(receipt.get("nonewprivs"), "1", "NoNewPrivs {when}");
    assert_eq!(receipt.get("seccomp"), "2", "Seccomp {when}");
    assert_eq!(
        receipt.get("userns"),
        "EPERM",
        "the user-namespace probe {when}"
    );
    let uids = receipt.get("uid");
    assert!(
        uids.split(',').all(|uid| uid != "0"),
        "a root UID {when}: {uids}"
    );
    println!(
        "RECORD n0 linux {when} pid={} uid={uids} threads={} capbnd={}",
        receipt.pid,
        receipt.get("threads"),
        receipt.get("capbnd")
    );
}

/// Steps 2–4: the plugin host is core's `plugin-host` child, held by a
/// pidfd ([`Tree::plugin_host`]); its readback before any plugin code
/// beyond the constructor and again after the plugin started a thread.
fn assert_readbacks(tree: &mut Tree) {
    let host = tree.plugin_host().pid;
    tree.wait_ready();
    let constructor = tree
        .fixture
        .wait("constructor", STARTUP_BOUND)
        .expect("the constructor probe ran");
    assert_eq!(
        constructor.pid, host,
        "the constructor ran in the plugin host"
    );
    assert_eq!(constructor.get("threads"), "1", "single-threaded at load");
    assert_postcondition(&constructor, "at the constructor");
    let security = tree
        .fixture
        .wait("security", STARTUP_BOUND)
        .expect("the post-thread probe ran");
    assert_eq!(security.pid, host);
    assert_eq!(security.get("plugin_thread"), "started");
    let threads: u32 = security.get("threads").parse().unwrap();
    assert!(threads > 1, "the plugin's own thread exists ({threads})");
    assert_postcondition(&security, "after plugin threads");
}

/// Step 5's shared assertions once the refusal was reported: no plugin
/// code ran; the refused plugin host, held by the pidfd the test opened
/// while it waited in the refused `seccomp(2)` (`support::refusal`),
/// ended and was reaped by core; and core has no child left but the
/// outside control, live or zombie.
fn assert_refused_cleanly(tree: &mut Tree) {
    let refused = tree
        .core
        .refusals
        .as_mut()
        .expect("a refusal scenario")
        .finish();
    assert!(
        !refused.is_empty(),
        "no plugin host called seccomp(2) under the refusing filter"
    );
    assert!(tree.fixture.receipts("constructor").is_empty());
    assert!(tree.fixture.receipts("create").is_empty());
    for host in &refused {
        assert!(host.wait_exit(DEATH_BOUND).is_some(), "{host:?} ended");
        assert!(
            super::support::host::wait_until(DEATH_BOUND, || host.reaped()),
            "{host:?} was reaped by core"
        );
    }
    let core = tree.core.process.pid;
    let leftover: Vec<u32> = children_of(core)
        .into_iter()
        .filter(|pid| *pid != tree.control.pid)
        .collect();
    assert!(leftover.is_empty(), "core still has children {leftover:?}");
    println!(
        "RECORD n0 linux refusal refused_hosts={:?}",
        refused.iter().map(|host| host.pid).collect::<Vec<_>>()
    );
}

#[test]
fn linux_security_host_readback_before_constructor_and_after_threads() {
    let mut tree = Tree::launch(CoreKind::Helper, READBACK);
    assert_readbacks(&mut tree);
}

#[test]
fn linux_security_refused_filter_fails_closed_without_constructor() {
    let mut tree = Tree::launch(CoreKind::Helper, REFUSAL);
    let created = super::support::host::wait_until(STARTUP_BOUND, || tree.created().is_some());
    assert!(created, "the core helper's create returned");
    let created = tree.created().unwrap();
    assert_eq!(created["result"], "failed", "{created:?}");
    assert_eq!(created["stage"], "security", "{created:?}");
    assert_eq!(created["code"], "security_unavailable", "{created:?}");
    // Failed and never retried: past the first restart delay.
    let status = tree.status_after_create();
    assert_eq!(status["phase"], "Failed", "{status:?}");
    assert_eq!(status["restarts"], "0", "{status:?}");
    assert_eq!(status["crash_events"], "0", "{status:?}");
    assert_refused_cleanly(&mut tree);
}

// ---------------------------------------------------------------- packaged

fn packaged_binary() -> PathBuf {
    let path = std::env::var_os(PACKAGED).unwrap_or_else(|| {
        panic!("{PACKAGED} must name the packaged, setup-enabled vosu executable")
    });
    let path = std::fs::canonicalize(&path)
        .unwrap_or_else(|error| panic!("{PACKAGED}={}: {error}", path.to_string_lossy()));
    assert!(
        path.is_file(),
        "{PACKAGED}={} is not a file",
        path.display()
    );
    path
}

/// Step 1, first half: `getcap` shows the setup capabilities.
fn assert_setup_capabilities(binary: &std::path::Path) {
    let output = std::process::Command::new("getcap")
        .arg(binary)
        .output()
        .expect("getcap (libcap) is installed");
    let text = String::from_utf8_lossy(&output.stdout).to_lowercase();
    println!("RECORD n0 getcap {}", text.trim());
    assert!(
        text.contains("cap_sys_ptrace"),
        "{} lacks the setup capabilities: run the authorized `vosu --setup` first",
        binary.display()
    );
    let events = ["cap_bpf", "cap_perfmon", "cap_sys_resource"];
    let present = events.iter().filter(|cap| text.contains(*cap)).count();
    assert!(
        present == 0 || present == events.len(),
        "a partial process-event capability set: {text}"
    );
}

/// Step 1, second half: the started daemon itself holds capabilities.
fn assert_core_capable(tree: &Tree) {
    let status =
        std::fs::read_to_string(format!("/proc/{}/status", tree.core.process.pid)).unwrap();
    let effective = status
        .lines()
        .find_map(|line| line.strip_prefix("CapEff:"))
        .map(str::trim)
        .expect("CapEff");
    println!("RECORD n0 linux daemon CapEff={effective}");
    assert_ne!(
        effective, EMPTY_SET,
        "the packaged daemon runs without capabilities: not packaged evidence"
    );
}

fn packaged_tree(scenario: Scenario) -> Tree {
    let binary = packaged_binary();
    assert_setup_capabilities(&binary);
    let tree = Tree::launch(CoreKind::Daemon(binary), scenario);
    if !scenario.refuse_seccomp {
        assert_core_capable(&tree);
    }
    tree
}

#[test]
#[ignore = "packaged Linux only: after an explicitly authorized `vosu --setup` on VOSU_PLUGIN_HOST_PACKAGED_BINARY"]
fn linux_packaged_capability_drop() {
    let mut tree = packaged_tree(Scenario {
        name: "packaged",
        ..READBACK
    });
    assert_readbacks(&mut tree);
}

#[test]
#[ignore = "packaged Linux only: after an explicitly authorized `vosu --setup` on VOSU_PLUGIN_HOST_PACKAGED_BINARY"]
fn linux_packaged_capability_drop_refusal_control() {
    let mut tree = packaged_tree(Scenario {
        name: "packaged_refusal",
        ..REFUSAL
    });
    let daemon = tree.core.daemon.as_ref().unwrap();
    let id = tree.scenario.plugin_id();
    let view = daemon.wait_plugin(&id, STARTUP_BOUND, |view| view["reason"] == "security");
    assert_eq!(view["state"], "failed", "{view}");
    assert!(
        view["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("security_unavailable")),
        "{view}"
    );
    // Not retried: still failed past the first restart delay.
    std::thread::sleep(Duration::from_millis(2500));
    let view = daemon.plugin(&id).expect("the plugin is listed");
    assert_eq!(view["state"], "failed", "{view}");
    assert_eq!(view["restarts"], 0, "{view}");
    assert_eq!(view["crash_events"], 0, "{view}");
    assert_refused_cleanly(&mut tree);
}

#[test]
#[ignore = "packaged Linux only: after an explicitly authorized `vosu --setup` on VOSU_PLUGIN_HOST_PACKAGED_BINARY"]
fn linux_packaged_capability_drop_core_only_death() {
    // While live: the published plugin host dies with the daemon.
    let mut live = packaged_tree(Scenario {
        name: "packaged_live",
        ..READBACK
    });
    live.wait_ready();
    super::parent_death::kill_core_and_assert(&mut live, false);
    // During bootstrap: `create` is still blocked.
    let mut bootstrap = packaged_tree(Scenario {
        name: "packaged_bootstrap",
        fault: "create_block",
        ..READBACK
    });
    bootstrap.plugin_host();
    assert!(bootstrap.fixture.receipts("create_released").is_empty());
    super::parent_death::kill_core_and_assert(&mut bootstrap, false);
    assert!(bootstrap.fixture.receipts("create_released").is_empty());
}

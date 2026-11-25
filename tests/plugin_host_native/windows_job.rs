//! M5 N0: native Windows core-only death of a whole plugin-host tree
//! (`docs/v2/plugin-abi.md` §7.4). The tree is core → the real
//! `vosu plugin-host` running the testkit → a descendant the plugin
//! started, plus an outside control process that is not in the plugin
//! host's job.
//!
//! Before the kill the test reads, through its own OS calls rather than
//! the code under test:
//!
//! - the plugin host's immediate job at library load (the constructor's
//!   readback): kill-on-close, no breakaway, holding only the plugin host;
//! - the descendant's immediate job: the same limits, holding the plugin
//!   host, the descendant and only processes of their own tree (the console
//!   host Windows creates for a console child), every one of which must
//!   end with core;
//! - the token identity (user SID, integrity, elevation) of core, plugin
//!   host and descendant, which must all match: a process plugin runs with
//!   core's token (§7.4);
//! - the handle tables of the plugin host and the descendant, which hold no
//!   job object: neither can keep its job alive past core.
//!
//! It then kills only core, never the tree (`taskkill /T` would hide a
//! broken job), and reads every process's end from its own handle.
//!
//! [`windows_core_death_kills_job_tree`] uses a core helper process running
//! production `PluginHost`/`ProcessHostLauncher` code; its `_daemon`
//! variant runs the same assertions against the real `vosu` daemon with an
//! approved `--plugin`.

use std::collections::BTreeSet;

use super::parent_death::{daemon, kill_core_and_assert};
use super::support::jobs::{
    BREAKAWAY, KILL_ON_JOB_CLOSE, handle_types, job_type_index, token_identity,
};
use super::support::tracked::{Tracked, parent_of};
use super::support::tree::{CoreKind, DEATH_BOUND, STARTUP_BOUND, Scenario, Tree};

const JOB_TREE: Scenario = Scenario {
    name: "job_tree",
    fault: "none",
    descendant: true,
    refuse_seccomp: false,
};

fn flags(text: &str) -> u32 {
    u32::from_str_radix(text.trim_start_matches("0x"), 16).unwrap()
}

fn pid_set(text: &str) -> BTreeSet<u32> {
    text.split(',').map(|pid| pid.parse().unwrap()).collect()
}

fn assert_strict(job_flags: u32, whose: &str) {
    assert_ne!(
        job_flags & KILL_ON_JOB_CLOSE,
        0,
        "{whose}: kill-on-close ({job_flags:#x})"
    );
    assert_eq!(
        job_flags & BREAKAWAY,
        0,
        "{whose}: breakaway ({job_flags:#x})"
    );
}

fn core_death_kills_job_tree(kind: CoreKind) {
    let mut tree = Tree::launch(kind, JOB_TREE);
    tree.wait_ready();
    let host = tree.plugin_host().pid;

    // The plugin host's own job when the library loaded: only itself.
    let constructor = tree
        .fixture
        .wait("constructor", STARTUP_BOUND)
        .expect("the constructor ran");
    assert_eq!(constructor.pid, host);
    assert_strict(flags(constructor.get("job_flags")), "plugin host at load");
    assert_eq!(pid_set(constructor.get("job_pids")), BTreeSet::from([host]));

    // The descendant inherited that job, which now holds the plugin host,
    // the descendant and nothing outside their own tree: a console child
    // also gets its console host (conhost.exe), created for it and listed
    // in the same job.
    let (descendant, marker) = tree.descendant();
    let descendant_pid = descendant.pid;
    assert_strict(flags(&marker["job_flags"]), "descendant");
    let listed = pid_set(&marker["job_pids"]);
    assert!(
        listed.is_superset(&BTreeSet::from([host, descendant_pid])),
        "the descendant's immediate job is the plugin host's: {listed:?}"
    );
    let others: Vec<Tracked> = listed
        .iter()
        .filter(|pid| ![host, descendant_pid].contains(pid))
        .map(|&pid| {
            let parent = parent_of(pid);
            assert!(
                parent == Some(host) || parent == Some(descendant_pid),
                "job member {pid} (parent {parent:?}) is outside the plugin host's tree"
            );
            Tracked::open(pid, None)
        })
        .collect();

    // Same token identity throughout: process plugins run with core's
    // token; approval is a trust decision, not a sandbox.
    let core_token = token_identity(tree.core.process.raw());
    let host_token = token_identity(tree.plugin_host().raw());
    let (descendant, _) = tree.descendant();
    let descendant_token = token_identity(descendant.raw());
    assert_eq!(host_token, core_token);
    assert_eq!(descendant_token, core_token);
    println!(
        "RECORD n0 windows token integrity_rid={:#x} elevated={}",
        core_token.integrity_rid, core_token.elevated
    );

    // No job handle crossed into the tree.
    let job_type = job_type_index();
    for (whose, handle) in [
        ("plugin host", tree.plugin_host().raw()),
        ("descendant", tree.descendant().0.raw()),
    ] {
        let leaked: Vec<usize> = handle_types(handle)
            .into_iter()
            .filter(|(_, index)| *index == job_type)
            .map(|(value, _)| value)
            .collect();
        assert!(leaked.is_empty(), "{whose} holds job handles {leaked:x?}");
    }

    kill_core_and_assert(&mut tree, true);
    // Every other process the job held ends with it too.
    for process in &others {
        assert!(
            process.wait_exit(DEATH_BOUND).is_some(),
            "job member {process:?} outlived core"
        );
    }
    println!(
        "RECORD n0 windows job_members={listed:?} others_exit={:?}",
        others
            .iter()
            .map(|process| process.exit_code())
            .collect::<Vec<_>>()
    );
}

#[test]
fn windows_core_death_kills_job_tree() {
    core_death_kills_job_tree(CoreKind::Helper);
}

#[test]
fn windows_core_death_kills_job_tree_daemon() {
    core_death_kills_job_tree(daemon());
}

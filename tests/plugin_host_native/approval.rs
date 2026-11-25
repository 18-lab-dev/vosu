//! M5 U0: user-trust `--plugin` approval on the real daemon
//! (catalog-v2.md A5.5, `docs/v2/plugin-abi.md` §7.1/§7.6). A real `vosu`
//! daemon ([`daemon`]) runs P0's testkit library as a `--plugin` process
//! package; the testkit's receipts prove whether, and in which process, the
//! library was ever mapped. Every mutation goes through the dashboard
//! listener with the dashboard's origin and CSRF token, as the dashboard's
//! Plugins section sends it.
//!
//! Run with `VOSU_PLUGIN_APPROVAL_DAEMON` set (by
//! `scripts/plugin-host-fixtures.py --release-daemon`) these are the
//! prerelease release-binary evidence; the two `#[ignore]` tests need a
//! release daemon and run only with it (`-- --ignored approval_`).

#[path = "approval/daemon.rs"]
mod daemon;

use std::path::Path;
use std::time::Duration;

use daemon::{Daemon, Presented};
use reqwest::Method;
use serde_json::{Value, json};

use super::support::artifacts::{Artifact, Staged, sha256};
use super::support::fixture::Fixture;
use super::support::host::{Identity, wait_until};
use super::support::tracked::Tracked;

const ID: &str = "test.p0.testkit";
const BOUND: Duration = Duration::from_secs(30);
/// How long a refused package is watched for any sign of execution.
const QUIET: Duration = Duration::from_millis(500);
/// The WARN a load without approval logs (`plugin::approval`).
const NEEDS_APPROVAL: &str = "needs approval, which only the vosu dashboard can give";

/// One test's scratch root, staged testkit package and receipts.
struct Scenario {
    fixture: Fixture,
    staged: Staged,
    root: tempfile::TempDir,
}

impl Scenario {
    fn new() -> Self {
        let root = super::support::scratch();
        let staged = Artifact::Testkit.stage(&root.path().join("package"), &[]);
        let fixture = Fixture::new(&root.path().join("receipts"));
        fixture.control(&staged, "none", None, None);
        Self {
            fixture,
            staged,
            root,
        }
    }

    fn start(&self, executable: &Path) -> Daemon {
        Daemon::start(self.root.path(), executable, &[&self.staged.dir])
    }

    /// No process ever mapped the library: no constructor ran anywhere.
    fn assert_never_mapped(&self, daemon: &Daemon) {
        std::thread::sleep(QUIET);
        assert!(
            self.fixture.receipts("constructor").is_empty(),
            "the library was mapped: {:?}\n{}",
            self.fixture.receipts("constructor"),
            daemon.log()
        );
        assert!(self.fixture.receipts("create").is_empty());
    }

    /// The `create` receipts so far.
    fn creates(&self) -> usize {
        self.fixture.receipts("create").len()
    }

    /// Waits for a `create` from a plugin host not in `earlier` and returns
    /// its pid, which must be a child of `daemon`, not the daemon itself.
    /// Exactly `earlier.len() + 1` creates have run by then.
    fn created(&self, daemon: &Daemon, earlier: &[u32]) -> u32 {
        let count = earlier.len() + 1;
        assert!(
            wait_until(BOUND, || self.creates() >= count),
            "create #{count} never ran:\n{}",
            daemon.log()
        );
        let creates = self.fixture.receipts("create");
        assert_eq!(creates.len(), count, "{creates:?}");
        let create = creates
            .into_iter()
            .find(|receipt| !earlier.contains(&receipt.pid))
            .expect("no create from a new plugin host");
        assert_ne!(create.pid, daemon.pid(), "create ran inside the daemon");
        let constructor = self
            .fixture
            .receipts("constructor")
            .into_iter()
            .find(|receipt| receipt.pid == create.pid)
            .expect("no constructor receipt from the plugin host");
        assert_eq!(constructor.pid, create.pid);
        create.pid
    }

    fn manifest_sha256(&self) -> String {
        sha256(&self.staged.dir.join("manifest.toml"))
    }
}

/// The pending prompt, checked against the staged bytes.
fn prompt(scenario: &Scenario, daemon: &Daemon) -> Value {
    let plugin = daemon.plugin(ID);
    assert_eq!(plugin["state"], "approval_required", "{plugin}");
    assert_eq!(plugin["trust"], "user");
    assert_eq!(plugin["runtime"], "process");
    assert_eq!(plugin["approved"], false);
    let approval = plugin["approval"].clone();
    assert_eq!(approval["library_sha256"], scenario.staged.sha256.as_str());
    assert_eq!(
        approval["manifest_sha256"],
        scenario.manifest_sha256().as_str()
    );
    approval
}

fn generation(daemon: &Daemon) -> Value {
    json!({"generation": daemon.plugin(ID)["generation"]})
}

#[test]
fn approval_approve_starts_the_package_in_a_plugin_host_and_persists() {
    let (executable, _) = daemon::executable();
    let scenario = Scenario::new();
    let daemon = scenario.start(&executable);
    let approval = prompt(&scenario, &daemon);
    scenario.assert_never_mapped(&daemon);
    assert!(!daemon.approvals().exists(), "a prompt wrote the store");

    let view = daemon.act(Method::POST, ID, "approval", Some(&approval));
    assert_eq!(view["state"], "running", "{view}");
    assert_eq!(view["approved"], true);
    let host = scenario.created(&daemon, &[]);
    let record = std::fs::read_to_string(daemon.approvals()).unwrap();
    assert!(
        record.contains(ID) && record.contains(&scenario.staged.sha256),
        "{record}"
    );

    // The record persists: a new daemon runs it with no new approval.
    // The killed daemon's orphaned host is reaped by whatever adopts it,
    // and a container's PID 1 (cargo-nextest in the CI-parity container)
    // never reaps it: death is read from the host's pidfd or handle, which
    // signals at exit, not from `/proc` absence.
    let tracked = Tracked::open(host, None);
    daemon.kill();
    assert!(
        tracked.wait_exit(BOUND).is_some(),
        "the plugin host outlived its daemon"
    );
    let daemon = scenario.start(&executable);
    scenario.created(&daemon, &[host]);
    assert!(wait_until(BOUND, || daemon.plugin(ID)["state"] == "running"));
    assert_eq!(daemon.plugin(ID)["approved"], true);
}

#[test]
fn approval_denied_prompt_leaves_the_package_unloaded() {
    let (executable, _) = daemon::executable();
    let scenario = Scenario::new();
    let daemon = scenario.start(&executable);
    // The user saw the prompt and cancelled it: the dashboard sends nothing.
    let approval = prompt(&scenario, &daemon);
    // A start button pressed afterwards still cannot run it.
    let view = daemon.act(Method::POST, ID, "restart", Some(&generation(&daemon)));
    assert_eq!(view["state"], "approval_required", "{view}");
    // Approving bytes other than those shown is refused.
    let mut other = approval.clone();
    other["library_sha256"] = json!("0".repeat(64));
    let (status, body) = daemon.request(
        Method::POST,
        &format!("/api/plugins/{ID}/approval"),
        Some(&other),
        Presented::Dashboard,
    );
    assert_eq!(status, 409, "{body}");
    assert!(body.contains("approval_stale"), "{body}");
    scenario.assert_never_mapped(&daemon);
    assert!(!daemon.approvals().exists());
    assert_eq!(daemon.plugin(ID)["state"], "approval_required");
}

#[test]
fn approval_revoke_stops_the_plugin_host_and_blocks_the_next_start() {
    let (executable, _) = daemon::executable();
    let scenario = Scenario::new();
    let daemon = scenario.start(&executable);
    let approval = prompt(&scenario, &daemon);
    daemon.act(Method::POST, ID, "approval", Some(&approval));
    let host = scenario.created(&daemon, &[]);
    let identity = Identity::of(host);

    let view = daemon.act(Method::DELETE, ID, "approval", None);
    assert_eq!(view["state"], "approval_required", "{view}");
    assert_eq!(view["approved"], false);
    assert_eq!(scenario.fixture.wait("destroy", BOUND).unwrap().pid, host);
    assert!(
        wait_until(BOUND, || !identity.alive()),
        "the revoked plugin host survived"
    );
    let store = std::fs::read_to_string(daemon.approvals()).unwrap();
    assert!(
        !store.contains(ID),
        "the record survived revocation: {store}"
    );

    let view = daemon.act(Method::POST, ID, "restart", Some(&generation(&daemon)));
    assert_eq!(view["state"], "approval_required", "{view}");
    std::thread::sleep(QUIET);
    assert_eq!(scenario.creates(), 1, "a revoked package ran again");
}

#[test]
fn approval_changed_library_is_refused_on_restart() {
    let (executable, _) = daemon::executable();
    let scenario = Scenario::new();
    let daemon = scenario.start(&executable);
    let approval = prompt(&scenario, &daemon);
    daemon.act(Method::POST, ID, "approval", Some(&approval));
    let identity = Identity::of(scenario.created(&daemon, &[]));
    // Stop first: Windows keeps a mapped library open.
    let view = daemon.act(Method::POST, ID, "stop", Some(&generation(&daemon)));
    assert_eq!(view["state"], "stopped", "{view}");
    assert!(wait_until(BOUND, || !identity.alive()));

    // A rebuilt library: same path, other bytes.
    let mut bytes = std::fs::read(&scenario.staged.library).unwrap();
    bytes.extend_from_slice(b"\0u0 changed library");
    std::fs::write(&scenario.staged.library, &bytes).unwrap();

    let view = daemon.act(Method::POST, ID, "restart", Some(&generation(&daemon)));
    assert_eq!(view["state"], "approval_required", "{view}");
    assert_eq!(view["approved"], false);
    let pending = view["approval"].clone();
    assert_eq!(
        pending["library_sha256"],
        sha256(&scenario.staged.library).as_str()
    );
    assert_ne!(pending["library_sha256"], approval["library_sha256"]);
    assert_eq!(pending["manifest_sha256"], approval["manifest_sha256"]);
    // The old approval no longer applies to the new bytes.
    let (status, body) = daemon.request(
        Method::POST,
        &format!("/api/plugins/{ID}/approval"),
        Some(&approval),
        Presented::Dashboard,
    );
    assert_eq!(status, 409, "{body}");

    // A new daemon refuses it too.
    daemon.kill();
    let daemon = scenario.start(&executable);
    assert_eq!(daemon.plugin(ID)["state"], "approval_required");
    std::thread::sleep(QUIET);
    assert_eq!(scenario.creates(), 1, "the changed library ran");
}

#[test]
fn approval_near_miss_management_requests_are_refused() {
    let (executable, _) = daemon::executable();
    let scenario = Scenario::new();
    let daemon = scenario.start(&executable);
    let approval = prompt(&scenario, &daemon);
    let approve = format!("/api/plugins/{ID}/approval");
    for (presented, expected) in [
        (Presented::NoToken, 403),
        (Presented::WrongToken, 403),
        (Presented::ForeignOrigin, 403),
        (Presented::OverlayListener, 404),
    ] {
        let (status, body) = daemon.request(Method::POST, &approve, Some(&approval), presented);
        assert_eq!(status, expected, "POST approval with {presented:?}: {body}");
        let (status, body) = daemon.request(
            Method::POST,
            &format!("/api/plugins/{ID}/restart"),
            Some(&generation(&daemon)),
            presented,
        );
        assert_eq!(status, expected, "POST restart with {presented:?}: {body}");
    }
    let (status, _) = daemon.request(Method::DELETE, &approve, None, Presented::NoToken);
    assert_eq!(status, 403, "DELETE approval without a token");
    for (method, path) in [
        (Method::PUT, approve.clone()),
        (Method::POST, format!("/api/plugins/{ID}/approval/")),
        (Method::POST, format!("/api/plugins/{ID}/Approval")),
        (Method::POST, format!("/api/plugins/{ID}x/approval")),
        (Method::POST, "/api/plugins//approval".to_owned()),
    ] {
        let (status, body) =
            daemon.request(method.clone(), &path, Some(&approval), Presented::Dashboard);
        assert!(
            (400..500).contains(&status),
            "{method} {path} -> {status}: {body}"
        );
    }
    scenario.assert_never_mapped(&daemon);
    assert!(!daemon.approvals().exists(), "a near miss wrote the store");

    // The same approval, authorized, is what the refusals withheld.
    let view = daemon.act(Method::POST, ID, "approval", Some(&approval));
    assert_eq!(view["state"], "running", "{view}");
    scenario.created(&daemon, &[]);
}

#[test]
fn approval_non_interactive_daemon_keeps_the_package_unloaded() {
    let (executable, _) = daemon::executable();
    let scenario = Scenario::new();
    // Nobody answers: the daemon runs with no approval ever arriving.
    let daemon = scenario.start(&executable);
    assert!(
        daemon.wait_for_log(NEEDS_APPROVAL, BOUND),
        "no actionable WARN:\n{}",
        daemon.log()
    );
    scenario.assert_never_mapped(&daemon);
    let plugin = daemon.plugin(ID);
    assert_eq!(plugin["state"], "approval_required", "{plugin}");
    assert_eq!(plugin["reason"], "approval");
    assert!(!daemon.approvals().exists(), "approval was implied");
}

/// The release daemon is a prerelease build without the test harness, and
/// a user-trust in-process request is refused before its library is mapped
/// (never rewritten to `process`), while its process sibling waits for
/// approval.
#[test]
#[ignore = "needs VOSU_PLUGIN_APPROVAL_DAEMON (plugin-host-fixtures.py --release-daemon)"]
fn approval_release_daemon_refuses_in_process_without_the_test_harness() {
    let (executable, release) = daemon::executable();
    assert!(release, "{} is unset", daemon::RELEASE_DAEMON);
    assert!(
        !daemon::version(&executable).pre.is_empty(),
        "not a prerelease"
    );
    let image = std::fs::read(&executable).unwrap();
    for marker in [
        "/__vosu_test/",
        "x-vosu-test-capture",
        "VOSU_TEST_ADAPTER_CAPTURE",
    ] {
        assert!(
            !image
                .windows(marker.len())
                .any(|window| window == marker.as_bytes()),
            "the release daemon contains test-harness marker {marker}"
        );
    }

    let scenario = Scenario::new();
    let local = Artifact::Testkit.stage(
        &scenario.root.path().join("in-process"),
        &[
            ("runtime = 'process'", "runtime = 'in-process'"),
            ("test.p0.testkit", "test.u0.inprocess"),
        ],
    );
    let local_receipts = Fixture::new(&scenario.root.path().join("in-process-receipts"));
    local_receipts.control(&local, "none", None, None);
    let daemon = Daemon::start(
        scenario.root.path(),
        &executable,
        &[&local.dir, &scenario.staged.dir],
    );
    prompt(&scenario, &daemon);
    let refused = daemon
        .plugins()
        .into_iter()
        .find(|plugin| {
            plugin["provenance"]["path"]
                .as_str()
                .is_some_and(|path| path.contains("in-process"))
        })
        .expect("the refused package is listed");
    assert_eq!(refused["state"], "failed", "{refused}");
    assert_eq!(refused["reason"], "load");
    assert!(
        refused["detail"]
            .as_str()
            .unwrap()
            .contains("in-process runtime refused"),
        "{refused}"
    );
    std::thread::sleep(QUIET);
    assert!(
        local_receipts.receipts("constructor").is_empty(),
        "the refused in-process library was mapped"
    );
    scenario.assert_never_mapped(&daemon);
}

/// A stable release build refuses every `--plugin` package before reading
/// it; there is nothing to approve.
#[test]
#[ignore = "needs VOSU_PLUGIN_APPROVAL_STABLE_DAEMON, a stable-version release build"]
fn approval_stable_release_daemon_refuses_every_plugin() {
    let executable = std::path::PathBuf::from(
        std::env::var_os(daemon::STABLE_DAEMON)
            .unwrap_or_else(|| panic!("{} is unset", daemon::STABLE_DAEMON)),
    );
    assert!(
        daemon::version(&executable).pre.is_empty(),
        "not a stable version"
    );
    let scenario = Scenario::new();
    let daemon = scenario.start(&executable);
    let plugins = daemon.plugins();
    let refused = plugins
        .iter()
        .find(|plugin| plugin["trust"] == "user")
        .unwrap_or_else(|| panic!("the --plugin package is not listed: {plugins:?}"));
    assert_eq!(
        refused["id"],
        Value::Null,
        "the stable build read the manifest"
    );
    assert_eq!(refused["state"], "failed");
    assert_eq!(refused["reason"], "load");
    assert_eq!(refused["approval"], Value::Null);
    assert!(
        refused["detail"]
            .as_str()
            .unwrap()
            .contains("stable release builds refuse"),
        "{refused}"
    );
    scenario.assert_never_mapped(&daemon);
    assert!(!daemon.approvals().exists());
}

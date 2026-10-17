use super::*;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::broadcast;

#[derive(Default)]
struct RecordingReplace {
    calls: AtomicUsize,
    fail: AtomicBool,
    same_directory: AtomicBool,
}

impl AtomicReplace for RecordingReplace {
    fn replace(&self, temp: &Path, target: &Path) -> io::Result<()> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.same_directory
            .store(temp.parent() == target.parent(), Ordering::SeqCst);
        if self.fail.swap(false, Ordering::SeqCst) {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "injected"));
        }
        crate::platform::atomic_replace(temp, target)
    }
}

struct RemovingReplace {
    source: std::path::PathBuf,
}

impl AtomicReplace for RemovingReplace {
    fn replace(&self, temp: &Path, target: &Path) -> io::Result<()> {
        crate::platform::atomic_replace(temp, target)?;
        match std::fs::remove_file(&self.source) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}

fn options(replacer: Arc<RecordingReplace>, debounce_ms: u64) -> ConfigStoreOptions {
    ConfigStoreOptions {
        debounce: Duration::from_millis(debounce_ms),
        atomic_replace: replacer,
    }
}

async fn changed(receiver: &mut broadcast::Receiver<ConfigChanged>) -> ConfigChanged {
    tokio::time::timeout(Duration::from_secs(3), receiver.recv())
        .await
        .expect("config change timeout")
        .expect("config change channel closed")
}

async fn wait_for_value(
    receiver: &mut broadcast::Receiver<ConfigChanged>,
    predicate: impl Fn(&Config) -> bool,
) -> ConfigChanged {
    loop {
        let event = changed(receiver).await;
        if predicate(&event.new) {
            return event;
        }
    }
}

const DEFAULT_TOML: &str = "\
DEBUG_LOG = false
ENABLE_AUTOUPDATE = true
OPEN_DASHBOARD_ON_STARTUP = true
START_AT_LOGIN = false

SHOW_MP_COMMANDS = false
CALCULATE_PP = true
READ_MANIA_SCROLL_SPEED = true

ENABLE_KEY_OVERLAY = true

ENABLE_MANIA_KEY_HIT_ERRORS = false
POLL_RATE = 150.0
PRECISE_DATA_POLL_RATE = 10.0
V2_SUBSCRIPTION_DEBOUNCE = 1.0

SERVER_IP = \"127.0.0.1\"
SERVER_PORT = 24050.0
DASHBOARD_IP = \"127.0.0.1\"
DASHBOARD_PORT = 24051.0
ALLOWED_IPS = \"127.0.0.1,localhost,absolute\"

STATIC_FOLDER_PATH = \"./static\"
";

#[test]
fn defaults_and_serialization_match_canonical_toml_bytes() {
    let document = ConfigDocument::parse(DEFAULT_TOML).expect("parse canonical defaults");
    assert_eq!(document.config(), &Config::default());
    assert_eq!(document.serialize().as_bytes(), DEFAULT_TOML.as_bytes());
    assert_eq!(
        DEFAULT_SCHEMA
            .items()
            .iter()
            .map(|item| item.binding)
            .collect::<Vec<_>>(),
        [
            "ENABLE_AUTOUPDATE",
            "OPEN_DASHBOARD_ON_STARTUP",
            "START_AT_LOGIN",
            "DEBUG_LOG",
            "CALCULATE_PP",
            "ENABLE_KEY_OVERLAY",
            "ENABLE_MANIA_KEY_HIT_ERRORS",
            "POLL_RATE",
            "PRECISE_DATA_POLL_RATE",
            "V2_SUBSCRIPTION_DEBOUNCE",
            "SHOW_MP_COMMANDS",
            "READ_MANIA_SCROLL_SPEED",
            "SERVER_IP",
            "SERVER_PORT",
            "DASHBOARD_IP",
            "DASHBOARD_PORT",
            "STATIC_FOLDER_PATH",
            "ALLOWED_IPS",
        ]
    );
}

#[test]
fn mania_key_hit_errors_defaults_off_and_round_trips() {
    assert!(
        !ConfigDocument::parse("")
            .unwrap()
            .config()
            .enable_mania_key_hit_errors
    );
    let document = ConfigDocument::parse("ENABLE_MANIA_KEY_HIT_ERRORS = true\n").unwrap();
    assert!(document.config().enable_mania_key_hit_errors);
    assert!(
        ConfigDocument::parse(&document.serialize())
            .unwrap()
            .config()
            .enable_mania_key_hit_errors
    );
}

#[test]
fn start_at_login_defaults_parses_refreshes_and_round_trips() {
    assert!(!ConfigDocument::parse("").unwrap().config().start_at_login);
    let mut document = ConfigDocument::parse("START_AT_LOGIN = true\n").unwrap();
    assert!(document.config().start_at_login);
    assert!(document.serialize().contains("START_AT_LOGIN = true\n"));
    assert!(
        ConfigDocument::parse(&document.serialize())
            .unwrap()
            .config()
            .start_at_login
    );
    document.refresh("DEBUG_LOG = true\n").unwrap();
    assert!(document.config().start_at_login);
    document.refresh("START_AT_LOGIN = \"true\"\n").unwrap();
    assert!(!document.config().start_at_login);
    document.refresh("START_AT_LOGIN = true\n").unwrap();
    document.refresh("START_AT_LOGIN = false\n").unwrap();
    assert!(!document.config().start_at_login);
}

#[test]
fn serialization_preserves_toml_types_escaping_and_fractional_values() {
    let config = Config {
        debug_log: true,
        poll_rate: 150.25,
        server_port: 24_050.5,
        static_folder_path: "folder \"quoted\"\\line\nnext".to_owned(),
        allowed_ips: "host-a,host-b,absolute".to_owned(),
        ..Config::default()
    };

    let serialized = ConfigDocument::new(config.clone()).serialize();
    let values = toml::from_str::<toml::Table>(&serialized).expect("parse serialized TOML");

    assert_eq!(values["DEBUG_LOG"].as_bool(), Some(true));
    assert_eq!(values["POLL_RATE"].as_float(), Some(150.25));
    assert_eq!(values["SERVER_PORT"].as_float(), Some(24_050.5));
    assert_eq!(
        values["STATIC_FOLDER_PATH"].as_str(),
        Some("folder \"quoted\"\\line\nnext")
    );
    assert_eq!(
        values["ALLOWED_IPS"].as_str(),
        Some("host-a,host-b,absolute"),
        "ALLOWED_IPS remains one TOML string"
    );
    assert_eq!(
        ConfigDocument::parse(&serialized)
            .expect("round-trip serialized TOML")
            .into_config(),
        config
    );
}

#[test]
fn refresh_retains_bindings_missing_from_toml() {
    let mut document = ConfigDocument::parse(
        "SERVER_IP = \"localhost\"\nSERVER_PORT = 1234\nENABLE_AUTOUPDATE = false\n",
    )
    .expect("parse initial TOML");

    let warnings = document
        .refresh("DEBUG_LOG = true\n")
        .expect("refresh valid TOML");

    assert!(warnings.is_empty());
    assert_eq!(document.config().server_ip, "localhost");
    assert_eq!(document.config().server_port, 1234.0);
    assert!(!document.config().enable_auto_update);
    assert!(document.config().debug_log);
}

#[test]
fn known_bindings_with_invalid_toml_types_use_schema_defaults() {
    let mut document = ConfigDocument::parse(
        "\
ENABLE_AUTOUPDATE = false
DEBUG_LOG = true
POLL_RATE = 250.0
SERVER_IP = \"custom\"
SERVER_PORT = 1234.0
STATIC_FOLDER_PATH = \"custom\"
ALLOWED_IPS = \"custom\"
",
    )
    .expect("parse initial TOML");

    document
        .refresh(
            "\
ENABLE_AUTOUPDATE = \"true\"
DEBUG_LOG = 1
POLL_RATE = inf
SERVER_IP = 123
SERVER_PORT = \"25000\"
STATIC_FOLDER_PATH = false
ALLOWED_IPS = [\"host-a\", \"host-b\"]
",
        )
        .expect("wrong types are per-key fallbacks");

    assert!(document.config().enable_auto_update);
    assert!(!document.config().debug_log);
    assert_eq!(document.config().poll_rate, 150.0);
    assert_eq!(document.config().server_ip, "127.0.0.1");
    assert_eq!(document.config().server_port, 24_050.0);
    assert_eq!(document.config().static_folder_path, "./static");
    assert_eq!(
        document.config().allowed_ips,
        "127.0.0.1,localhost,absolute"
    );
}

#[test]
fn malformed_refresh_rolls_back_without_partial_changes() {
    let mut document = ConfigDocument::parse("DEBUG_LOG = false\nSERVER_PORT = 1234.5\n")
        .expect("parse initial TOML");
    let before = document.clone();

    assert!(
        document
            .refresh("DEBUG_LOG = true\nSERVER_PORT = [")
            .is_err()
    );
    assert_eq!(document, before);
}

#[test]
fn schema_minima_are_clamped() {
    let mut document = ConfigDocument::default();
    document
        .refresh(
            "\
POLL_RATE = -5.0
PRECISE_DATA_POLL_RATE = 0.0
V2_SUBSCRIPTION_DEBOUNCE = -0.25
SERVER_PORT = -1.5
",
        )
        .expect("parse clamped TOML");

    assert_eq!(document.config().poll_rate, 100.0);
    assert_eq!(document.config().precise_data_poll_rate, 1.0);
    assert_eq!(document.config().v2_subscription_debounce, 1.0);
    assert_eq!(document.config().server_port, -1.5);
}

#[test]
fn oversized_disk_intervals_are_bounded_before_publication() {
    let document = ConfigDocument::parse(
        "POLL_RATE = 1e100\nPRECISE_DATA_POLL_RATE = 1e300\nV2_SUBSCRIPTION_DEBOUNCE = 1e200\n",
    )
    .unwrap();
    assert_eq!(document.config().poll_rate, ConfigDocument::MAX_INTERVAL_MS);
    assert_eq!(
        document.config().precise_data_poll_rate,
        ConfigDocument::MAX_INTERVAL_MS
    );
    assert_eq!(
        document.config().v2_subscription_debounce,
        ConfigDocument::MAX_INTERVAL_MS
    );
}

#[test]
fn initialization_uses_toml_then_vosu_env_then_tosu_env_then_defaults() {
    let temp = tempfile::tempdir().expect("temporary config directory");
    let path = temp.path().join("vosu.toml");
    std::fs::write(
        temp.path().join("tosu.env"),
        "DEBUG_LOG=true\nSERVER_IP=tosu-host\nSERVER_PORT=11000\n",
    )
    .expect("write tosu.env");
    std::fs::write(
        temp.path().join("vosu.env"),
        "\
DEBUG_LOG=true
SERVER_PORT=22000
POLL_RATE=0x96
ENABLE_AUTOUPDATE=1
STATIC_FOLDER_PATH=first
STATIC_FOLDER_PATH=\"folder # retained\" # ignored comment
",
    )
    .expect("write vosu.env");
    std::fs::write(
        &path,
        "\
DEBUG_LOG = \"wrong type shadows legacy\"
SERVER_PORT = 33000.5
UNKNOWN_BINDING = true
",
    )
    .expect("write vosu.toml");
    let replacer = RecordingReplace::default();

    let initial = super::migration::initialize_file(&path, &replacer).expect("initialize config");
    assert!(replacer.same_directory.load(Ordering::SeqCst));
    assert_eq!(replacer.calls.load(Ordering::SeqCst), 1);

    assert!(
        !initial.config.debug_log,
        "wrong TOML type uses schema default"
    );
    assert!(
        !initial.config.enable_auto_update,
        "legacy bool syntax is retained"
    );
    assert_eq!(initial.config.server_port, 33_000.5, "TOML wins");
    assert_eq!(initial.config.poll_rate, 150.0, "vosu.env fills a gap");
    assert_eq!(
        initial.config.server_ip, "tosu-host",
        "tosu.env fills a gap"
    );
    assert_eq!(
        initial.config.static_folder_path, "folder # retained",
        "legacy duplicates are last-wins and quoted comments are retained"
    );
    assert_eq!(
        initial.config.allowed_ips,
        Config::default().allowed_ips,
        "schema defaults are the final source"
    );
    let canonical = toml::from_str::<toml::Table>(std::str::from_utf8(&initial.bytes).unwrap())
        .expect("canonical TOML");
    assert_eq!(canonical.len(), BINDING_ORDER.len());
    assert!(!canonical.contains_key("UNKNOWN_BINDING"));
    assert_eq!(
        std::fs::read(&path).expect("read canonical file"),
        initial.bytes
    );
}

#[test]
fn legacy_migration_accepts_export_followed_by_tab() {
    let temp = tempfile::tempdir().expect("temporary config directory");
    let path = temp.path().join("vosu.toml");
    let legacy = temp.path().join("vosu.env");
    std::fs::write(
        &legacy,
        "export\tSERVER_PORT=25001\nexport \tSERVER_IP=export-host\n",
    )
    .expect("write vosu.env");
    let replacer = RecordingReplace::default();

    let initial = super::migration::initialize_file(&path, &replacer).expect("initialize config");

    assert_eq!(initial.config.server_port, 25_001.0);
    assert_eq!(initial.config.server_ip, "export-host");
    assert!(
        initial
            .warnings
            .iter()
            .all(|warning| !warning.starts_with("Removed deprecated")),
        "the export prefix must not become part of a diagnostic binding"
    );
    assert!(!legacy.exists(), "readable migration input is removed");
}

#[test]
fn malformed_authoritative_toml_does_not_touch_any_source() {
    let temp = tempfile::tempdir().expect("temporary config directory");
    let path = temp.path().join("vosu.toml");
    let toml_before = "DEBUG_LOG = false\nSERVER_PORT = [";
    let vosu_before = "DEBUG_LOG=true\n";
    let tosu_before = "SERVER_IP=tosu-host\n";
    std::fs::write(&path, toml_before).expect("write malformed TOML");
    std::fs::write(temp.path().join("vosu.env"), vosu_before).expect("write vosu.env");
    std::fs::write(temp.path().join("tosu.env"), tosu_before).expect("write tosu.env");
    let replacer = RecordingReplace::default();

    let error = super::migration::initialize_file(&path, &replacer)
        .err()
        .expect("malformed TOML must fail");

    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert_eq!(replacer.calls.load(Ordering::SeqCst), 0);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), toml_before);
    assert_eq!(
        std::fs::read_to_string(temp.path().join("vosu.env")).unwrap(),
        vosu_before
    );
    assert_eq!(
        std::fs::read_to_string(temp.path().join("tosu.env")).unwrap(),
        tosu_before
    );
}

#[test]
fn failed_atomic_replace_leaves_toml_and_legacy_sources_unchanged() {
    let temp = tempfile::tempdir().expect("temporary config directory");
    let path = temp.path().join("vosu.toml");
    let toml_before = "SERVER_IP = \"toml-host\"\n";
    let vosu_before = "SERVER_IP=vosu-host\n";
    let tosu_before = "SERVER_IP=tosu-host\n";
    std::fs::write(&path, toml_before).expect("write TOML");
    std::fs::write(temp.path().join("vosu.env"), vosu_before).expect("write vosu.env");
    std::fs::write(temp.path().join("tosu.env"), tosu_before).expect("write tosu.env");
    let replacer = RecordingReplace::default();
    replacer.fail.store(true, Ordering::SeqCst);

    let error = super::migration::initialize_file(&path, &replacer)
        .err()
        .expect("injected replace failure");

    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert_eq!(replacer.calls.load(Ordering::SeqCst), 1);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), toml_before);
    assert_eq!(
        std::fs::read_to_string(temp.path().join("vosu.env")).unwrap(),
        vosu_before
    );
    assert_eq!(
        std::fs::read_to_string(temp.path().join("tosu.env")).unwrap(),
        tosu_before
    );
}

#[cfg(unix)]
#[test]
fn canonical_config_symlink_is_rejected_without_touching_link_or_target() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("temporary config directory");
    let target = temp.path().join("managed.toml");
    let path = temp.path().join("vosu.toml");
    let target_before = "SERVER_PORT = [";
    std::fs::write(&target, target_before).expect("write managed target");
    symlink(&target, &path).expect("create canonical config symlink");
    let replacer = RecordingReplace::default();

    let initialize_error = super::migration::initialize_file(&path, &replacer)
        .err()
        .expect("canonical symlink must be rejected before it is read");
    assert_eq!(initialize_error.kind(), io::ErrorKind::InvalidInput);

    let write_error = super::io::atomic_write(&path, b"SERVER_PORT = 25000.0\n", &replacer)
        .expect_err("canonical symlink must be rejected before a write");
    assert_eq!(write_error.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(replacer.calls.load(Ordering::SeqCst), 0);
    assert!(
        std::fs::symlink_metadata(&path)
            .expect("read link metadata")
            .file_type()
            .is_symlink()
    );
    assert_eq!(std::fs::read_link(&path).expect("read link"), target);
    assert_eq!(
        std::fs::read_to_string(&target).expect("read managed target"),
        target_before
    );
}

#[test]
fn successful_replace_removes_readable_legacy_sources_and_tolerates_not_found() {
    let temp = tempfile::tempdir().expect("temporary config directory");
    let path = temp.path().join("vosu.toml");
    let vosu_env = temp.path().join("vosu.env");
    let tosu_env = temp.path().join("tosu.env");
    std::fs::write(&vosu_env, "SERVER_PORT=25000\n").expect("write vosu.env");
    std::fs::write(&tosu_env, "SERVER_IP=tosu-host\n").expect("write tosu.env");
    let replacer = RemovingReplace {
        source: vosu_env.clone(),
    };

    let initial = super::migration::initialize_file(&path, &replacer).expect("initialize config");

    assert_eq!(initial.config.server_port, 25_000.0);
    assert_eq!(initial.config.server_ip, "tosu-host");
    assert!(path.exists());
    assert!(!vosu_env.exists());
    assert!(!tosu_env.exists());
}

#[test]
fn bare_relative_target_resolves_against_an_explicit_absolute_directory() {
    let temp = tempfile::tempdir().expect("temporary config directory");

    let resolved = super::store::absolute_config_path_from(PathBuf::from("vosu.toml"), temp.path());

    assert!(resolved.is_absolute());
    assert_eq!(resolved, temp.path().join("vosu.toml"));
}

#[tokio::test]
async fn before_publish_transactions_serialize_concurrent_resource_swaps() {
    let temp = tempfile::tempdir().expect("temporary config directory");
    let path = temp.path().join("vosu.toml");
    let store = Arc::new(
        ConfigStore::initialize(&path)
            .await
            .expect("initialize config"),
    );
    let resource = Arc::new(Mutex::new(Vec::new()));
    let (entered_send, entered_receive) = std::sync::mpsc::channel();
    let (release_send, release_receive) = std::sync::mpsc::channel();
    let release_receive = Arc::new(Mutex::new(release_receive));
    store.register_before_publish({
        let resource = Arc::clone(&resource);
        move |next| {
            resource
                .lock()
                .expect("resource mutex")
                .push(next.static_folder_path.clone());
            if next.static_folder_path == "static-b" {
                entered_send.send(()).expect("announce blocked publication");
                release_receive
                    .lock()
                    .expect("release mutex")
                    .recv()
                    .expect("release blocked publication");
            }
        }
    });

    let first = std::thread::spawn({
        let store = Arc::clone(&store);
        move || store.update(|config| config.static_folder_path = "static-b".into())
    });
    entered_receive
        .recv_timeout(Duration::from_secs(1))
        .expect("first publication entered hook");
    let second = std::thread::spawn({
        let store = Arc::clone(&store);
        move || store.update(|config| config.static_folder_path = "static-c".into())
    });
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(store.current().static_folder_path, "./static");
    assert_eq!(
        resource.lock().expect("resource mutex").as_slice(),
        &["./static".to_owned(), "static-b".to_owned()]
    );

    release_send.send(()).expect("release first publication");
    assert!(first.join().expect("first update thread"));
    assert!(second.join().expect("second update thread"));
    assert_eq!(
        resource.lock().expect("resource mutex").as_slice(),
        &[
            "./static".to_owned(),
            "static-b".to_owned(),
            "static-c".to_owned(),
        ]
    );
    assert_eq!(store.current().static_folder_path, "static-c");
    store.shutdown().await.expect("shutdown config");
}

#[tokio::test]
async fn lifecycle_debounces_saves_and_suppresses_own_hash_event() {
    let temp = tempfile::tempdir().expect("temporary config directory");
    let path = temp.path().join("vosu.toml");
    let replacer = Arc::new(RecordingReplace::default());
    let store = ConfigStore::initialize_with_options(&path, options(replacer.clone(), 50))
        .await
        .expect("initialize config");
    replacer.calls.store(0, Ordering::SeqCst);
    let mut changes = store.subscribe();

    store.update(|config| config.server_port = 24_051.0);
    let first = changed(&mut changes).await;
    store.update(|config| config.server_port = 24_052.0);
    let second = changed(&mut changes).await;
    store.update(|config| config.server_port = 24_053.0);
    let third = changed(&mut changes).await;
    assert_eq!(first.old.server_port, 24_050.0);
    assert_eq!(first.new.server_port, 24_051.0);
    assert_eq!(second.old.server_port, 24_051.0);
    assert_eq!(second.new.server_port, 24_052.0);
    assert_eq!(third.old.server_port, 24_052.0);
    assert_eq!(third.new.server_port, 24_053.0);

    // Wait for the debounced save itself rather than a fixed delay: `RecordingReplace`
    // counts the call before the rename lands, and a busy CI runner can take longer than
    // the 50 ms debounce plus a fixed margin.
    let saved_port = || {
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| ConfigDocument::parse(&text).ok())
            .map(|document| document.config().server_port)
    };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while saved_port() != Some(24_053.0) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "debounced save did not land within 5 s"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // The three updates must coalesce into exactly one save, including after a further
    // debounce period.
    tokio::time::sleep(Duration::from_millis(120)).await;
    assert_eq!(replacer.calls.load(Ordering::SeqCst), 1);
    assert!(
        tokio::time::timeout(Duration::from_millis(150), changes.recv())
            .await
            .is_err(),
        "self-generated watcher events must be hash suppressed"
    );
    store.shutdown().await.expect("shutdown config");
}

#[tokio::test]
async fn lifecycle_reloads_external_create_modify_and_rename_with_exact_snapshots() {
    let temp = tempfile::tempdir().expect("temporary config directory");
    let path = temp.path().join("vosu.toml");
    let store = ConfigStore::initialize(&path)
        .await
        .expect("initialize config");
    let mut changes = store.subscribe();

    std::fs::write(&path, "SERVER_PORT = 25001.0\n").expect("external modify");
    let modified = wait_for_value(&mut changes, |config| config.server_port == 25_001.0).await;
    assert_eq!(modified.old.server_port, 24_050.0);
    assert_eq!(modified.new.server_port, 25_001.0);
    std::fs::write(&path, "SERVER_PORT = 25099.0\nBROKEN = [\n")
        .expect("malformed external modify");
    assert!(
        tokio::time::timeout(Duration::from_millis(250), changes.recv())
            .await
            .is_err(),
        "malformed TOML must not publish a snapshot"
    );
    assert_eq!(
        store.current().server_port,
        25_001.0,
        "malformed TOML must retain the last-good snapshot"
    );

    std::fs::remove_file(&path).expect("remove config");
    std::fs::write(&path, "SERVER_PORT = 25002.0\n").expect("external create");
    let created = wait_for_value(&mut changes, |config| config.server_port == 25_002.0).await;
    assert_eq!(created.old.server_port, 25_001.0);
    assert_eq!(created.new.server_port, 25_002.0);

    let replacement = temp.path().join("external.toml");
    std::fs::write(&replacement, "SERVER_PORT = 25003.0\n").expect("write rename replacement");
    std::fs::rename(&replacement, &path).expect("external rename");
    let renamed = wait_for_value(&mut changes, |config| config.server_port == 25_003.0).await;
    assert_eq!(renamed.old.server_port, 25_002.0);
    assert_eq!(renamed.new.server_port, 25_003.0);
    store.shutdown().await.expect("shutdown config");
}

#[tokio::test]
async fn before_publish_barrier_runs_before_snapshot_becomes_observable() {
    let temp = tempfile::tempdir().expect("temporary config directory");
    let path = temp.path().join("vosu.toml");
    let store = Arc::new(
        ConfigStore::initialize(&path)
            .await
            .expect("initialize config"),
    );
    let observed = Arc::new(Mutex::new(Vec::new()));
    let weak_store = Arc::downgrade(&store);
    let hook_observed = Arc::clone(&observed);
    store.register_before_publish(move |next| {
        let visible = weak_store.upgrade().expect("store remains alive").current();
        hook_observed.lock().expect("observations mutex").push((
            visible.static_folder_path.clone(),
            next.static_folder_path.clone(),
        ));
    });
    let mut snapshots = store.subscribe_current();

    assert!(store.update(|config| config.static_folder_path = "new-root".to_owned()));
    snapshots.changed().await.expect("published snapshot");

    assert_eq!(
        observed.lock().expect("observations mutex").as_slice(),
        &[
            ("./static".to_owned(), "./static".to_owned()),
            ("./static".to_owned(), "new-root".to_owned()),
        ],
        "the hook must see the old public snapshot while installing the new generation"
    );
    assert_eq!(snapshots.borrow().static_folder_path, "new-root");
    store.shutdown().await.expect("shutdown config");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_publications_keep_root_hook_and_snapshot_in_order() {
    let temp = tempfile::tempdir().expect("temporary config directory");
    let path = temp.path().join("vosu.toml");
    let store = Arc::new(
        ConfigStore::initialize(&path)
            .await
            .expect("initialize config"),
    );
    let roots = Arc::new(Mutex::new(Vec::new()));
    let (entered_b_send, entered_b_receive) = std::sync::mpsc::channel();
    let (release_b_send, release_b_receive) = std::sync::mpsc::channel();
    let release_b_receive = Arc::new(Mutex::new(release_b_receive));
    let hook_roots = Arc::clone(&roots);
    store.register_before_publish(move |next| {
        hook_roots
            .lock()
            .expect("root observations mutex")
            .push(next.static_folder_path.clone());
        if next.static_folder_path == "root-b" {
            entered_b_send
                .send(())
                .expect("announce blocked root-b hook");
            release_b_receive
                .lock()
                .expect("release receiver mutex")
                .recv()
                .expect("release root-b hook");
        }
    });

    let update_b = {
        let store = Arc::clone(&store);
        std::thread::spawn(move || {
            store.update(|config| config.static_folder_path = "root-b".to_owned())
        })
    };
    entered_b_receive.recv().expect("root-b hook entered");
    let (started_c_send, started_c_receive) = std::sync::mpsc::channel();
    let update_c = {
        let store = Arc::clone(&store);
        std::thread::spawn(move || {
            started_c_send.send(()).expect("announce root-c update");
            store.update(|config| config.static_folder_path = "root-c".to_owned())
        })
    };
    started_c_receive.recv().expect("root-c update started");
    release_b_send.send(()).expect("release root-b hook");

    assert!(update_b.join().expect("join root-b update"));
    assert!(update_c.join().expect("join root-c update"));
    assert_eq!(
        roots.lock().expect("root observations mutex").as_slice(),
        &[
            "./static".to_owned(),
            "root-b".to_owned(),
            "root-c".to_owned(),
        ]
    );
    assert_eq!(store.current().static_folder_path, "root-c");
    store.shutdown().await.expect("shutdown config");
}

#[tokio::test]
async fn failed_flush_retains_dirty_state_for_shutdown_retry_and_cleans_temp() {
    let temp = tempfile::tempdir().expect("temporary config directory");
    let path = temp.path().join("vosu.toml");
    let replacer = Arc::new(RecordingReplace::default());
    let store = ConfigStore::initialize_with_options(&path, options(replacer.clone(), 60_000))
        .await
        .expect("initialize config");
    let original = std::fs::read(&path).expect("read original config");
    replacer.calls.store(0, Ordering::SeqCst);
    replacer.fail.store(true, Ordering::SeqCst);

    store.update(|config| config.server_port = 26_000.0);
    let error = store
        .flush()
        .await
        .expect_err("injected replacement must fail");
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert_eq!(replacer.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        std::fs::read(&path).expect("read retained config"),
        original
    );
    assert!(
        std::fs::read_dir(temp.path())
            .expect("list config directory")
            .all(|entry| !entry
                .expect("directory entry")
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp"))
    );

    store
        .shutdown()
        .await
        .expect("shutdown retries the failed save");
    assert_eq!(replacer.calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        ConfigDocument::parse(&std::fs::read_to_string(&path).expect("read retried config"))
            .expect("parse retried config")
            .config()
            .server_port,
        26_000.0
    );
}

#[tokio::test]
async fn debounced_save_failure_is_rescheduled_until_persisted() {
    let temp = tempfile::tempdir().expect("temporary config directory");
    let path = temp.path().join("vosu.toml");
    let replacer = Arc::new(RecordingReplace::default());
    let store = ConfigStore::initialize_with_options(&path, options(replacer.clone(), 50))
        .await
        .expect("initialize config");
    replacer.fail.store(true, Ordering::SeqCst);
    store.update(|config| config.server_port = 26_001.0);

    // No flush/shutdown may drive this save: a failed debounced write must
    // retry itself. Observe the durable file rather than worker scheduling.
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let document = ConfigDocument::parse(
                &std::fs::read_to_string(&path).expect("read persisted config"),
            )
            .expect("parse persisted config");
            if document.config().server_port == 26_001.0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("failed debounced save must retry without another update");
    store.shutdown().await.expect("shutdown config");
}

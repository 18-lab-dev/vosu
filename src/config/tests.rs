use super::*;
use std::io;
use std::path::Path;
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
        if self.fail.load(Ordering::SeqCst) {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "injected"));
        }
        crate::platform::atomic_replace(temp, target)
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

const DEFAULT_ENV: &str = "\
DEBUG_LOG=false
ENABLE_AUTOUPDATE=true
OPEN_DASHBOARD_ON_STARTUP=true

SHOW_MP_COMMANDS=false
CALCULATE_PP=true
READ_MANIA_SCROLL_SPEED=true

ENABLE_KEY_OVERLAY=true
ENABLE_INGAME_OVERLAY=false

POLL_RATE=150
PRECISE_DATA_POLL_RATE=10

INGAME_OVERLAY_KEYBIND=Control + Shift + Space
INGAME_OVERLAY_MAX_FPS=60

SERVER_IP=127.0.0.1
SERVER_PORT=24050
ALLOWED_IPS=127.0.0.1,localhost,absolute

STATIC_FOLDER_PATH=./static";

#[test]
fn defaults_and_serialization_match_source_bytes() {
    let store = ConfigDocument::parse(DEFAULT_ENV);
    assert_eq!(store.config(), &Config::default());
    assert_eq!(store.serialize().as_bytes(), DEFAULT_ENV.as_bytes());
    assert_eq!(
        DEFAULT_SCHEMA
            .items()
            .iter()
            .map(|item| item.binding)
            .collect::<Vec<_>>(),
        [
            "ENABLE_AUTOUPDATE",
            "OPEN_DASHBOARD_ON_STARTUP",
            "DEBUG_LOG",
            "CALCULATE_PP",
            "ENABLE_KEY_OVERLAY",
            "POLL_RATE",
            "PRECISE_DATA_POLL_RATE",
            "SHOW_MP_COMMANDS",
            "READ_MANIA_SCROLL_SPEED",
            "SERVER_IP",
            "SERVER_PORT",
            "STATIC_FOLDER_PATH",
            "ENABLE_INGAME_OVERLAY",
            "INGAME_OVERLAY_KEYBIND",
            "INGAME_OVERLAY_MAX_FPS",
            "ALLOWED_IPS",
        ]
    );
}

#[test]
fn parsing_matches_boolean_number_and_unknown_fallbacks() {
    let store = ConfigDocument::parse(
        "\
DEBUG_LOG=TRUE
ENABLE_AUTOUPDATE=1
OPEN_DASHBOARD_ON_STARTUP=true
SERVER_PORT=not-a-number
POLL_RATE=0x96
INGAME_OVERLAY_MAX_FPS=-12.5
STATIC_FOLDER_PATH=\"folder # retained\" # ignored comment
SERVER_IP: localhost
UNKNOWN_BINDING=true",
    );
    let config = store.config();

    assert!(!config.debug_log);
    assert!(!config.enable_auto_update);
    assert!(config.open_dashboard_on_startup);
    assert_eq!(config.server_port, 24_050.0);
    assert_eq!(config.poll_rate, 150.0);
    assert_eq!(config.ingame_overlay_max_fps, -12.5);
    assert_eq!(config.static_folder_path, "folder # retained");
    assert_eq!(config.server_ip, "localhost");
}

#[test]
fn only_polling_minima_are_clamped_and_warned() {
    let mut store = ConfigDocument::parse("POLL_RATE=-5\nPRECISE_DATA_POLL_RATE=0");
    let warnings = store.refresh(
        "\
POLL_RATE=-5
PRECISE_DATA_POLL_RATE=0
SERVER_PORT=-1
INGAME_OVERLAY_MAX_FPS=-2",
    );

    assert_eq!(store.config().poll_rate, 100.0);
    assert_eq!(store.config().precise_data_poll_rate, 1.0);
    assert_eq!(store.config().server_port, -1.0);
    assert_eq!(store.config().ingame_overlay_max_fps, -2.0);
    assert_eq!(
        warnings,
        [
            "Config %POLL_RATE% value %-5% is below the minimum %100%, clamping.",
            "Config %PRECISE_DATA_POLL_RATE% value %0% is below the minimum %1%, clamping.",
        ]
    );
}

#[test]
fn refresh_retains_missing_values_and_resets_invalid_numbers_to_defaults() {
    let mut store = ConfigDocument::parse("SERVER_IP=localhost\nSERVER_PORT=1234");
    store.refresh("DEBUG_LOG=true\nSERVER_PORT=Infinity");

    assert_eq!(store.config().server_ip, "localhost");
    assert_eq!(store.config().server_port, 24_050.0);
    assert!(store.config().debug_log);
}

#[tokio::test]
async fn lifecycle_migrates_with_precedence_and_removes_deprecated() {
    let temp = tempfile::tempdir().expect("temporary config directory");
    let path = temp.path().join("vosu.env");
    // `tosu.env` is accepted only as a one-time migration input.
    std::fs::write(
        temp.path().join("tosu.env"),
        "DEBUG_LOG=true\nSERVER_IP=old-host\nOLD_SECOND=two\nOLD_FIRST=one",
    )
    .expect("write legacy config");
    std::fs::write(&path, "DEBUG_LOG=false\nSERVER_PORT=25000").expect("write current config");

    let store = ConfigStore::initialize(&path)
        .await
        .expect("initialize config");
    assert!(!store.current().debug_log, "current file must win");
    assert_eq!(store.current().server_ip, "old-host", "legacy fills gaps");
    assert_eq!(store.current().server_port, 25_000.0);
    assert!(!temp.path().join("tosu.env").exists());
    let bytes = std::fs::read_to_string(&path).expect("read migrated config");
    assert!(!bytes.contains("OLD_FIRST"));
    assert!(!bytes.contains("OLD_SECOND"));
    assert_eq!(
        store.warnings()[0],
        "Removed deprecated configuration properties: %OLD_SECOND, OLD_FIRST%"
    );
    assert_eq!(
        store.warnings().last().map(String::as_str),
        Some("Configuration file successfully migrated.")
    );
    store.shutdown().await.expect("shutdown config");
}

#[tokio::test]
async fn lifecycle_creates_exact_defaults_with_same_directory_atomic_replace() {
    let temp = tempfile::tempdir().expect("temporary config directory");
    let path = temp.path().join("vosu.env");
    let replacer = Arc::new(RecordingReplace::default());
    let store = ConfigStore::initialize_with_options(&path, options(replacer.clone(), 20))
        .await
        .expect("initialize config");

    assert_eq!(
        std::fs::read(&path).expect("read defaults"),
        DEFAULT_ENV.as_bytes()
    );
    assert!(replacer.same_directory.load(Ordering::SeqCst));
    assert_eq!(replacer.calls.load(Ordering::SeqCst), 1);
    assert!(
        std::fs::read_dir(temp.path())
            .expect("list config directory")
            .all(|entry| !entry
                .expect("directory entry")
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp"))
    );
    store.shutdown().await.expect("shutdown config");
}

#[tokio::test]
async fn before_publish_transactions_serialize_concurrent_resource_swaps() {
    let temp = tempfile::tempdir().expect("temporary config directory");
    let path = temp.path().join("vosu.env");
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
    let path = temp.path().join("vosu.env");
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

    tokio::time::sleep(Duration::from_millis(180)).await;
    assert_eq!(replacer.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        ConfigDocument::parse(&std::fs::read_to_string(&path).expect("saved config"))
            .config()
            .server_port,
        24_053.0
    );
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
    let path = temp.path().join("vosu.env");
    let store = ConfigStore::initialize(&path)
        .await
        .expect("initialize config");
    let mut changes = store.subscribe();

    std::fs::write(&path, format!("{DEFAULT_ENV}\nSERVER_PORT=25001")).expect("external modify");
    let modified = wait_for_value(&mut changes, |config| config.server_port == 25_001.0).await;
    assert_eq!(modified.old.server_port, 24_050.0);
    assert_eq!(modified.new.server_port, 25_001.0);

    std::fs::remove_file(&path).expect("remove config");
    std::fs::write(&path, format!("{DEFAULT_ENV}\nSERVER_PORT=25002")).expect("external create");
    let created = wait_for_value(&mut changes, |config| config.server_port == 25_002.0).await;
    assert_eq!(created.old.server_port, 25_001.0);
    assert_eq!(created.new.server_port, 25_002.0);

    let replacement = temp.path().join("external.env");
    std::fs::write(&replacement, format!("{DEFAULT_ENV}\nSERVER_PORT=25003"))
        .expect("write rename replacement");
    std::fs::rename(&replacement, &path).expect("external rename");
    let renamed = wait_for_value(&mut changes, |config| config.server_port == 25_003.0).await;
    assert_eq!(renamed.old.server_port, 25_002.0);
    assert_eq!(renamed.new.server_port, 25_003.0);
    store.shutdown().await.expect("shutdown config");
}

#[tokio::test]
async fn before_publish_barrier_runs_before_snapshot_becomes_observable() {
    let temp = tempfile::tempdir().expect("temporary config directory");
    let path = temp.path().join("vosu.env");
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
    let path = temp.path().join("vosu.env");
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
async fn failed_atomic_replace_retains_old_file_and_cleans_temp() {
    let temp = tempfile::tempdir().expect("temporary config directory");
    let path = temp.path().join("vosu.env");
    let replacer = Arc::new(RecordingReplace::default());
    let store = ConfigStore::initialize_with_options(&path, options(replacer.clone(), 500))
        .await
        .expect("initialize config");
    let original = std::fs::read(&path).expect("read original config");
    replacer.fail.store(true, Ordering::SeqCst);

    store.update(|config| config.server_port = 26_000.0);
    let error = store
        .flush()
        .await
        .expect_err("injected replacement must fail");
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
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
    store.shutdown().await.expect("shutdown after failed save");
}

mod cards;
mod manifest;
mod settings;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use vosu_proptest::bounded;

use serde_json::{Value, json};

use super::*;
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[derive(Default)]
struct RecordingDownloader(Mutex<Vec<DownloadRequest>>);
impl CounterDownloader for RecordingDownloader {
    fn download_and_install(
        &self,
        request: DownloadRequest,
        destination: PathBuf,
        _cancel: tokio_util::sync::CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<(), CounterError>> + Send + 'static>> {
        lock(&self.0).push(request);
        Box::pin(async move {
            std::fs::create_dir_all(&destination)?;
            std::fs::write(destination.join("index.html"), "ok")?;
            Ok(())
        })
    }
}

fn fixture() -> (tempfile::TempDir, CounterService, Arc<RecordingDownloader>) {
    let root = tempfile::tempdir().unwrap();
    let static_root = root.path().join("static");
    std::fs::create_dir(&static_root).unwrap();
    let downloader = Arc::new(RecordingDownloader::default());
    let service = CounterService::new(
        static_root,
        root.path().join("settings"),
        downloader.clone(),
    );
    (root, service, downloader)
}

fn all_settings() -> Vec<CounterSetting> {
    [
        ("text", SettingType::Text, json!("hello")),
        ("color", SettingType::Color, json!("#fff")),
        ("number", SettingType::Number, json!(12)),
        ("checkbox", SettingType::Checkbox, json!(false)),
        ("button", SettingType::Button, json!("press")),
        ("options", SettingType::Options, json!("one")),
        ("commands", SettingType::Commands, json!(["a"])),
        ("textarea", SettingType::Textarea, json!("many")),
        ("password", SettingType::Password, json!("secret")),
        ("header", SettingType::Header, Value::Null),
    ]
    .into_iter()
    .map(|(id, kind, value)| CounterSetting {
        unique_id: id.into(),
        unique_check: None,
        kind,
        title: id.into(),
        options: None,
        description: String::new(),
        value,
    })
    .collect()
}

#[test]
fn oversized_metadata_is_rejected_before_parsing() {
    let (root, service, _) = fixture();
    let counter = root.path().join("static/counter");
    std::fs::create_dir(&counter).unwrap();
    std::fs::write(counter.join("metadata.txt"), vec![b'x'; 64 * 1024 + 1]).unwrap();
    // The folder is reported as broken; it never hides other overlays.
    let readable = root.path().join("static/readable");
    std::fs::create_dir(&readable).unwrap();
    std::fs::write(readable.join("index.html"), "").unwrap();
    let (counters, broken) = service.discover_all().unwrap();
    assert_eq!(counters.len(), 1);
    assert_eq!(counters[0].folder_name, "readable");
    assert_eq!(broken.len(), 1);
    assert_eq!(broken[0].folder_name, "counter");
    assert!(
        broken[0].error.starts_with("metadata.txt: "),
        "{}",
        broken[0].error
    );
    assert!(
        broken[0].error.contains("metadata file exceeds limit"),
        "{}",
        broken[0].error
    );
}

#[test]
fn discovers_metadata_html_order_and_immediate_ignore_scope() {
    let (root, service, _) = fixture();
    let static_root = root.path().join("static");
    let ignored = static_root.join("ignored");
    let nested_ignored = static_root.join("parent/deep");
    let metadata_counter = static_root.join("meta");
    let local = static_root.join("local");
    for path in [&ignored, &nested_ignored, &metadata_counter, &local] {
        std::fs::create_dir_all(path).unwrap();
    }
    std::fs::write(ignored.join("ignore.txt"), "").unwrap();
    std::fs::write(ignored.join("index.html"), "").unwrap();
    std::fs::write(nested_ignored.join("ignore.txt"), "").unwrap();
    std::fs::write(nested_ignored.join("index.html"), "").unwrap();
    std::fs::write(metadata_counter.join("metadata.txt"), "Name: Fancy ## comment\r\nAuthor: Alice\nVersion:\nResolution: 1920 x 1080 ## pixels\nAuthorLinks: https://a,https://b\nUnknown line").unwrap();
    std::fs::write(metadata_counter.join("index.html"), "").unwrap();
    std::fs::write(local.join("index.html"), "").unwrap();
    let counters = service.discover().unwrap();
    assert_eq!(counters.len(), 3);
    assert_eq!(counters[0].name, "Fancy");
    assert!(
        !counters
            .iter()
            .any(|counter| counter.folder_name == "ignored")
    );
    assert!(
        counters
            .iter()
            .any(|counter| counter.folder_name == "parent/deep")
    );
    let meta = counters
        .iter()
        .find(|counter| counter.name == "Fancy")
        .unwrap();
    assert_eq!(meta.author, "Alice");
    assert_eq!(meta.version, "1.0");
    assert_eq!(meta.resolution, vec![json!(1920), json!(1080)]);
    assert_eq!(meta.authorlinks, vec!["https://a", "https://b"]);
    assert_eq!(
        counters
            .iter()
            .filter(|counter| counter.folder_name == "meta")
            .count(),
        1
    );
}

#[test]
fn discovery_skips_counters_nested_under_node_modules() {
    let (root, service, _) = fixture();
    let overlay = root.path().join("static/overlay");
    let dependency = overlay.join("node_modules/dependency");
    std::fs::create_dir_all(&dependency).unwrap();
    std::fs::write(overlay.join("index.html"), "").unwrap();
    std::fs::write(dependency.join("index.html"), "").unwrap();

    let folder_names = service
        .discover()
        .unwrap()
        .into_iter()
        .map(|counter| counter.folder_name)
        .collect::<Vec<_>>();

    assert_eq!(folder_names, ["overlay"]);
}

#[cfg(unix)]
#[test]
fn discovery_follows_outside_directory_symlink_under_alias_name() {
    use std::os::unix::fs::symlink;

    let (root, service, _) = fixture();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("index.html"), "").unwrap();
    symlink(outside.path(), root.path().join("static/alias")).unwrap();

    let folder_names = service
        .discover()
        .unwrap()
        .into_iter()
        .map(|counter| counter.folder_name)
        .collect::<Vec<_>>();

    assert_eq!(folder_names, ["alias"]);
}

#[cfg(unix)]
#[test]
fn discovery_mounted_alias_skips_descendant_directory_symlink_that_escapes_target() {
    use std::os::unix::fs::symlink;

    let (root, service, _) = fixture();
    let alias_target = tempfile::tempdir().unwrap();
    let escaped = tempfile::tempdir().unwrap();
    std::fs::write(alias_target.path().join("index.html"), "").unwrap();
    std::fs::write(escaped.path().join("index.html"), "").unwrap();
    symlink(escaped.path(), alias_target.path().join("unservable")).unwrap();
    symlink(alias_target.path(), root.path().join("static/alias")).unwrap();

    let folder_names = service
        .discover()
        .unwrap()
        .into_iter()
        .map(|counter| counter.folder_name)
        .collect::<Vec<_>>();

    assert_eq!(folder_names, ["alias"]);
}

#[cfg(unix)]
#[test]
fn discovery_does_not_promote_directory_alias_nested_under_ordinary_counter() {
    use std::os::unix::fs::symlink;

    let (root, service, _) = fixture();
    let ordinary = root.path().join("static/ordinary");
    let nested_target = tempfile::tempdir().unwrap();
    std::fs::create_dir(&ordinary).unwrap();
    std::fs::write(ordinary.join("index.html"), "").unwrap();
    std::fs::write(nested_target.path().join("index.html"), "").unwrap();
    symlink(nested_target.path(), ordinary.join("nested-alias")).unwrap();

    let folder_names = service
        .discover()
        .unwrap()
        .into_iter()
        .map(|counter| counter.folder_name)
        .collect::<Vec<_>>();

    assert_eq!(folder_names, ["ordinary"]);
}

#[cfg(unix)]
#[test]
fn discovery_stops_top_level_directory_symlink_cycles() {
    use std::os::unix::fs::symlink;

    let (root, service, _) = fixture();
    let overlay = root.path().join("static/overlay");
    std::fs::create_dir(&overlay).unwrap();
    std::fs::write(overlay.join("index.html"), "").unwrap();
    symlink(root.path().join("static"), root.path().join("static/loop")).unwrap();

    let folder_names = service
        .discover()
        .unwrap()
        .into_iter()
        .map(|counter| counter.folder_name)
        .collect::<Vec<_>>();

    assert_eq!(folder_names, ["overlay"]);
}

#[cfg(unix)]
#[test]
fn discovery_skips_unresolvable_and_file_symlinks() {
    use std::os::unix::fs::symlink;

    let (root, service, _) = fixture();
    let static_root = root.path().join("static");
    let overlay = static_root.join("overlay");
    let linked_file = static_root.join("linked-file");
    std::fs::create_dir(&overlay).unwrap();
    std::fs::create_dir(&linked_file).unwrap();
    std::fs::write(overlay.join("index.html"), "").unwrap();
    std::fs::write(root.path().join("outside-index.html"), "").unwrap();
    symlink(
        root.path().join("outside-index.html"),
        linked_file.join("index.html"),
    )
    .unwrap();
    symlink(root.path().join("missing"), static_root.join("dangling")).unwrap();
    symlink("loop-b", static_root.join("loop-a")).unwrap();
    symlink("loop-a", static_root.join("loop-b")).unwrap();

    let folder_names = service
        .discover()
        .unwrap()
        .into_iter()
        .map(|counter| counter.folder_name)
        .collect::<Vec<_>>();

    assert_eq!(folder_names, ["overlay"]);
}

#[tokio::test]
async fn debounce_commits_only_latest_payload() {
    let (root, service, _) = fixture();
    std::fs::create_dir_all(root.path().join("static/counter")).unwrap();
    service
        .settings()
        .save_developer_settings("counter", all_settings())
        .unwrap();
    let first_service = service.clone();
    let first = tokio::spawn(async move {
        bounded!(first_service.save_user_settings_debounced(
            "counter".into(),
            vec![SettingInput {
                unique_id: "text".into(),
                value: json!("first"),
            }],
        ))
        .unwrap()
    });
    tokio::time::sleep(Duration::from_millis(25)).await;
    let second = bounded!(service.save_user_settings_debounced(
        "counter".into(),
        vec![SettingInput {
            unique_id: "text".into(),
            value: json!("second"),
        }],
    ))
    .unwrap();
    assert!(!bounded!(first).unwrap());
    assert!(second);
    assert_eq!(
        service.get_counter_values("counter").unwrap()["text"],
        "second"
    );
}

#[tokio::test]
async fn parses_raw_download_input_once_and_honors_update_lifecycle() {
    let (root, service, downloader) = fixture();
    let request = CounterService::parse_download_request(
        "https%3A%2F%2Fexample.com%2Fa%252Fb.zip%3Fx%3Da%2526b",
        Some("counter"),
        Some("false"),
    )
    .unwrap();
    assert_eq!(request.url, "https://example.com/a%2Fb.zip?x=a%26b");
    assert_eq!(request.name, "counter");
    bounded!(service.download(&request)).unwrap();
    assert!(bounded!(service.download(&request)).is_err());
    let update = DownloadRequest {
        update: true,
        ..request
    };
    bounded!(service.download(&update)).unwrap();
    assert_eq!(lock(&downloader.0).len(), 2);
    assert!(root.path().join("static/counter/index.html").exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        symlink(
            root.path().join("does-not-exist"),
            root.path().join("static/dangling"),
        )
        .unwrap();
        let dangling = DownloadRequest {
            url: "https://example.com/d.zip".into(),
            name: "dangling".into(),
            update: true,
        };
        assert!(matches!(
            bounded!(service.download(&dangling)),
            Err(CounterError::Symlink)
        ));
    }
}

#[test]
fn delete_unlinks_symlinks_and_rejects_traversal() {
    let (root, service, _) = fixture();
    let counter = root.path().join("static/counter");
    std::fs::create_dir(&counter).unwrap();
    assert!(service.delete("a/b").is_err());
    assert!(service.delete("../outside").is_err());
    service.delete("counter").unwrap();
    assert!(!counter.exists());

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let outside = root.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        symlink(&outside, root.path().join("static/link")).unwrap();
        service.delete("link").unwrap();
        assert!(std::fs::symlink_metadata(root.path().join("static/link")).is_err());
        assert!(outside.exists());
        let outside_file = root.path().join("outside-file");
        std::fs::write(&outside_file, "").unwrap();
        symlink(&outside_file, root.path().join("static/file-link")).unwrap();
        service.delete("file-link").unwrap();
        assert!(std::fs::symlink_metadata(root.path().join("static/file-link")).is_err());
        assert!(outside_file.exists());
        symlink(
            root.path().join("gone"),
            root.path().join("static/dangling"),
        )
        .unwrap();
        service.delete("dangling").unwrap();
        assert!(std::fs::symlink_metadata(root.path().join("static/dangling")).is_err());
    }
}

#[tokio::test]
async fn reloadable_service_switches_assets_counters_settings_and_mutations_atomically() {
    let root = tempfile::tempdir().unwrap();
    let root_a = root.path().join("static-a");
    let root_b = root.path().join("static-b");
    let settings_root = root.path().join("settings");
    std::fs::create_dir_all(root_a.join("alpha")).unwrap();
    std::fs::create_dir_all(root_b.join("beta")).unwrap();
    std::fs::write(root_a.join("alpha/index.html"), "asset-a").unwrap();
    std::fs::write(root_b.join("beta/index.html"), "asset-b").unwrap();
    std::fs::write(
        root_b.join("beta/settings.json"),
        serde_json::to_vec(&all_settings()).unwrap(),
    )
    .unwrap();
    let downloader = Arc::new(RecordingDownloader::default());
    let service = ReloadableCounterService::from_service(CounterService::new(
        root_a.clone(),
        settings_root.clone(),
        downloader.clone(),
    ));
    let asset = |path: &str| std::fs::read_to_string(service.current_root().join(path));

    assert_eq!(service.discover().unwrap()[0].folder_name, "alpha");
    let body = asset("alpha/index.html").unwrap();
    assert_eq!(body, "asset-a");

    service.reload_static_root(root_b.clone());
    let counters = service.discover().unwrap();
    assert_eq!(counters.len(), 1);
    assert_eq!(counters[0].folder_name, "beta");
    assert!(asset("alpha/index.html").is_err());
    let body = asset("beta/index.html").unwrap();
    assert_eq!(body, "asset-b");

    service
        .save_user_values(
            "beta",
            &[SettingInput {
                unique_id: "text".into(),
                value: json!("root-b"),
            }],
        )
        .unwrap();
    assert_eq!(
        service.get_counter_values("beta").unwrap()["text"],
        "root-b"
    );
    assert!(settings_root.join("beta.settings.values.toml").exists());
    // Opening folders is core's desktop integration (A7.1), not this host's.

    bounded!(service.download(&DownloadRequest {
        url: "https://example.com/new.zip".into(),
        name: "downloaded".into(),
        update: false,
    }))
    .unwrap();
    assert!(root_b.join("downloaded/index.html").exists());
    assert!(!root_a.join("downloaded").exists());
    service.delete("downloaded").unwrap();

    let missing = root.path().join("not-created");
    service.reload_static_root(missing.clone());
    assert_eq!(service.current_root(), missing);
    assert!(service.discover().is_err());
    service.reload_static_root(root_b);
    assert_eq!(service.discover().unwrap()[0].folder_name, "beta");
    bounded!(service.shutdown());
}

#[tokio::test]
async fn reloadable_service_keeps_one_debounce_generation_across_root_changes() {
    let root = tempfile::tempdir().unwrap();
    let root_a = root.path().join("a");
    let root_b = root.path().join("b");
    for static_root in [&root_a, &root_b] {
        std::fs::create_dir_all(static_root.join("counter")).unwrap();
        std::fs::write(
            static_root.join("counter/settings.json"),
            serde_json::to_vec(&all_settings()).unwrap(),
        )
        .unwrap();
    }
    let service = Arc::new(ReloadableCounterService::from_service(CounterService::new(
        root_a,
        root.path().join("settings"),
        Arc::new(RecordingDownloader::default()),
    )));
    let first_service = Arc::clone(&service);
    let first = tokio::spawn(async move {
        bounded!(first_service.save_user_settings_debounced(
            "counter".into(),
            vec![SettingInput {
                unique_id: "text".into(),
                value: json!("old"),
            }],
        ))
        .unwrap()
    });
    while bounded!(service.pending_save_generation("counter")) != Some(1) {
        tokio::task::yield_now().await;
    }
    service.reload_static_root(root_b);
    let second = bounded!(service.save_user_settings_debounced(
        "counter".into(),
        vec![SettingInput {
            unique_id: "text".into(),
            value: json!("new"),
        }],
    ))
    .unwrap();
    assert!(!bounded!(first).unwrap());
    assert!(second);
    assert_eq!(
        service.get_counter_values("counter").unwrap()["text"],
        "new"
    );
}

#[cfg(unix)]
#[test]
fn discovery_rejects_symlinked_settings_file() {
    use std::os::unix::fs::symlink;
    let (root, service, _) = fixture();
    let counter = root.path().join("static/counter");
    std::fs::create_dir(&counter).unwrap();
    std::fs::write(counter.join("index.html"), "").unwrap();
    let outside = root.path().join("outside-settings.json");
    std::fs::write(&outside, "[]").unwrap();
    symlink(outside, counter.join("settings.json")).unwrap();
    // Rejected, and listed as a broken overlay rather than failing the scan.
    assert!(service.discover().unwrap().is_empty());
    let (_, broken) = service.discover_all().unwrap();
    assert_eq!(broken.len(), 1);
    assert_eq!(broken[0].folder_name, "counter");
    assert_eq!(
        broken[0].error,
        format!("settings: {}", CounterError::Symlink)
    );
}

struct CancellingDownloader {
    started: Arc<tokio::sync::Semaphore>,
    finished: Arc<AtomicBool>,
}

impl CounterDownloader for CancellingDownloader {
    fn download_and_install(
        &self,
        _request: DownloadRequest,
        _destination: PathBuf,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<(), CounterError>> + Send + 'static>> {
        let started = self.started.clone();
        let finished = self.finished.clone();
        Box::pin(async move {
            started.add_permits(1);
            cancel.cancelled().await;
            finished.store(true, Ordering::Release);
            Ok(())
        })
    }
}

#[tokio::test]
async fn install_admission_rejects_when_four_jobs_are_active() {
    let root = tempfile::tempdir().unwrap();
    let static_root = root.path().join("static");
    std::fs::create_dir(&static_root).unwrap();
    let downloader = Arc::new(CancellingDownloader {
        started: Arc::new(tokio::sync::Semaphore::new(0)),
        finished: Arc::new(AtomicBool::new(false)),
    });
    let service = Arc::new(ReloadableCounterService::from_service(CounterService::new(
        static_root,
        root.path().join("settings"),
        downloader.clone(),
    )));
    let mut jobs = Vec::new();
    for index in 0..4 {
        let service = service.clone();
        jobs.push(tokio::spawn(async move {
            bounded!(service.download(&DownloadRequest {
                url: "https://example.com/a.zip".into(),
                name: format!("counter{index}"),
                update: false,
            }))
        }));
    }
    let permit = bounded!(downloader.started.acquire_many(4)).unwrap();
    permit.forget();
    let reloaded_root = root.path().join("replacement");
    std::fs::create_dir(&reloaded_root).unwrap();
    service.reload_static_root(reloaded_root);
    assert!(matches!(
        bounded!(service.download(&DownloadRequest {
            url: "https://example.com/fifth.zip".into(),
            name: "counter4".into(),
            update: false,
        })),
        Err(CounterError::InstallBusy)
    ));
    bounded!(service.shutdown());
    for job in jobs {
        bounded!(job).unwrap().unwrap();
    }
}

#[tokio::test]
async fn shutdown_cancels_and_joins_active_installs() {
    let root = tempfile::tempdir().unwrap();
    let static_root = root.path().join("static");
    std::fs::create_dir(&static_root).unwrap();
    let downloader = Arc::new(CancellingDownloader {
        started: Arc::new(tokio::sync::Semaphore::new(0)),
        finished: Arc::new(AtomicBool::new(false)),
    });
    let service = CounterService::new(
        static_root,
        root.path().join("settings"),
        downloader.clone(),
    );
    let request = DownloadRequest {
        url: "https://example.com/a.zip".into(),
        name: "counter".into(),
        update: false,
    };
    let downloading_service = service.clone();
    let download = tokio::spawn(async move { bounded!(downloading_service.download(&request)) });
    let permit = bounded!(downloader.started.acquire()).unwrap();
    permit.forget();
    let shutdown_a = service.clone();
    let shutdown_b = service.clone();
    bounded!(async { tokio::join!(shutdown_a.shutdown(), shutdown_b.shutdown()) });
    assert!(downloader.finished.load(Ordering::Acquire));
    assert!(bounded!(download).unwrap().is_ok());
    assert!(matches!(
        bounded!(service.download(&DownloadRequest {
            url: "https://example.com/b.zip".into(),
            name: "other".into(),
            update: false
        })),
        Err(CounterError::ShuttingDown)
    ));
}

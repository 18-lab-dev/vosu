use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use super::*;
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[derive(Default)]
struct RecordingOpener(Mutex<Vec<PathBuf>>);
impl CounterOpener for RecordingOpener {
    fn open(&self, path: &Path) -> io::Result<()> {
        lock(&self.0).push(path.to_owned());
        Ok(())
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

fn fixture() -> (
    tempfile::TempDir,
    CounterService,
    Arc<RecordingOpener>,
    Arc<RecordingDownloader>,
) {
    let root = tempfile::tempdir().unwrap();
    let static_root = root.path().join("static");
    std::fs::create_dir(&static_root).unwrap();
    let opener = Arc::new(RecordingOpener::default());
    let downloader = Arc::new(RecordingDownloader::default());
    let service = CounterService::new(
        static_root,
        root.path().join("settings"),
        opener.clone(),
        downloader.clone(),
    );
    (root, service, opener, downloader)
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
fn discovers_metadata_html_order_and_immediate_ignore_scope() {
    let (root, service, _, _) = fixture();
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
    let (root, service, _, _) = fixture();
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

#[test]
fn settings_union_migrates_coerces_and_roundtrips_compact_values() {
    let (root, service, _, _) = fixture();
    let folder = root.path().join("static/counter");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(
        folder.join("settings.json"),
        serde_json::to_vec(&all_settings()).unwrap(),
    )
    .unwrap();
    std::fs::write(folder.join("settings.values.json"), br#"{"text":"legacy"}"#).unwrap();
    let (_, migrated) = service.get_settings("counter").unwrap();
    assert_eq!(migrated["text"], "legacy");
    assert!(folder.join("settings.old-values.json").exists());
    let payload = all_settings()
        .into_iter()
        .map(|setting| SettingInput {
            unique_id: format!("{}!!", setting.unique_id),
            value: match setting.kind {
                SettingType::Number => json!("7.5"),
                SettingType::Checkbox => json!("true"),
                _ => setting.value,
            },
        })
        .collect::<Vec<_>>();
    service
        .settings()
        .save_user_values("counter", &payload)
        .unwrap();
    let values = service.get_counter_values("counter").unwrap();
    assert_eq!(values["number"], json!(7.5));
    assert_eq!(values["checkbox"], true);
    assert!(!values.contains_key("header"));
    for id in [
        "text", "color", "number", "checkbox", "button", "options", "commands", "textarea",
        "password",
    ] {
        assert!(values.contains_key(id));
    }
    let saved: Value =
        serde_json::from_slice(&std::fs::read(service.settings().values_path("counter")).unwrap())
            .unwrap();
    assert!(saved.is_object());
}

#[test]
fn developer_update_requires_yes_query() {
    let (root, service, _, _) = fixture();
    std::fs::create_dir_all(root.path().join("static/counter")).unwrap();
    assert!(
        service
            .save_developer_settings("counter", all_settings(), Some("no"))
            .is_err()
    );
    let saved = service
        .save_developer_settings("counter", all_settings(), Some("yes"))
        .unwrap();
    assert_eq!(saved.len(), 10);
}

#[tokio::test]
async fn debounce_commits_only_latest_payload() {
    let (root, service, _, _) = fixture();
    std::fs::create_dir_all(root.path().join("static/counter")).unwrap();
    service
        .settings()
        .save_developer_settings("counter", all_settings())
        .unwrap();
    let first_service = service.clone();
    let first = tokio::spawn(async move {
        first_service
            .save_user_settings_debounced(
                "counter".into(),
                vec![SettingInput {
                    unique_id: "text".into(),
                    value: json!("first"),
                }],
            )
            .await
            .unwrap()
    });
    tokio::time::sleep(Duration::from_millis(25)).await;
    let second = service
        .save_user_settings_debounced(
            "counter".into(),
            vec![SettingInput {
                unique_id: "text".into(),
                value: json!("second"),
            }],
        )
        .await
        .unwrap();
    assert!(!first.await.unwrap());
    assert!(second);
    assert_eq!(
        service.get_counter_values("counter").unwrap()["text"],
        "second"
    );
}

#[tokio::test]
async fn parses_raw_download_input_once_and_honors_update_lifecycle() {
    let (root, service, _, downloader) = fixture();
    let request = CounterService::parse_download_request(
        "https%3A%2F%2Fexample.com%2Fa%252Fb.zip%3Fx%3Da%2526b",
        Some("counter"),
        Some("false"),
    )
    .unwrap();
    assert_eq!(request.url, "https://example.com/a%2Fb.zip?x=a%26b");
    assert_eq!(request.name, "counter");
    service.download(&request).await.unwrap();
    assert!(service.download(&request).await.is_err());
    let update = DownloadRequest {
        update: true,
        ..request
    };
    service.download(&update).await.unwrap();
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
            service.download(&dangling).await,
            Err(CounterError::Symlink)
        ));
    }
}

#[test]
fn open_delete_reject_traversal_and_symlink_escape() {
    let (root, service, opener, _) = fixture();
    let counter = root.path().join("static/counter");
    std::fs::create_dir(&counter).unwrap();
    assert!(service.open("../outside").is_err());
    assert!(service.delete("a/b").is_err());
    service.open("counter").unwrap();
    assert_eq!(lock(&opener.0).as_slice(), std::slice::from_ref(&counter));
    service.delete("counter").unwrap();
    assert!(!counter.exists());

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let outside = root.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        symlink(&outside, root.path().join("static/link")).unwrap();
        assert!(matches!(service.open("link"), Err(CounterError::Symlink)));
        assert!(matches!(service.delete("link"), Err(CounterError::Symlink)));
        assert!(outside.exists());
        symlink(
            root.path().join("gone"),
            root.path().join("static/dangling"),
        )
        .unwrap();
        assert!(matches!(
            service.open("dangling"),
            Err(CounterError::Symlink)
        ));
        assert!(matches!(
            service.delete("dangling"),
            Err(CounterError::Symlink)
        ));
    }
}

#[tokio::test]
async fn reloadable_service_switches_assets_counters_settings_and_mutations_atomically() {
    use std::io::Read as _;

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
    let opener = Arc::new(RecordingOpener::default());
    let downloader = Arc::new(RecordingDownloader::default());
    let service = ReloadableCounterService::from_service(CounterService::new(
        root_a.clone(),
        settings_root.clone(),
        opener.clone(),
        downloader.clone(),
    ));

    assert_eq!(service.discover().unwrap()[0].folder_name, "alpha");
    let crate::server::files::ResourceOutcome::File(mut asset_a) =
        crate::server::files::resolve_resource(
            Some(&service.current_root()),
            "alpha/index.html",
            "/alpha/index.html",
            crate::server::files::MissingKind::NotFound,
        )
        .unwrap()
    else {
        panic!("root A asset was not resolved");
    };
    let mut body = String::new();
    asset_a.file.read_to_string(&mut body).unwrap();
    assert_eq!(body, "asset-a");

    service.reload_static_root(root_b.clone());
    let counters = service.discover().unwrap();
    assert_eq!(counters.len(), 1);
    assert_eq!(counters[0].folder_name, "beta");
    assert!(matches!(
        crate::server::files::resolve_resource(
            Some(&service.current_root()),
            "alpha/index.html",
            "/alpha/index.html",
            crate::server::files::MissingKind::NotFound,
        ),
        Ok(crate::server::files::ResourceOutcome::Missing(
            crate::server::files::MissingKind::NotFound
        ))
    ));
    let crate::server::files::ResourceOutcome::File(mut asset_b) =
        crate::server::files::resolve_resource(
            Some(&service.current_root()),
            "beta/index.html",
            "/beta/index.html",
            crate::server::files::MissingKind::NotFound,
        )
        .unwrap()
    else {
        panic!("root B asset was not resolved");
    };
    body.clear();
    asset_b.file.read_to_string(&mut body).unwrap();
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
    assert!(settings_root.join("beta.values.json").exists());
    service.open("beta").unwrap();
    assert_eq!(lock(&opener.0).as_slice(), &[root_b.join("beta")]);

    service
        .download(&DownloadRequest {
            url: "https://example.com/new.zip".into(),
            name: "downloaded".into(),
            update: false,
        })
        .await
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
    service.shutdown().await;
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
        Arc::new(RecordingOpener::default()),
        Arc::new(RecordingDownloader::default()),
    )));
    let first_service = Arc::clone(&service);
    let first = tokio::spawn(async move {
        first_service
            .save_user_settings_debounced(
                "counter".into(),
                vec![SettingInput {
                    unique_id: "text".into(),
                    value: json!("old"),
                }],
            )
            .await
            .unwrap()
    });
    while service.pending_save_generation("counter").await != Some(1) {
        tokio::task::yield_now().await;
    }
    service.reload_static_root(root_b);
    let second = service
        .save_user_settings_debounced(
            "counter".into(),
            vec![SettingInput {
                unique_id: "text".into(),
                value: json!("new"),
            }],
        )
        .await
        .unwrap();
    assert!(!first.await.unwrap());
    assert!(second);
    assert_eq!(
        service.get_counter_values("counter").unwrap()["text"],
        "new"
    );
}

#[test]
fn settings_reject_missing_and_symlink_counter_directories_without_creating_files() {
    let (root, service, _, _) = fixture();
    assert!(matches!(
        service.get_settings("missing"),
        Err(CounterError::Settings(SettingsError::MissingCounter))
    ));
    assert!(!root.path().join("static/missing").exists());
    assert!(!service.settings().values_path("missing").exists());

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let outside = root.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("settings.json"), "[]").unwrap();
        symlink(&outside, root.path().join("static/linked")).unwrap();
        assert!(matches!(
            service.get_settings("linked"),
            Err(CounterError::Settings(SettingsError::UnsafeCounterPath))
        ));
        assert!(!service.settings().values_path("linked").exists());
        let ordinary = root.path().join("static/ordinary");
        std::fs::create_dir(&ordinary).unwrap();
        symlink(
            outside.join("settings.json"),
            ordinary.join("settings.json"),
        )
        .unwrap();
        assert!(matches!(
            service.get_settings("ordinary"),
            Err(CounterError::Settings(SettingsError::UnsafeSettingsFile))
        ));
    }
}

#[cfg(unix)]
#[test]
fn discovery_rejects_symlinked_settings_file() {
    use std::os::unix::fs::symlink;
    let (root, service, _, _) = fixture();
    let counter = root.path().join("static/counter");
    std::fs::create_dir(&counter).unwrap();
    std::fs::write(counter.join("index.html"), "").unwrap();
    let outside = root.path().join("outside-settings.json");
    std::fs::write(&outside, "[]").unwrap();
    symlink(outside, counter.join("settings.json")).unwrap();
    assert!(matches!(service.discover(), Err(CounterError::Symlink)));
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
        Arc::new(RecordingOpener::default()),
        downloader.clone(),
    );
    let request = DownloadRequest {
        url: "https://example.com/a.zip".into(),
        name: "counter".into(),
        update: false,
    };
    let downloading_service = service.clone();
    let download = tokio::spawn(async move { downloading_service.download(&request).await });
    let permit = downloader.started.acquire().await.unwrap();
    permit.forget();
    let shutdown_a = service.clone();
    let shutdown_b = service.clone();
    tokio::join!(shutdown_a.shutdown(), shutdown_b.shutdown());
    assert!(downloader.finished.load(Ordering::Acquire));
    assert!(download.await.unwrap().is_ok());
    assert!(matches!(
        service
            .download(&DownloadRequest {
                url: "https://example.com/b.zip".into(),
                name: "other".into(),
                update: false
            })
            .await,
        Err(CounterError::ShuttingDown)
    ));
}

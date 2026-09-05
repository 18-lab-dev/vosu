use super::document::ConfigDocument;
use super::io::{ConfigStoreOptions, config_parent, hash_bytes, join_error, notify_error};
use super::migration::initialize_file;
use super::reload::{run_before_publish, run_store};
use super::schema::Config;
use arc_swap::ArcSwap;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio::task::JoinHandle;

/// An immutable configuration transition published after a successful update.
#[derive(Clone, Debug)]
pub struct ConfigChanged {
    pub old: Arc<Config>,
    pub new: Arc<Config>,
}

pub(super) type BeforePublishHook = Arc<dyn Fn(&Config) + Send + Sync>;

/// Live, atomically published configuration backed by `vosu.toml`.
pub struct ConfigStore {
    path: PathBuf,
    current: Arc<ArcSwap<Config>>,
    changed: broadcast::Sender<ConfigChanged>,
    snapshots: watch::Sender<Arc<Config>>,
    commands: mpsc::UnboundedSender<StoreCommand>,
    worker: Mutex<Option<JoinHandle<io::Result<()>>>>,
    before_publish: Arc<RwLock<Vec<BeforePublishHook>>>,
    publish_lock: Arc<Mutex<()>>,
    warnings: Vec<String>,
}

pub(super) enum StoreCommand {
    Save,
    Flush(oneshot::Sender<io::Result<()>>),
    Shutdown(oneshot::Sender<io::Result<()>>),
}
fn absolute_config_path(path: PathBuf) -> io::Result<PathBuf> {
    let current_dir = std::env::current_dir()?;
    Ok(absolute_config_path_from(path, &current_dir))
}

pub(super) fn absolute_config_path_from(path: PathBuf, current_dir: &Path) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        current_dir.join(path)
    }
}

impl ConfigStore {
    /// Migrates, loads, and begins watching the supplied `vosu.toml` path.
    pub async fn initialize(path: impl Into<PathBuf>) -> io::Result<Self> {
        Self::initialize_with_options(path, ConfigStoreOptions::default()).await
    }

    pub async fn initialize_with_options(
        path: impl Into<PathBuf>,
        options: ConfigStoreOptions,
    ) -> io::Result<Self> {
        let path = absolute_config_path(path.into())?;
        let initialize_path = path.clone();
        let initialize_replace = options.atomic_replace.clone();
        let initial = tokio::task::spawn_blocking(move || {
            initialize_file(&initialize_path, initialize_replace.as_ref())
        })
        .await
        .map_err(join_error)??;

        let current = Arc::new(ArcSwap::from_pointee(initial.config));
        let initial_arc = current.load_full();
        let (changed, _) = broadcast::channel(32);
        let (snapshots, _) = watch::channel(initial_arc);
        let (commands, command_rx) = mpsc::unbounded_channel();
        let (watch_events, watch_rx) = mpsc::unbounded_channel();
        let before_publish = Arc::new(RwLock::new(Vec::new()));
        let publish_lock = Arc::new(Mutex::new(()));
        let watched_path = path.clone();
        let mut watcher = RecommendedWatcher::new(
            move |event: notify::Result<notify::Event>| {
                let Ok(event) = event else {
                    return;
                };
                if !matches!(event.kind, EventKind::Create(_) | EventKind::Modify(_)) {
                    return;
                }
                if event.paths.is_empty() || event.paths.iter().any(|path| path == &watched_path) {
                    let _ = watch_events.send(());
                }
            },
            notify::Config::default(),
        )
        .map_err(notify_error)?;
        let parent = config_parent(&path);
        watcher
            .watch(parent, RecursiveMode::NonRecursive)
            .map_err(notify_error)?;

        let worker_path = path.clone();
        let worker_current = current.clone();
        let worker_changed = changed.clone();
        let worker_snapshots = snapshots.clone();
        let worker_before_publish = Arc::clone(&before_publish);
        let worker_publish_lock = Arc::clone(&publish_lock);
        // The single store owner preserves persistence/publication ordering, but
        // filesystem and synchronous reload hooks must not occupy a Tokio worker.
        let runtime = tokio::runtime::Handle::current();
        let worker = tokio::task::spawn_blocking(move || {
            runtime.block_on(run_store(
                worker_path,
                worker_current,
                worker_changed,
                worker_snapshots,
                command_rx,
                watch_rx,
                watcher,
                worker_before_publish,
                worker_publish_lock,
                options,
                hash_bytes(&initial.bytes),
            ))
        });

        Ok(Self {
            path,
            current,
            changed,
            snapshots,
            commands,
            worker: Mutex::new(Some(worker)),
            before_publish,
            warnings: initial.warnings,
            publish_lock,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn current(&self) -> Arc<Config> {
        self.current.load_full()
    }

    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ConfigChanged> {
        self.changed.subscribe()
    }

    pub fn subscribe_current(&self) -> watch::Receiver<Arc<Config>> {
        self.snapshots.subscribe()
    }

    /// Registers synchronous work that must complete before a new snapshot is
    /// made observable. Runtime resource generations use this barrier to swap
    /// their backing state before readers can observe the corresponding config.
    pub fn register_before_publish(&self, hook: impl Fn(&Config) + Send + Sync + 'static) {
        let _publication = self
            .publish_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let hook: BeforePublishHook = Arc::new(hook);
        hook(&self.current.load_full());
        self.before_publish
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(hook);
    }

    /// Publishes an in-memory update immediately and schedules one debounced save.
    pub fn update(&self, change: impl FnOnce(&mut Config)) -> bool {
        let _publication = self
            .publish_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let old = self.current.load_full();
        let mut next = (*old).clone();
        change(&mut next);
        let mut document = ConfigDocument::new(next);
        document.validate();
        let next = Arc::new(document.into_config());
        if *old == *next {
            return false;
        }
        run_before_publish(&self.before_publish, &next);
        self.publish(old, next);
        let _ = self.commands.send(StoreCommand::Save);
        true
    }

    pub async fn flush(&self) -> io::Result<()> {
        let (send, receive) = oneshot::channel();
        self.commands
            .send(StoreCommand::Flush(send))
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "config worker stopped"))?;
        receive
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "config worker stopped"))?
    }

    pub async fn shutdown(&self) -> io::Result<()> {
        let (send, receive) = oneshot::channel();
        self.commands
            .send(StoreCommand::Shutdown(send))
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "config worker stopped"))?;
        let result = receive
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "config worker stopped"))?;
        let worker = self
            .worker
            .lock()
            .expect("config worker mutex poisoned")
            .take();
        if let Some(worker) = worker {
            worker.await.map_err(join_error)??;
        }
        result
    }

    fn publish(&self, old: Arc<Config>, new: Arc<Config>) {
        self.current.store(new.clone());
        self.snapshots.send_replace(new.clone());
        let _ = self.changed.send(ConfigChanged { old, new });
    }
}

use super::document::ConfigDocument;
use super::io::{ConfigStoreOptions, hash_bytes, persist_current};
use super::schema::Config;
use super::store::{BeforePublishHook, ConfigChanged, StoreCommand};
use arc_swap::ArcSwap;
use notify::RecommendedWatcher;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use tokio::sync::{broadcast, mpsc, watch};

/// Grouped inputs for [`run_store`]; keeps the worker's startup state
/// explicit without a long positional parameter list.
pub(super) struct RunStoreArgs {
    pub(super) path: PathBuf,
    pub(super) current: Arc<ArcSwap<Config>>,
    pub(super) changed: broadcast::Sender<ConfigChanged>,
    pub(super) snapshots: watch::Sender<Arc<Config>>,
    pub(super) commands: mpsc::UnboundedReceiver<StoreCommand>,
    pub(super) watch_events: mpsc::Receiver<()>,
    pub(super) watcher: RecommendedWatcher,
    pub(super) before_publish: Arc<RwLock<Vec<BeforePublishHook>>>,
    pub(super) publish_lock: Arc<Mutex<()>>,
    pub(super) options: ConfigStoreOptions,
    pub(super) current_hash: [u8; 32],
}

pub(super) async fn run_store(args: RunStoreArgs) -> io::Result<()> {
    let RunStoreArgs {
        path,
        current,
        changed,
        snapshots,
        mut commands,
        mut watch_events,
        watcher: _watcher,
        before_publish,
        publish_lock,
        options,
        mut current_hash,
    } = args;
    let mut save_deadline = None;
    loop {
        tokio::select! {
            command = commands.recv() => {
                match command {
                    Some(StoreCommand::Save) => {
                        save_deadline = Some(tokio::time::Instant::now() + options.debounce);
                    }
                    Some(StoreCommand::Flush(reply)) => {
                        let result = persist_current(
                            &path,
                            &current,
                            options.atomic_replace.as_ref(),
                            &mut current_hash,
                        );
                        save_deadline = if result.is_err() {
                            Some(tokio::time::Instant::now() + options.debounce)
                        } else {
                            None
                        };
                        let _ = reply.send(result);
                    }
                    Some(StoreCommand::Shutdown(reply)) => {
                        let result = if save_deadline.take().is_some() {
                            persist_current(
                                &path,
                                &current,
                                options.atomic_replace.as_ref(),
                                &mut current_hash,
                            )
                        } else {
                            Ok(())
                        };
                        let return_result = match &result {
                            Ok(()) => Ok(()),
                            Err(error) => Err(io::Error::new(error.kind(), error.to_string())),
                        };
                        let _ = reply.send(result);
                        return return_result;
                    }
                    None => return Ok(()),
                }
            }
            event = watch_events.recv() => {
                if event.is_none() {
                    continue;
                }
                let _publication = publish_lock
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                // Retries a transient missing-name window (see
                // `vosu_sys::platform::atomic_replace` docs) briefly rather than
                // silently dropping this reload on a load-dependent Windows race.
                // Blocking (not `read_published_async`): this runs inside
                // `spawn_blocking` (see `ConfigStore::initialize_with_options`),
                // and a std `Mutex` guard is held across the call.
                let Ok(bytes) = vosu_sys::platform::read_published(&path) else {
                    continue;
                };
                let hash = hash_bytes(&bytes);
                if hash == current_hash {
                    continue;
                }
                let Ok(content) = std::str::from_utf8(&bytes) else {
                    continue;
                };
                let old = current.load_full();
                let mut document = ConfigDocument::new((*old).clone());
                if document.refresh(content).is_err() {
                    continue;
                }
                let new = Arc::new(document.into_config());
                current_hash = hash;
                if *old != *new {
                    run_before_publish(&before_publish, &new);
                    current.store(new.clone());
                    snapshots.send_replace(new.clone());
                    let _ = changed.send(ConfigChanged { old, new });
                }
            }
            () = wait_for_deadline(save_deadline), if save_deadline.is_some() => {
                let result = persist_current(
                    &path,
                    &current,
                    options.atomic_replace.as_ref(),
                    &mut current_hash,
                );
                save_deadline = if result.is_err() {
                    Some(tokio::time::Instant::now() + options.debounce)
                } else {
                    None
                };
            }
        }
    }
}

pub(super) fn run_before_publish(hooks: &RwLock<Vec<BeforePublishHook>>, next: &Config) {
    let hooks = hooks
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for hook in hooks.iter() {
        hook(next);
    }
}

async fn wait_for_deadline(deadline: Option<tokio::time::Instant>) {
    if let Some(deadline) = deadline {
        tokio::time::sleep_until(deadline).await;
    }
}

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

#[allow(clippy::too_many_arguments)]
pub(super) async fn run_store(
    path: PathBuf,
    current: Arc<ArcSwap<Config>>,
    changed: broadcast::Sender<ConfigChanged>,
    snapshots: watch::Sender<Arc<Config>>,
    mut commands: mpsc::UnboundedReceiver<StoreCommand>,
    mut watch_events: mpsc::UnboundedReceiver<()>,
    _watcher: RecommendedWatcher,
    before_publish: Arc<RwLock<Vec<BeforePublishHook>>>,
    publish_lock: Arc<Mutex<()>>,
    options: ConfigStoreOptions,
    mut current_hash: [u8; 32],
) -> io::Result<()> {
    let mut save_deadline = None;
    loop {
        tokio::select! {
            command = commands.recv() => {
                match command {
                    Some(StoreCommand::Save) => {
                        save_deadline = Some(tokio::time::Instant::now() + options.debounce);
                    }
                    Some(StoreCommand::Flush(reply)) => {
                        save_deadline = None;
                        let result = persist_current(
                            &path,
                            &current,
                            options.atomic_replace.as_ref(),
                            &mut current_hash,
                        );
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
                let Ok(bytes) = std::fs::read(&path) else {
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
                document.refresh(content);
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
                save_deadline = None;
                let _ = persist_current(
                    &path,
                    &current,
                    options.atomic_replace.as_ref(),
                    &mut current_hash,
                );
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

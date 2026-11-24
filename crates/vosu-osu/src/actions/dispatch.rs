//! Bounded owned import workers. No locks are held across host callbacks or I/O.
use super::import::{ArchiveLimits, ImportError, ImportResult, StableStage, ValidatedArchive};
use crate::session::OsuSession;
use serde::Deserialize;
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{Arc, Mutex},
    thread::JoinHandle,
};
use tokio_util::sync::CancellationToken;
use vosu_osu_model::enums::ClientType;
use vosu_plugin_api::{
    ActionInvoke, ActionProgress, ActionResult, ActionStatus, CborValue, HostServices, Message,
    PluginError, vosu_status,
};
pub const IMPORT_ACTION: &str = "vosu.osu.beatmap.import";
const MAX_IMPORTS: usize = 8;
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ImportArgs {
    source_path: PathBuf,
}
struct Worker {
    cancel: CancellationToken,
    join: JoinHandle<()>,
}
#[derive(Default)]
pub(crate) struct ImportActions {
    workers: Mutex<HashMap<u64, Worker>>,
    // Lock order: workers -> early_cancels. Concurrent dispatch may deliver
    // cancellation before invocation; retain at most one ABI caller's window.
    early_cancels: Mutex<HashSet<u64>>,
}
impl ImportActions {
    pub fn invoke<H: HostServices + Send + Sync + 'static>(
        &self,
        host: &Arc<H>,
        runtime: tokio::runtime::Handle,
        action: ActionInvoke,
        session: Option<&Arc<OsuSession>>,
    ) -> Result<(), PluginError> {
        let args = action.args.deserialized::<ImportArgs>();
        let failure = if action.action != IMPORT_ACTION {
            Some(("unknown_action", "unknown osu action"))
        } else if args.is_err() {
            Some(("invalid_arguments", "invalid beatmap import arguments"))
        } else if session.is_none() {
            Some(("target_gone", "selected client unavailable"))
        } else {
            None
        };
        if let Some((code, message)) = failure {
            host.send(
                &Message::ActionResult(error(action.invocation, code, message)),
                0,
            );
            return Ok(());
        }
        let session = session.expect("validated session");
        let cancel = session.lifecycle_token();
        let selected = Arc::downgrade(session);
        let songs = session
            .publication
            .get()
            .and_then(|p| p.bus.resources.get())
            .and_then(|p| p.import_songs_root());
        let lazer = session.import_context.get().cloned();
        let client = session.client;
        let invocation = action.invocation;
        let source = args.expect("validated args").source_path;
        let host = Arc::downgrade(host);
        let mut workers = self.workers.lock().unwrap_or_else(|e| e.into_inner());
        // Finished joins cannot call back; join outside the registry lock anyway.
        let finished = workers
            .extract_if(|_, w| w.join.is_finished())
            .map(|(_, w)| w)
            .collect::<Vec<_>>();
        drop(workers);
        for worker in finished {
            let _ = worker.join.join();
        }
        let mut workers = self.workers.lock().unwrap_or_else(|e| e.into_inner());
        if self
            .early_cancels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&invocation)
        {
            cancel.cancel();
        }
        if workers.contains_key(&invocation) {
            return Err(vosu_status::VOSU_ERR_INVALID_ARGUMENT.into());
        }
        if workers.len() >= MAX_IMPORTS {
            drop(workers);
            if let Some(host) = host.upgrade() {
                host.send(
                    &Message::ActionResult(error(
                        invocation,
                        "busy",
                        "beatmap import capacity reached",
                    )),
                    0,
                );
            }
            return Ok(());
        }
        let token = cancel.clone();
        let join = std::thread::Builder::new()
            .name("osu-import".into())
            .spawn(move || {
                let progress = |fraction, message: &str| {
                    if let Some(host) = host.upgrade() {
                        host.send(
                            &Message::ActionProgress(ActionProgress {
                                invocation,
                                fraction: Some(fraction),
                                message: Some(message.into()),
                            }),
                            0,
                        );
                    }
                };
                let result = (|| {
                    progress(0.0, "Validating beatmap archive");
                    let mut archive =
                        ValidatedArchive::open(&source, ArchiveLimits::default(), &token)?;
                    progress(0.5, "Preparing selected client handoff");
                    let live = || {
                        selected
                            .upgrade()
                            .filter(|s| !s.lifecycle_token().is_cancelled())
                            .ok_or(ImportError::Cancelled)
                    };
                    match client {
                        ClientType::Stable => {
                            let stage = StableStage::prepare(
                                &mut archive,
                                songs.as_deref().ok_or(ImportError::InvalidTarget)?,
                                &token,
                            )?;
                            live()?;
                            progress(
                                0.9,
                                "Queuing archive for osu! stable; refresh remains game-owned",
                            );
                            stage.commit(&token)
                        }
                        ClientType::Lazer => {
                            let (attached, context) = lazer.ok_or(ImportError::InvalidTarget)?;
                            let target = super::import::LazerTarget::from_attached(
                                &attached,
                                context.cwd,
                                context.environment,
                            )?;
                            live()?;
                            progress(0.9, "Submitting archive to selected osu! lazer client");
                            runtime.block_on(target.submit(archive, &token))
                        }
                    }
                })();
                if let Some(host) = host.upgrade() {
                    host.send(&Message::ActionResult(terminal(invocation, result)), 0);
                }
            })
            .map_err(|_| PluginError::from(vosu_status::VOSU_ERR_NOT_READY))?;
        workers.insert(invocation, Worker { cancel, join });
        Ok(())
    }
    pub fn cancel(&self, invocation: u64) -> Result<(), PluginError> {
        let workers = self.workers.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(worker) = workers.get(&invocation) {
            worker.cancel.cancel();
        } else {
            let mut pending = self.early_cancels.lock().unwrap_or_else(|e| e.into_inner());
            if !pending.contains(&invocation) && pending.len() >= 64 {
                return Err(vosu_status::VOSU_ERR_NOT_READY.into());
            }
            pending.insert(invocation);
        }
        Ok(())
    }
    #[cfg(test)]
    pub fn finished(&self) -> bool {
        self.workers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .all(|worker| worker.join.is_finished())
    }
    pub fn shutdown(&self) {
        let workers = std::mem::take(&mut *self.workers.lock().unwrap_or_else(|e| e.into_inner()));
        for worker in workers.values() {
            worker.cancel.cancel();
        }
        for worker in workers.into_values() {
            // A re-entrant host callback may release the plugin on this worker.
            // Returning from that callback is its final operation; never self-join.
            if worker.join.thread().id() != std::thread::current().id() {
                let _ = worker.join.join();
            }
        }
    }
}
fn error(invocation: u64, code: &str, message: &str) -> ActionResult {
    ActionResult {
        invocation,
        status: ActionStatus::Error,
        value: None,
        code: Some(code.into()),
        message: Some(message.into()),
    }
}
fn terminal(invocation: u64, result: Result<ImportResult, ImportError>) -> ActionResult {
    match result {
        Ok(result) => ActionResult {
            invocation,
            status: ActionStatus::Ok,
            value: Some(CborValue::serialized(&result).expect("static import result encodes")),
            code: None,
            message: None,
        },
        Err(reason) => error(
            invocation,
            match reason {
                ImportError::Cancelled => "cancelled",
                ImportError::InvalidSource | ImportError::InvalidArchive => "invalid_archive",
                ImportError::LimitExceeded => "limit_exceeded",
                ImportError::InvalidTarget => "target_gone",
                ImportError::DestinationExists => "destination_exists",
                ImportError::Io | ImportError::Handoff => "import_failed",
            },
            &reason.to_string(),
        ),
    }
}

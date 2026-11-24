//! osu!-owned resource preparation and lookup. O2 connects these cold selection
//! changes to publication and ABI dispatch; no capture-tick hooks live here.
mod identity;
pub mod legacy_skin;
pub(crate) mod publication;
mod resolve;
mod roots;
mod set;
mod skin;

pub use resolve::{AUDIO, BACKGROUND, CHART, PreparedResources, SET, SKIN};
pub use roots::RootPaths;
pub use set::TreeSource;

/// Paths are decoded, root-relative facts from the reader. Logical names are
/// separate from physical paths (notably lazer content-store hashes).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    pub roots: RootPaths,
    pub lazer: bool,
    pub chart: Option<String>,
    pub audio: Option<String>,
    pub background: Option<String>,
    pub chart_name: Option<String>,
    pub audio_name: Option<String>,
    pub background_name: Option<String>,
    pub set: Option<TreeSource>,
    pub skin: Option<TreeSource>,
}

type Work = (Selection, tokio::sync::oneshot::Sender<PreparedResources>);

/// One owned worker and at most one queued cold change. No locks or callbacks
/// cross preparation; the filesystem work runs exclusively on blocking workers.
/// The lifecycle owner must call `shutdown` to await all accepted preparation.
pub struct ResourceWorker {
    sender: tokio::sync::mpsc::Sender<Work>,
    task: tokio::task::JoinHandle<()>,
}

impl ResourceWorker {
    pub fn start() -> Self {
        let (sender, mut receiver) = tokio::sync::mpsc::channel::<Work>(1);
        // Stream ids belong to the worker's instance lifetime, not a selection.
        let stream_ids = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(1));
        let task = tokio::spawn(async move {
            while let Some((selection, reply)) = receiver.recv().await {
                if reply.is_closed() {
                    continue;
                }
                let Ok(mut prepared) =
                    tokio::task::spawn_blocking(move || PreparedResources::prepare(selection))
                        .await
                else {
                    continue;
                };
                prepared.next_stream = std::sync::Arc::clone(&stream_ids);
                let _ = reply.send(prepared);
            }
        });
        Self { sender, task }
    }

    /// Nonblocking, bounded submission. Saturation is explicit; callers retain
    /// their cold-change dirty flag and retry on completion, not on a timer.
    pub fn prepare(
        &self,
        selection: Selection,
    ) -> Option<tokio::sync::oneshot::Receiver<PreparedResources>> {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        self.sender.try_send((selection, sender)).ok()?;
        Some(receiver)
    }

    pub async fn shutdown(self) {
        drop(self.sender);
        let _ = self.task.await;
    }
}

#[cfg(test)]
mod tests;

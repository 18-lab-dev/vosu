//! The overlay root's read-only lease (A7.1 `OverlayRootView`, A7.1-OR §3):
//! core mints it for the bundled overlay host from the effective
//! `STATIC_FOLDER_PATH`; the host serves only through it and never learns
//! the path. A top-level directory link is a separately configured mount, as
//! in 1.x: its target is opened once and everything beneath it stays
//! confined to that target. Retiring a lease (root change, shutdown) makes
//! every later operation on it, including reads of open files, `TargetGone`.
use super::surface_files::{Authority, Root, native_error, open_root, opened};
use super::*;
use vosu_surface::files::{
    DirectoryReader, FileError, OpenedFile, Operation, RelativeChild, RootLease, RootOperations,
};

/// Ends one minted overlay root generation. Dropping it retires the lease.
pub struct OverlayRootRetirement(Arc<AtomicBool>);
impl Drop for OverlayRootRetirement {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

struct Inner {
    authority: Authority,
    path: std::path::PathBuf,
    root: Arc<SafeRoot>,
    current: Arc<AtomicBool>,
}

#[derive(Clone)]
struct OverlayRoot(Arc<Inner>);

impl OverlayRoot {
    /// The capability a child resolves under: the root, or the target of a
    /// top-level directory link, with the child path relative to it.
    fn select(
        &self,
        path: &RelativeChild,
    ) -> Result<(RootCapability, SafeRelativePath), FileError> {
        let inner = &self.0;
        inner.authority.check()?;
        if !inner.current.load(Ordering::Acquire) {
            return Err(FileError::TargetGone);
        }
        let child = SafeRelativePath::parse(path.as_str()).map_err(native_error)?;
        let capability = |root| RootCapability {
            root,
            current: Arc::clone(&inner.current),
            session_current: None,
        };
        if let Some(first) = child.as_path().components().next() {
            let mount = inner.path.join(first.as_os_str());
            let linked = std::fs::symlink_metadata(&mount)
                .is_ok_and(|metadata| metadata.file_type().is_symlink());
            if linked && std::fs::metadata(&mount).is_ok_and(|target| target.is_dir()) {
                let root = SafeRoot::open(&mount).map_err(native_error)?;
                return Ok((capability(Arc::new(root)), child.strip_first()));
            }
        }
        Ok((capability(Arc::clone(&inner.root)), child))
    }
}

impl RootOperations for OverlayRoot {
    fn open(&self, path: &RelativeChild) -> Operation<'_, OpenedFile> {
        let (root, path) = (self.clone(), path.clone());
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let (capability, child) = root.select(&path)?;
                opened(open_root(&capability, &child)?)
            })
            .await
            .map_err(|_| FileError::TargetGone)?
        })
    }

    fn directory(&self, path: &RelativeChild) -> Operation<'_, Box<dyn DirectoryReader>> {
        let (root, path) = (self.clone(), path.clone());
        Box::pin(async move {
            let selector = root.clone();
            let (capability, child) = tokio::task::spawn_blocking(move || selector.select(&path))
                .await
                .map_err(|_| FileError::TargetGone)??;
            let child = RelativeChild::parse(&child.as_path().to_string_lossy())?;
            RootLease::new(Root {
                authority: root.0.authority.clone(),
                root: capability,
            })
            .directory(&child)
            .await
        })
    }
}

impl PluginHost {
    /// Mints the overlay host's read-only `OverlayRootView` lease over
    /// `root` (absolute, already resolved by core). Only the live bundled
    /// `requester` may use it; the returned retirement ends it.
    pub fn overlay_root_lease(
        self: &Arc<Self>,
        requester: PluginInstanceId,
        root: &Path,
    ) -> Result<(RootLease, OverlayRootRetirement), FileError> {
        let authority = Authority {
            host: Arc::downgrade(self),
            requester,
        };
        authority.check()?;
        let opened_root = SafeRoot::open(root).map_err(native_error)?;
        let current = Arc::new(AtomicBool::new(true));
        Ok((
            RootLease::new(OverlayRoot(Arc::new(Inner {
                authority,
                path: root.to_owned(),
                root: Arc::new(opened_root),
                current: Arc::clone(&current),
            }))),
            OverlayRootRetirement(current),
        ))
    }
}

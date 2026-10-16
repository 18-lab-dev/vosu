//! Crash-safe network Apply journal and overlay-origin ownership history.
use super::{
    AtomicReplace, Config, ConfigStore,
    io::{atomic_write, reject_target_symlink},
};
use serde::{Deserialize, Serialize};
use std::{
    io,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
struct NetworkSettings {
    server_ip: String,
    server_port: f64,
    dashboard_ip: String,
    dashboard_port: f64,
    allowed_ips: String,
}
impl NetworkSettings {
    fn capture(config: &Config) -> Self {
        Self {
            server_ip: config.server_ip.clone(),
            server_port: config.server_port,
            dashboard_ip: config.dashboard_ip.clone(),
            dashboard_port: config.dashboard_port,
            allowed_ips: config.allowed_ips.clone(),
        }
    }
    fn restore(&self, config: &mut Config) {
        config.server_ip.clone_from(&self.server_ip);
        config.server_port = self.server_port;
        config.dashboard_ip.clone_from(&self.dashboard_ip);
        config.dashboard_port = self.dashboard_port;
        config.allowed_ips.clone_from(&self.allowed_ips);
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct OverlayEndpoint {
    host: String,
    port: f64,
}
#[derive(Default, Serialize, Deserialize)]
struct NetworkState {
    pending: Option<NetworkSettings>,
    overlay_origins: Vec<OverlayEndpoint>,
}
fn journal_path(path: &Path) -> PathBuf {
    path.with_extension("network.json")
}
fn load(path: &Path) -> io::Result<NetworkState> {
    reject_target_symlink(path)?;
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(NetworkState::default()),
        Err(error) => Err(error),
    }
}
fn persist(path: &Path, state: &NetworkState, replace: &dyn AtomicReplace) -> io::Result<()> {
    atomic_write(
        path,
        &serde_json::to_vec(state).map_err(io::Error::other)?,
        replace,
    )
}
// Unknown hostnames may resolve onto either bind; never rely on DNS to prove isolation.
fn overlapping_hosts(a: &str, b: &str) -> bool {
    let ip = |host: &str| {
        host.trim()
            .trim_matches(['[', ']'])
            .parse::<std::net::IpAddr>()
            .ok()
            .map(crate::server::auth::normalize_ip)
    };
    match (ip(a), ip(b)) {
        (Some(a), Some(b)) => a == b || a.is_unspecified() || b.is_unspecified(),
        _ => true,
    }
}
impl ConfigStore {
    /// Claim only after updater handoff and live-owner checks have completed.
    pub(crate) async fn claim_network_ownership(&self) -> io::Result<()> {
        self.network_owner
            .store(true, std::sync::atomic::Ordering::Release);
        self.rollback_pending_network().await
    }
    pub(crate) async fn validate_network(&self, candidate: &Config) -> io::Result<()> {
        let path = journal_path(self.path());
        let state = tokio::task::spawn_blocking(move || load(&path))
            .await
            .map_err(io::Error::other)??;
        let current = self.current();
        let conflicts = |host: &str, port: f64| {
            candidate.dashboard_port == port && overlapping_hosts(&candidate.dashboard_ip, host)
        };
        if conflicts(&candidate.server_ip, candidate.server_port)
            || conflicts(&current.server_ip, current.server_port)
            || (candidate.server_port == current.dashboard_port
                && overlapping_hosts(&candidate.server_ip, &current.dashboard_ip))
            || state
                .overlay_origins
                .iter()
                .any(|origin| conflicts(&origin.host, origin.port))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Dashboard origin must not overlap a current or previous overlay origin",
            ));
        }
        Ok(())
    }
    pub(crate) async fn stage_network(&self, candidate: &Config) -> io::Result<()> {
        let _guard = self.network_lock.lock().await;
        let path = journal_path(self.path());
        let config = self.current();
        self.network_owner
            .store(true, std::sync::atomic::Ordering::Release);
        let candidate_origin = OverlayEndpoint {
            host: candidate.server_ip.clone(),
            port: candidate.server_port,
        };
        let replace = self.atomic_replace.clone();
        tokio::task::spawn_blocking(move || {
            let mut state = load(&path)?;
            if state.pending.is_some() {
                return Err(io::Error::other("A network change is already pending"));
            }
            let origin = OverlayEndpoint {
                host: config.server_ip.clone(),
                port: config.server_port,
            };
            if !state.overlay_origins.contains(&origin) {
                state.overlay_origins.push(origin);
            }
            if !state.overlay_origins.contains(&candidate_origin) {
                state.overlay_origins.push(candidate_origin);
            }
            state.pending = Some(NetworkSettings::capture(&config));
            persist(&path, &state, replace.as_ref())
        })
        .await
        .map_err(io::Error::other)?
    }
    pub(crate) async fn confirm_network(&self) -> io::Result<()> {
        let _guard = self.network_lock.lock().await;
        let path = journal_path(self.path());
        let replace = self.atomic_replace.clone();
        tokio::task::spawn_blocking(move || {
            let mut state = load(&path)?;
            state.pending = None;
            persist(&path, &state, replace.as_ref())
        })
        .await
        .map_err(io::Error::other)?
    }
    pub(crate) async fn rollback_pending_network(&self) -> io::Result<()> {
        let _guard = self.network_lock.lock().await;
        let path = journal_path(self.path());
        let mut state = tokio::task::spawn_blocking({
            let path = path.clone();
            move || load(&path)
        })
        .await
        .map_err(io::Error::other)??;
        if let Some(previous) = state.pending.as_ref() {
            self.update(|config| previous.restore(config));
            self.flush().await?;
            state.pending = None;
            let replace = self.atomic_replace.clone();
            tokio::task::spawn_blocking(move || persist(&path, &state, replace.as_ref()))
                .await
                .map_err(io::Error::other)??;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn startup_and_shutdown_restore_unconfirmed_network_without_losing_other_settings() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("vosu.toml");
        let store = ConfigStore::initialize(&path).await.unwrap();
        store.stage_network(&store.current()).await.unwrap();
        store.update(|config| {
            config.server_port = 2405.0;
            config.debug_log = true;
        });
        store.flush().await.unwrap();
        // Simulate crash recovery: the new process sees persisted new values and a journal.
        let recovered = ConfigStore::initialize(&path).await.unwrap();
        recovered.claim_network_ownership().await.unwrap();
        assert_eq!(recovered.current().server_port, 24050.0);
        assert!(recovered.current().debug_log);
        recovered.stage_network(&recovered.current()).await.unwrap();
        recovered.update(|config| config.server_port = 2406.0);
        recovered.flush().await.unwrap();
        recovered.shutdown().await.unwrap();
        assert_eq!(recovered.current().server_port, 24050.0);
        assert!(load(&journal_path(&path)).unwrap().pending.is_none());
        store.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn rejects_overlapping_binds_and_remembers_previous_overlay_origins() {
        let root = tempfile::tempdir().unwrap();
        let store = ConfigStore::initialize(root.path().join("vosu.toml"))
            .await
            .unwrap();
        let mut swapped = (*store.current()).clone();
        swapped.server_port = swapped.dashboard_port;
        swapped.dashboard_port = 24890.0;
        assert!(
            store.validate_network(&swapped).await.is_err(),
            "the rollback dashboard cannot temporarily become an overlay origin"
        );
        for host in [
            "127.0.0.1",
            "0.0.0.0",
            "::",
            "localhost",
            "::ffff:127.0.0.1",
        ] {
            let mut candidate = (*store.current()).clone();
            candidate.dashboard_ip = host.into();
            candidate.dashboard_port = candidate.server_port;
            assert!(store.validate_network(&candidate).await.is_err(), "{host}");
        }
        store.stage_network(&store.current()).await.unwrap();
        store.update(|config| config.server_port = 24893.0);
        store.flush().await.unwrap();
        store.confirm_network().await.unwrap();
        let mut candidate = (*store.current()).clone();
        candidate.dashboard_port = 24050.0;
        assert!(store.validate_network(&candidate).await.is_err());
        candidate.dashboard_port = 24894.0;
        assert!(store.validate_network(&candidate).await.is_ok());
        store.shutdown().await.unwrap();
    }
}

#[cfg(test)]
mod ownership_tests {
    use super::*;
    #[tokio::test]
    async fn secondary_launch_preserves_pending_change_and_probes_current_dashboard() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("vosu.toml");
        let owner = ConfigStore::initialize(&path).await.unwrap();
        let mut candidate = (*owner.current()).clone();
        candidate.server_port = 24893.0;
        candidate.dashboard_port = 24894.0;
        owner.stage_network(&candidate).await.unwrap();
        owner.update(|config| *config = candidate.clone());
        owner.flush().await.unwrap();
        let journal = std::fs::read(journal_path(&path)).unwrap();
        let secondary = ConfigStore::initialize(&path).await.unwrap();
        assert_eq!(
            secondary.current().dashboard_port,
            24894.0,
            "live-owner probe must use the pending dashboard port"
        );
        assert_eq!(std::fs::read(journal_path(&path)).unwrap(), journal);
        secondary.shutdown().await.unwrap();
        assert_eq!(owner.current().dashboard_port, 24894.0);
        assert_eq!(std::fs::read(journal_path(&path)).unwrap(), journal);
        owner.rollback_pending_network().await.unwrap();
        assert_eq!(owner.current().dashboard_port, 24052.0);
        let mut later = (*owner.current()).clone();
        later.dashboard_port = 24893.0;
        assert!(
            owner.validate_network(&later).await.is_err(),
            "rolled-back candidate overlay origins remain reserved"
        );
        owner.shutdown().await.unwrap();
    }
}

use super::*;
pub(super) struct SnapshotPathResolver<'a> {
    pub(super) executable: PathBuf,
    pub(super) launch: PathBuf,
    pub(super) environment: &'a EnvironmentSnapshot,
}

impl PathResolver for SnapshotPathResolver<'_> {
    fn current_exe(&self) -> io::Result<PathBuf> {
        Ok(self.executable.clone())
    }

    fn launch_dir(&self) -> io::Result<PathBuf> {
        Ok(self.launch.clone())
    }

    fn env_var(&self, name: &str) -> Option<OsString> {
        self.environment.get(name).map(OsString::from)
    }
}

/// Request authorization policy whose immutable snapshot is replaced on config
/// reload. Requests already in flight keep their loaded policy; subsequent
/// requests observe the new allow-list and server host without a listener
/// restart.
pub struct ReloadableAuthorizer {
    policy: ArcSwap<AuthorizationPolicy>,
}

impl ReloadableAuthorizer {
    pub fn new(allowed_ips: &str, server_host: &str) -> Self {
        Self {
            policy: ArcSwap::from_pointee(AuthorizationPolicy::new(allowed_ips, server_host)),
        }
    }

    pub fn reload(&self, allowed_ips: &str, server_host: &str) {
        self.policy
            .store(Arc::new(AuthorizationPolicy::new(allowed_ips, server_host)));
    }
}

impl RequestAuthorizer for ReloadableAuthorizer {
    fn authorize(
        &self,
        peer: std::net::IpAddr,
        headers: RequestHeaders<'_>,
    ) -> AuthorizationDecision {
        self.policy.load().authorize(peer, headers)
    }
}

//! Browser credentials terminate here; core sees only an exact launch principal.
use std::sync::{Arc, Weak};

use crate::actions::{ActionDescriptor, ActionRegistry, JobOwner, Principal, ToolAuthority};

use super::{
    Entry, ToolRegistry,
    capabilities::{EmptyRegistry, InstalledCapabilities},
};

pub(super) const MAX_LAUNCH_SESSIONS: usize = 1024;

pub(super) struct LaunchSession {
    pub tool: String,
    entry: Weak<Entry>,
    owner: JobOwner,
}
impl LaunchSession {
    pub(super) fn new(entry: &Arc<Entry>) -> Self {
        Self {
            tool: entry.id.clone(),
            entry: Arc::downgrade(entry),
            owner: JobOwner::for_tool(Arc::from(entry.id.as_str())),
        }
    }
    pub(super) fn is_live(&self) -> bool {
        self.entry.strong_count() != 0
    }
    /// Whether this session was exchanged for exactly this package discovery.
    pub(super) fn is(&self, entry: &Arc<Entry>) -> bool {
        std::ptr::eq(self.entry.as_ptr(), Arc::as_ptr(entry))
    }
    pub(super) fn owner(&self) -> JobOwner {
        self.owner.clone()
    }
}

pub(super) struct Providers {
    pub actions: Arc<dyn ActionRegistry>,
    pub capabilities: Arc<dyn InstalledCapabilities>,
}
impl Default for Providers {
    fn default() -> Self {
        Self {
            actions: Arc::new(EmptyRegistry),
            capabilities: Arc::new(EmptyRegistry),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Forbidden;

impl ToolRegistry {
    /// Composition injects live loaded-provider metadata. Empty by default.
    pub fn set_action_providers(
        &self,
        actions: Arc<dyn ActionRegistry>,
        capabilities: Arc<dyn InstalledCapabilities>,
    ) {
        *self.action_providers.lock() = Providers {
            actions,
            capabilities,
        };
        self.publish();
    }

    /// The API v1 job registry whose jobs an invalidated session's owner
    /// asks to cancel (API v1 "Tool launch sessions" item 6).
    pub fn set_jobs(&self, jobs: crate::actions::JobRegistry) {
        *self.jobs.lock() = Some(jobs);
    }

    /// Recheck this on every action/job request, using the job's fixed action.
    /// Lock order: sessions -> release -> entries -> release -> saved/state.
    /// Metadata callbacks always run without any registry lock held.
    pub fn authorize_action_session(
        &self,
        token: &str,
        action: &str,
    ) -> Result<Principal, Forbidden> {
        let (entry, owner) = self.session_record(token)?;
        self.authorize_entry(&entry, action, None)?;
        Ok(Principal::Tool(owner))
    }

    /// Authenticate only for owner-scoped job lookup. Before exposing or
    /// cancelling the job, reauthorize its fixed action with the method above.
    pub fn authenticate_action_session(&self, token: &str) -> Result<Principal, Forbidden> {
        let (entry, owner) = self.session_record(token)?;
        self.check_live_entry(&entry)?;
        Ok(Principal::Tool(owner))
    }

    fn session_record(&self, token: &str) -> Result<(Arc<Entry>, JobOwner), Forbidden> {
        let sessions = self.sessions.lock();
        let record = sessions
            .iter()
            .find(|(key, _)| super::equal(key, token))
            .map(|(_, record)| record)
            .ok_or(Forbidden)?;
        Ok((
            record.entry.upgrade().ok_or(Forbidden)?,
            record.owner.clone(),
        ))
    }

    fn check_live_entry(&self, entry: &Arc<Entry>) -> Result<(), Forbidden> {
        if self.stopping.load(std::sync::atomic::Ordering::Acquire) {
            return Err(Forbidden);
        }
        let current = self.entry(&entry.id).map_err(|_| Forbidden)?;
        if !Arc::ptr_eq(entry, &current)
            || !self.enabled(entry)
            || !self.trusted(entry)
            || entry.manifest.is_none()
            || entry.error.is_some()
        {
            return Err(Forbidden);
        }
        Ok(())
    }

    fn validation_snapshot(
        &self,
        installed: &dyn InstalledCapabilities,
    ) -> crate::plugin::ValidationSnapshot {
        // Build from actual current entries, never a second registry/cache.
        // Provider callbacks run before taking the tool entries lock.
        let mut snapshot = installed.validation_snapshot();
        snapshot
            .packages
            .extend(self.entries.lock().values().filter_map(|entry| {
                entry
                    .manifest
                    .as_ref()
                    .map(|manifest| manifest.registration(entry.source))
            }));
        snapshot
    }

    fn authorize_entry(
        &self,
        entry: &Arc<Entry>,
        action: &str,
        expected: Option<&ActionDescriptor>,
    ) -> Result<(), Forbidden> {
        self.check_live_entry(entry)?;
        let manifest = entry.manifest.as_ref().ok_or(Forbidden)?;
        let (registry, installed) = {
            let providers = self.action_providers.lock();
            (providers.actions.clone(), providers.capabilities.clone())
        };
        manifest
            .validate_context(&self.validation_snapshot(installed.as_ref()), entry.source)
            .map_err(|_| Forbidden)?;
        let descriptor = registry.get(action).ok_or(Forbidden)?;
        if descriptor.id != action
            || expected.is_some_and(|value| value.version != descriptor.version)
        {
            return Err(Forbidden);
        }
        if manifest
            .requires
            .iter()
            .any(|r| !r.resolved(registry.as_ref(), installed.as_ref()))
        {
            return Err(Forbidden);
        }
        if !manifest.requires.iter().chain(&manifest.optional).any(|r| {
            r.id.strip_prefix("action:") == Some(action)
                && r.satisfied_by_version(&r.id, &descriptor.version)
        }) {
            return Err(Forbidden);
        }
        Ok(())
    }

    /// Revoking one tab never changes any other tab's owner.
    pub fn revoke_action_session(&self, token: &str) {
        self.sessions
            .lock()
            .retain(|key, _| !super::equal(key, token));
    }

    pub(super) fn blocked_capabilities(&self, entry: &Entry) -> Option<String> {
        let manifest = entry.manifest.as_ref()?;
        let (actions, installed) = {
            let providers = self.action_providers.lock();
            (providers.actions.clone(), providers.capabilities.clone())
        };
        if let Err(error) =
            manifest.validate_context(&self.validation_snapshot(installed.as_ref()), entry.source)
        {
            return Some(error);
        }
        manifest
            .requires
            .iter()
            .find(|r| !r.resolved(actions.as_ref(), installed.as_ref()))
            .map(|r| format!("missing capability {}", r.id))
    }

    pub(super) fn capability_view(
        &self,
        entry: &Entry,
    ) -> Option<(serde_json::Value, serde_json::Value)> {
        let manifest = entry.manifest.as_ref()?;
        let (actions, installed) = {
            let providers = self.action_providers.lock();
            (providers.actions.clone(), providers.capabilities.clone())
        };
        let describe = |requirements: &[super::capabilities::Requirement]| {
            serde_json::Value::Array(
                requirements
                    .iter()
                    .map(|r| {
                        serde_json::json!({
                            "id": r.id, "version": r.raw_version,
                            "satisfied": r.resolved(actions.as_ref(), installed.as_ref()),
                            "provider": r.provider(actions.as_ref(), installed.as_ref()),
                        })
                    })
                    .collect(),
            )
        };
        Some((describe(&manifest.requires), describe(&manifest.optional)))
    }
}

impl ToolAuthority for ToolRegistry {
    fn authorizes(&self, owner: &JobOwner, action: &ActionDescriptor) -> bool {
        let entry = {
            let sessions = self.sessions.lock();
            sessions
                .values()
                .find(|record| record.owner.same(owner))
                .and_then(|record| record.entry.upgrade())
        };
        entry.is_some_and(|entry| {
            owner.tool_id() == Some(entry.id.as_str())
                && self
                    .authorize_entry(&entry, &action.id, Some(action))
                    .is_ok()
        })
    }
}

impl crate::server::api_v1::actions::ActionAuthority for ToolRegistry {
    fn authenticate_action_session(&self, token: &str) -> Option<Principal> {
        ToolRegistry::authenticate_action_session(self, token).ok()
    }

    fn authorize_action_session(&self, token: &str, action: &str) -> Option<Principal> {
        ToolRegistry::authorize_action_session(self, token, action).ok()
    }
}

//! Exact loaded-action metadata lookup shared with tool authorization.
use std::sync::Arc;

use semver::Version;
use vosu_plugin_api::ActionSessionMode;

use crate::plugin::{action_schema::CompiledActionSchema, host_services::PluginInstanceId};

#[derive(Clone, Debug)]
pub struct ActionDescriptor {
    /// Full bare action id, never split to infer a provider boundary.
    pub id: String,
    pub version: Version,
    pub provider: PluginInstanceId,
    pub provider_id: String,
    pub title: String,
    pub session: ActionSessionMode,
    pub cli: bool,
    pub args_schema: Arc<CompiledActionSchema>,
    pub result_schema: Arc<CompiledActionSchema>,
}

/// Only currently loaded action providers may be returned.
pub trait ActionRegistry: Send + Sync {
    fn get(&self, action: &str) -> Option<Arc<ActionDescriptor>>;
}

/// Live launch/grant check, consulted for every tool invocation.
/// Missing authority is deny-by-default.
pub trait ToolAuthority: Send + Sync {
    fn authorizes(&self, owner: &super::JobOwner, action: &ActionDescriptor) -> bool;
}

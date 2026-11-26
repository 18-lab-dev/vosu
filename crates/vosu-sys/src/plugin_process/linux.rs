//! Linux objects behind one ring direction: a sealed anonymous `memfd`
//! mapping and an `eventfd` doorbell (`docs/v2/plugin-abi.md` §7.2), plus
//! the plugin host's launch, inherited-endpoint ownership and privilege drop
//! (§7.3, §7.6 "Launch", A5.1(b), A5.8).

mod capabilities;
mod channel;
mod objects;
mod spawn;

pub use capabilities::{
    NamespacePolicy, ParentDeathError, ReadbackMismatch, SecurityCode, SecurityFailure,
    SecurityReadback, SecurityStep, arm_parent_death, drop_privileges,
};
pub use channel::{
    ENDPOINT_ARGUMENT, ENDPOINT_FDS, EndpointError, InheritedDirection, InheritedEndpoints,
};
pub use objects::Doorbell;
pub(super) use objects::Mapping;
pub(super) use objects::{adopt_mapping, create_mapping};
pub use spawn::{
    ChildStage, PluginHostChild, PluginHostCommand, PluginHostLauncher, SpawnError,
    USER_NAMESPACES_ARGUMENT,
};

#[cfg(test)]
mod tests;

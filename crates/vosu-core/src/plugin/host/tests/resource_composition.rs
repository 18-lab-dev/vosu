//! The host must retain composition's exact persistent store, not quietly build
//! an ephemeral replacement that gives different identities after restart.
use std::sync::Arc;
use std::time::Duration;
use vosu_plugin_api::{CborValue, ResourceRef};

use super::helpers::{DummyStorage, SilentConsole, test_host};
use crate::logging::{Logger, LoggerOptions};
use crate::plugin::PluginHost;
use crate::plugin::settings::PluginSettingsService;
use crate::resources::{ReferenceClass, ReferenceIdentity, ResourceRegistry, ResourceSecret};
use crate::state::ReferenceStore;

#[test]
fn resource_requests_composition_retains_persisted_reference_store() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _enter = runtime.enter();
    let data = tempfile::tempdir().unwrap();
    let secret_path = data.path().join("resource-secret");
    let resources = Arc::new(ReferenceStore::new(
        ResourceSecret::open_or_create(&secret_path).unwrap(),
        ResourceRegistry::default(),
    ));
    assert!(resources.is_persistent());
    let logs = tempfile::tempdir().unwrap();
    let logger = Logger::new(
        logs.path(),
        LoggerOptions {
            version: "0.1.0".into(),
            debug_log: false,
            queue_capacity: 64,
        },
        Arc::new(SilentConsole),
    )
    .unwrap();
    let settings = Arc::new(PluginSettingsService::new(Arc::new(DummyStorage)));
    let host = PluginHost::new_with_resources(
        logger,
        settings,
        runtime.handle().clone(),
        resources.clone(),
    );
    assert!(Arc::ptr_eq(&host.state().store.resources, &resources));
    assert!(host.state().store.resources.is_persistent());
    let reference = ResourceRef {
        kind: "file".into(),
        key: CborValue::Text("persisted-key-83".into()),
        name: "persisted.osu".into(),
        content_id: CborValue::Integer(89.into()),
    };
    let first = ReferenceIdentity::new(
        &host.state().store.resources.secret,
        "testkit.resources.composition",
        ReferenceClass::Resource,
        reference.clone(),
    )
    .unwrap();
    let reopened = ResourceSecret::open_or_create(&secret_path).unwrap();
    let restarted = ReferenceIdentity::new(
        &reopened,
        "testkit.resources.composition",
        ReferenceClass::Resource,
        reference.clone(),
    )
    .unwrap();
    assert_eq!(first.id, restarted.id);
    let (_other_logs, ephemeral) = test_host(runtime.handle().clone());
    assert!(!ephemeral.state().store.resources.is_persistent());
    let isolated = ReferenceIdentity::new(
        &ephemeral.state().store.resources.secret,
        "testkit.resources.composition",
        ReferenceClass::Resource,
        reference,
    )
    .unwrap();
    assert_ne!(first.id, isolated.id);
    host.destroy_all(Duration::from_secs(2));
    ephemeral.destroy_all(Duration::from_secs(2));
}

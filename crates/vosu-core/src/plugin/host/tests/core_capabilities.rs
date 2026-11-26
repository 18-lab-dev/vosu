//! Core namespaces and core resource kinds are always-installed capabilities
//! at their core namespace's schema version (`docs/v2/catalog-v2.md`
//! "Built-in providers"), exactly as the install resolver's builtins report
//! them; they never name a plugin provider.

use super::helpers::test_host;
use crate::state::schema::{CORE_RESOURCE_KINDS, SchemaRegistry};

#[test]
fn core_resource_kinds_resolve_at_their_namespace_schema_version() {
    let runtime = tokio::runtime::Runtime::new().expect("building a Tokio runtime");
    let _enter = runtime.enter();
    let (_log_dir, host) = test_host(runtime.handle().clone());
    let registry = SchemaRegistry::with_core().expect("core schemas compile");
    for (kind, namespace) in CORE_RESOURCE_KINDS {
        let capability = format!("resource:{kind}");
        let expected = registry
            .get(namespace)
            .expect("core namespace")
            .version
            .clone();
        assert_eq!(
            host.installed_capability_version(&capability),
            Some(expected),
            "{capability}"
        );
        assert_eq!(
            host.installed_capability_provider(&capability),
            None,
            "{capability}"
        );
    }
    assert_eq!(
        host.installed_capability_version("field:vosu.chart"),
        Some(registry.get("vosu.chart").unwrap().version.clone())
    );
    // Not a core kind, and no live plugin provides it.
    assert_eq!(
        host.installed_capability_version("resource:chart.lyrics"),
        None
    );
    assert_eq!(
        host.installed_capability_version("resource:fixture.thing"),
        None
    );
}

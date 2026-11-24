//! M6a rule corpus: independent controls, SDK diagnostics, offline shape and real loaders.
#![expect(
    clippy::disallowed_methods,
    reason = "corpus fixtures write only isolated temporary package directories"
)]
#[path = "catalog_v2/autoremove.rs"]
mod autoremove;
#[path = "catalog_v2/capability.rs"]
mod capability;
#[path = "catalog_v2/catalog.rs"]
mod catalog;
#[path = "catalog_v2/consumers.rs"]
mod consumers;
#[path = "catalog_v2/context.rs"]
mod context;
#[path = "catalog_v2/declarations.rs"]
mod declarations;
#[path = "catalog_v2/game_detection.rs"]
mod game_detection;
#[path = "catalog_v2/install.rs"]
mod install;
#[path = "catalog_v2/legacy.rs"]
mod legacy;
#[path = "catalog_v2/manifest.rs"]
mod manifest;
#[path = "catalog_v2/matcher.rs"]
mod matcher;
#[path = "catalog_v2/mirror.rs"]
mod mirror;
#[path = "catalog_v2/plans.rs"]
mod plans;
#[path = "catalog_v2/plugin.rs"]
mod plugin;
#[path = "catalog_v2/resolver.rs"]
mod resolver;
#[path = "catalog_v2/schema.rs"]
mod schema;
#[path = "catalog_v2/support.rs"]
mod support;

#[test]
fn corpus_inventory_has_paired_controls_and_stable_diagnostics() {
    for (family, rows) in [
        ("manifest", manifest::corpus()),
        ("plugin", plugin::corpus()),
        ("declarations", declarations::corpus()),
        ("catalog", catalog::corpus()),
    ] {
        println!("{family}: {} paired rejection rows", rows.len());
        assert!(!rows.is_empty());
        for row in rows {
            assert_ne!(row.valid, row.invalid, "{}: {}", family, row.label);
            assert!(!row.rule.is_empty());
            assert!(row.pointer.is_empty() || row.pointer.starts_with('/'));
        }
    }
}

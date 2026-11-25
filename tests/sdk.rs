//! Workspace-owned SDK checks that need files outside the published
//! `vosu-plugin-api` package (AF.9): the C header's constants and layout,
//! the offline packaging JSON schemas under `docs/v2/schemas/`, and the
//! `prop_*` tests that share the workspace's `vosu-proptest` seed policy.
//! The SDK's own `cargo test -p vosu-plugin-api` stays self-contained.

#[path = "sdk/header_constants.rs"]
mod header_constants;
#[path = "sdk/layout_header.rs"]
mod layout_header;
#[path = "sdk/packaging_schema.rs"]
mod packaging_schema;
#[path = "sdk/prop_publish.rs"]
mod prop_publish;
#[path = "sdk/prop_validate.rs"]
mod prop_validate;

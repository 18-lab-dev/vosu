//! A8.1 / A8.3 idempotent legacy-override import planning: validates every
//! candidate legacy value against the plugin's real [`SettingsSchema`] before
//! any write, lets existing valid per-plugin overrides win per explicit key,
//! preserves explicit `false` / `0` / `0.0` / `""` values, and produces a
//! structured [`ImportOutcome`] for migration-journal receipts.

use serde_json::{Map, Value};
use vosu_plugin_api::manifest::settings::SettingsSchema;
use vosu_plugin_api::{Message, SettingsChanged};

use super::SettingsError;
use super::service::{json_map_to_cbor, valid_overrides};

/// Structured outcome of importing legacy overrides into a plugin's settings
/// store (A8.1 / A8.3).
///
/// Key lists are deterministically sorted so callers (such as a migration
/// journal) can record stable per-step receipts.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportOutcome {
    /// Explicitly-present legacy keys that were absent from existing valid
    /// per-plugin overrides and were durably written to storage.
    pub imported_keys: Vec<String>,
    /// Explicitly-present legacy keys skipped because valid per-plugin
    /// overrides already existed in storage (A8.1: existing valid overrides
    /// always win).
    pub preserved_existing_keys: Vec<String>,
    /// Complete effective settings object (schema defaults merged with the
    /// durable per-plugin overrides).
    pub effective: Map<String, Value>,
}

impl ImportOutcome {
    /// Whether any legacy key was newly imported and durably written.
    pub fn changed(&self) -> bool {
        !self.imported_keys.is_empty()
    }

    /// Builds the complete `VOSU_OP_SETTINGS_CHANGED` message matching
    /// [`Self::effective`] (§6.9).
    pub fn settings_changed_message(&self) -> Message {
        Message::SettingsChanged(SettingsChanged {
            values: json_map_to_cbor(self.effective.clone()),
        })
    }
}

/// Validated, in-memory merge plan computed before any durable write.
pub(super) struct ImportPlan {
    pub(super) imported_keys: Vec<String>,
    pub(super) preserved_existing_keys: Vec<String>,
    pub(super) next_overrides: Map<String, Value>,
    pub(super) effective: Map<String, Value>,
}

/// Validates every entry in `legacy_overrides` against `schema` and computes
/// the A8.1 merge against `existing` stored overrides without performing I/O.
///
/// - Any unknown, layout-only, or type-mismatched key in `legacy_overrides`
///   fails immediately with [`SettingsError::Schema`] (which names only the
///   setting key, never the value) — even if that key also exists in
///   `existing`.
/// - Existing *valid* overrides in `existing` always win per explicit key.
/// - Keys absent from valid existing overrides are imported verbatim from
///   `legacy_overrides`, preserving explicit `false`, `0`, `0.0`, and `""`.
pub(super) fn plan_import(
    schema: &SettingsSchema,
    existing: Map<String, Value>,
    legacy_overrides: &Map<String, Value>,
) -> Result<ImportPlan, SettingsError> {
    // Validate every candidate legacy entry upfront before inspecting or
    // modifying existing overrides so malformed legacy input always fails
    // closed without touching storage.
    schema.merge(legacy_overrides)?;

    let mut next_overrides = valid_overrides(schema, existing);
    let mut imported_keys = Vec::new();
    let mut preserved_existing_keys = Vec::new();

    for (key, value) in legacy_overrides {
        if next_overrides.contains_key(key) {
            preserved_existing_keys.push(key.clone());
        } else {
            next_overrides.insert(key.clone(), value.clone());
            imported_keys.push(key.clone());
        }
    }

    imported_keys.sort();
    preserved_existing_keys.sort();

    let effective = schema.merge(&next_overrides)?;

    Ok(ImportPlan {
        imported_keys,
        preserved_existing_keys,
        next_overrides,
        effective,
    })
}

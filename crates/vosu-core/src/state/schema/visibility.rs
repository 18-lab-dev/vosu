//! The `x-visibility` carve-out (D2; `docs/v2/state-schema.md`'s `internal`
//! section, ss:297-311; cat:58-85): `"x-visibility": "plugins"` is accepted
//! only on the officially-owned `vosu.osu` schema's literal top-level
//! `/properties/internal` key, at schema load. Anywhere else — a different
//! property, a nested path under `internal`, any other namespace, any
//! schema owner other than core or the `vosu.osu` plugin itself, any
//! `$defs` entry whether or not a `$ref` reaches it, any other JSON Schema
//! keyword (`additionalProperties`, `patternProperties`, `prefixItems`,
//! array-form `items`, `not`, `if`/`then`/`else`, `dependentSchemas`), or a
//! `$ref`/`oneOf` indirection that would otherwise let a definition's
//! annotation apply at `/internal` without literally sitting there — is a
//! load-time error, never a silent ignore.
//!
//! [`scan`] enforces this by walking the *entire* parsed document by its
//! literal JSON structure (every object key, every array index),
//! independent of which keywords the lane walker itself recurses into, so
//! none of the above can bypass it.

use std::sync::Arc;

use serde_json::Value;

use super::{SchemaError, SchemaOwner};

/// The one location `x-visibility: "plugins"` may literally sit at.
const ALLOWED_PATH: [&str; 2] = ["properties", "internal"];

/// Whether `owner` is allowed to carry the `vosu.osu` `/internal`
/// carve-out: the core bring-up fixture (`SchemaRegistry::with_core`,
/// documented there as a stand-in for S6/O6's
/// `BundledPlugin.namespace_schemas` wiring) or the `vosu.osu` plugin
/// itself. Basing this on the owner, not the title alone, is what stops a
/// same-titled schema registered under a different owner from ever
/// reaching the carve-out.
fn owner_eligible(owner: &SchemaOwner) -> bool {
    match owner {
        SchemaOwner::Core => true,
        SchemaOwner::Plugin(id) => id.as_ref() == "vosu.osu",
    }
}

/// Scans `root` for `x-visibility`, wherever it literally sits. `namespace`
/// is the `provides` id this document is being loaded under (its `title`
/// is checked separately, before this call).
pub(super) fn scan(namespace: &str, owner: &SchemaOwner, root: &Value) -> Result<(), SchemaError> {
    let mut path = Vec::new();
    scan_node(namespace, owner, root, &mut path)
}

fn scan_node(
    namespace: &str,
    owner: &SchemaOwner,
    node: &Value,
    path: &mut Vec<String>,
) -> Result<(), SchemaError> {
    match node {
        Value::Object(map) => {
            if let Some(value) = map.get("x-visibility") {
                let allowed = namespace == "vosu.osu"
                    && owner_eligible(owner)
                    && path.as_slice() == ALLOWED_PATH
                    && value.as_str() == Some("plugins");
                if !allowed {
                    return Err(SchemaError::VisibilityNotAllowed {
                        namespace: namespace.to_string(),
                        at: format!("/{}", path.join("/")),
                    });
                }
            }
            for (key, value) in map {
                if key == "x-visibility" {
                    continue;
                }
                path.push(key.clone());
                scan_node(namespace, owner, value, path)?;
                path.pop();
            }
            Ok(())
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                path.push(index.to_string());
                scan_node(namespace, owner, item, path)?;
                path.pop();
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// `vosu.osu`'s `/internal` pointer, iff [`scan`] has already confirmed no
/// *disallowed* `x-visibility` occurrence exists: the only possibility left
/// is the canonical one, or none at all, so this only needs to re-read that
/// one spot.
pub(super) fn hidden_subtrees(namespace: &str, owner: &SchemaOwner, root: &Value) -> Vec<Arc<str>> {
    if namespace != "vosu.osu" || !owner_eligible(owner) {
        return Vec::new();
    }
    let marked = root
        .get("properties")
        .and_then(|p| p.get("internal"))
        .and_then(|internal| internal.get("x-visibility"))
        .and_then(Value::as_str)
        == Some("plugins");
    if marked {
        vec![Arc::from("/internal")]
    } else {
        Vec::new()
    }
}

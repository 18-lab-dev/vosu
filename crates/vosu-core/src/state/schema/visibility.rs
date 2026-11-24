//! The `x-visibility` carve-out (D2; `docs/v2/state-schema.md`'s `internal`
//! section, ss:297-311; cat:58-85): `"x-visibility": "plugins"` is accepted
//! only on `vosu.osu`'s top-level `/internal` key, at schema load. Anywhere
//! else — a different subtree, a nested path under `/internal`, any other
//! namespace (core or plugin), or any value other than `"plugins"` at the
//! one allowed location — is a load-time error, never a silent ignore.

use super::SchemaError;

/// `path` is the property path (root-relative tokens, no leading `""`) the
/// `x-visibility` annotation was found at.
pub(super) fn check(namespace: &str, path: &[&str], value: &str) -> Result<(), SchemaError> {
    let allowed = namespace == "vosu.osu" && path == ["internal"] && value == "plugins";
    if allowed {
        return Ok(());
    }
    Err(SchemaError::VisibilityNotAllowed {
        namespace: namespace.to_string(),
        at: format!("/{}", path.join("/")),
    })
}

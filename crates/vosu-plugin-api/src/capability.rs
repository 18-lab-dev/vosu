//! Capability id grammar: `field:`/`resource:`/`action:`/`game:` prefixes
//! over a dotted path (`docs/v2/naming.md` "Capability id grammar",
//! `docs/v2/catalog-v2.md` "Capability fields"). A capability id's
//! namespace boundary is ambiguous from the string alone. This module checks
//! the prefix/suffix grammar; [`crate::manifest::NamespaceIndex`] resolves
//! boundaries using explicit bundled/installed/enabled-source registrations.
//! [`crate::manifest::Requirement`] combines coverage with semver without
//! conflating capability and package-release versions.
//!
//! The segment grammars here (`valid_kind_segment`/`valid_camel_path_segment`)
//! are also `crate::manifest`'s: a resource kind, action-name segment, and
//! plugin-id segment share one dash grammar, and a `field:` path segment
//! below a namespace uses a separate camelCase grammar
//! (`docs/v2/naming.md` "Kinds", `docs/v2/state-schema.md` "Naming"); they
//! live here, the lower-level module, so neither module re-derives a third
//! grammar of its own.

use core::fmt;

/// One parsed capability id: the reserved prefix plus its dotted suffix
/// (everything after `field:`/`resource:`/`action:`/`game:`), unparsed past
/// that — see the module doc comment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityId<'a> {
    Field(&'a str),
    Resource(&'a str),
    Action(&'a str),
    Game(&'a str),
}

/// A capability id failed the grammar `docs/v2/naming.md` "Capability id
/// grammar" states.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityError(String);

impl fmt::Display for CapabilityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid capability id {:?}", self.0)
    }
}

impl std::error::Error for CapabilityError {}

/// `[a-z0-9][a-z0-9-]{0,31}` segment grammar shared by resource kinds,
/// action-name segments, and plugin id segments (`docs/v2/naming.md`
/// "Kinds"). `max_len`/`allow_underscore` let `crate::manifest`'s
/// `root_slots` grammar (`[a-z0-9][a-z0-9_-]{0,63}`) reuse the same
/// first-character rule without a third near-duplicate.
pub(crate) fn valid_segment(s: &str, max_len: usize, allow_underscore: bool) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return false;
    }
    s.len() <= max_len
        && chars.all(|c| {
            c.is_ascii_lowercase()
                || c.is_ascii_digit()
                || c == '-'
                || (allow_underscore && c == '_')
        })
}

/// `[a-z0-9][a-z0-9-]{0,31}` (`docs/v2/naming.md` "Kinds"): a resource
/// kind, an action-name segment, or a plugin-id segment.
pub(crate) fn valid_kind_segment(s: &str) -> bool {
    valid_segment(s, 32, false)
}

/// One dotted segment of a `field:` id's path part below its namespace:
/// the dotted form of a namespace's own camelCase JSON Schema key
/// (`docs/v2/state-schema.md` "Naming": schema keys are camelCase and
/// never contain a dot).
pub(crate) fn valid_camel_path_segment(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_alphanumeric())
}

/// `^[a-z][a-z0-9-]{0,31}$` (`docs/v2/naming.md` "Game ids").
fn valid_game_id(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase())
        && s.len() <= 32
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// `vosu.sessions` is reserved and never a capability namespace or a
/// descendant of one (`docs/v2/naming.md` "Namespaces": "it is never a
/// plugin id, a published data namespace, or a `field:`/`resource:`/
/// `action:` capability, in any manifest, schema, or catalog entry").
const RESERVED_SESSIONS_NAMESPACE: &str = "vosu.sessions";

fn names_reserved_sessions_namespace(suffix: &str) -> bool {
    suffix == RESERVED_SESSIONS_NAMESPACE
        || suffix
            .strip_prefix(RESERVED_SESSIONS_NAMESPACE)
            .is_some_and(|tail| tail.starts_with('.'))
}

/// The five core namespaces (`docs/v2/naming.md` "Namespaces"), the only
/// namespaces this module can recognize by name without the full
/// installed/catalogued set (module doc comment).
pub(crate) const CORE_NAMESPACES: [&str; 5] = [
    "vosu.client",
    "vosu.status",
    "vosu.chart",
    "vosu.play",
    "vosu.skin",
];

/// A `field:` id's suffix is a namespace (bare, at least two dot-segments —
/// every core namespace is `vosu.<x>` and every plugin id is 2-4 segments,
/// so no valid namespace is a single segment) or a namespace plus a
/// dotted camelCase path below it; `docs/v2/naming.md` "Namespaces"
/// reserves `vosu.sessions` out of this entirely.
fn valid_field_suffix(suffix: &str) -> bool {
    if names_reserved_sessions_namespace(suffix) {
        return false;
    }
    // A core namespace is a closed, known set (unlike an arbitrary
    // plugin id): once the suffix names one, whatever remains below it
    // is a schema-key path, camelCase only, never a dash segment — this
    // mirrors `crate::manifest::valid_covers_entry`'s same core-namespace
    // check.
    for namespace in CORE_NAMESPACES {
        if suffix == namespace {
            return true;
        }
        if let Some(path) = suffix
            .strip_prefix(namespace)
            .and_then(|rest| rest.strip_prefix('.'))
        {
            return !path.is_empty() && path.split('.').all(valid_camel_path_segment);
        }
    }
    // Not a core namespace: a plugin's own namespace (2-4 dash segments)
    // optionally followed by a camelCase path below it. The
    // namespace/path boundary is unknowable here (module doc comment),
    // so every segment independently matches either grammar.
    let segments: Vec<&str> = suffix.split('.').collect();
    segments.len() >= 2
        && segments
            .iter()
            .all(|s| valid_kind_segment(s) || valid_camel_path_segment(s))
}

/// A `resource:` id's suffix: a bare core kind (`"skin"`), a core kind
/// under its owning namespace's short name (`"chart.background"`), or a
/// plugin's own kind (`"<plugin-id>.<kind>"`) — every segment the
/// dash-grammar `valid_kind_segment` (`docs/v2/naming.md` "Kinds";
/// resource suffixes never carry a camelCase path component).
fn valid_resource_suffix(suffix: &str) -> bool {
    suffix.split('.').all(valid_kind_segment)
}

/// An `action:` id's suffix: `<plugin-id>.<name>`, where `<plugin-id>` is
/// 2-4 segments and `<name>` is one or more further dot-segments
/// (`docs/v2/naming.md` "Kinds": "the full capability id is
/// `action:<plugin-id>.<name>`") — at least three segments total, every
/// one the dash grammar.
fn valid_action_suffix(suffix: &str) -> bool {
    let segments: Vec<&str> = suffix.split('.').collect();
    segments.len() >= 3 && segments.iter().all(|s| valid_kind_segment(s))
}

/// Parses one capability id into its prefix and suffix, checking the
/// suffix's own grammar (`docs/v2/naming.md` "Capability id grammar",
/// "Kinds", "Game ids", "Namespaces"). Namespace-boundary resolution is
/// performed by [`crate::manifest::NamespaceIndex`], not guessed here.
pub fn parse_capability(id: &str) -> Result<CapabilityId<'_>, CapabilityError> {
    let err = || CapabilityError(id.to_string());
    if let Some(suffix) = id.strip_prefix("field:") {
        return valid_field_suffix(suffix)
            .then_some(CapabilityId::Field(suffix))
            .ok_or_else(err);
    }
    if let Some(suffix) = id.strip_prefix("resource:") {
        return (!names_reserved_sessions_namespace(suffix) && valid_resource_suffix(suffix))
            .then_some(CapabilityId::Resource(suffix))
            .ok_or_else(err);
    }
    if let Some(suffix) = id.strip_prefix("action:") {
        return (!names_reserved_sessions_namespace(suffix) && valid_action_suffix(suffix))
            .then_some(CapabilityId::Action(suffix))
            .ok_or_else(err);
    }
    if let Some(suffix) = id.strip_prefix("game:") {
        return valid_game_id(suffix)
            .then_some(CapabilityId::Game(suffix))
            .ok_or_else(err);
    }
    Err(err())
}

/// `docs/v2/catalog-v2.md` "Capability coverage": a provided id satisfies a
/// required id when they are equal, or when the required id is the
/// provided id plus one or more further dotted segments. Operates on whole
/// capability id strings (prefix included), so a `field:` provider can
/// only ever cover another `field:` requirement, never a `resource:` one.
pub fn covers(provided: &str, required: &str) -> bool {
    provided == required
        || required
            .strip_prefix(provided)
            .is_some_and(|rest| rest.starts_with('.'))
}

/// Converts a `field:<namespace>[.<path>]` id into an RFC 6901 JSON
/// Pointer relative to `namespace`'s root: `field_to_pointer("vosu.osu",
/// "field:vosu.osu.play.pp")` is `Some("/play/pp")`, and the bare
/// namespace id itself maps to the root pointer `Some("")`. Returns `None`
/// when `field_id` is not a `field:` id, or does not name `namespace` or a
/// path below it.
pub fn field_to_pointer(namespace: &str, field_id: &str) -> Option<String> {
    let suffix = field_id.strip_prefix("field:")?;
    if suffix == namespace {
        return Some(String::new());
    }
    let path = suffix.strip_prefix(namespace)?.strip_prefix('.')?;
    Some(path.split('.').fold(String::new(), |mut pointer, segment| {
        pointer.push('/');
        pointer.push_str(segment);
        pointer
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_prefix() {
        assert_eq!(
            parse_capability("field:vosu.osu").unwrap(),
            CapabilityId::Field("vosu.osu")
        );
        assert_eq!(
            parse_capability("field:vosu.play.combo").unwrap(),
            CapabilityId::Field("vosu.play.combo")
        );
        assert_eq!(
            parse_capability("resource:chart.background").unwrap(),
            CapabilityId::Resource("chart.background")
        );
        assert_eq!(
            parse_capability("resource:skin").unwrap(),
            CapabilityId::Resource("skin")
        );
        assert_eq!(
            parse_capability("action:vosu.osu.beatmap.import").unwrap(),
            CapabilityId::Action("vosu.osu.beatmap.import")
        );
        assert_eq!(
            parse_capability("game:osu").unwrap(),
            CapabilityId::Game("osu")
        );
    }

    #[test]
    fn rejects_unknown_prefix_empty_suffix_and_bad_game_id() {
        assert!(parse_capability("field:").is_err());
        assert!(parse_capability("osu.combo").is_err());
        assert!(parse_capability("game:Osu").is_err());
        assert!(parse_capability("game:").is_err());
        assert!(parse_capability("game:o.su").is_err());
    }

    #[test]
    fn manifest_reserved_sessions_rejected_for_every_capability_class() {
        for prefix in ["field", "resource", "action", "game"] {
            for suffix in ["vosu.sessions", "vosu.sessions.child"] {
                assert!(parse_capability(&format!("{prefix}:{suffix}")).is_err());
            }
        }
        assert!(parse_capability("resource:vosu.sessions2.kind").is_ok());
        assert!(parse_capability("action:vosu.sessions2.run").is_ok());
    }

    #[test]
    fn rejects_one_segment_field_namespace() {
        assert!(parse_capability("field:vosu").is_err());
        assert!(parse_capability("field:acme").is_err());
    }

    #[test]
    fn rejects_reserved_sessions_namespace() {
        assert!(parse_capability("field:vosu.sessions").is_err());
        assert!(parse_capability("field:vosu.sessions.focused").is_err());
    }

    #[test]
    fn rejects_malformed_segments() {
        // Uppercase and over-length segments never parse, under any
        // prefix: there is no grammar (dash or camelCase) that accepts
        // them.
        assert!(parse_capability("field:vosu.play.c-ombo").is_err());
        assert!(parse_capability("action:acme.game.doIt").is_err());
        assert!(parse_capability(&format!("resource:{}", "a".repeat(63))).is_err());
    }

    #[test]
    fn rejects_action_id_with_too_few_segments() {
        assert!(parse_capability("action:acme").is_err());
        assert!(parse_capability("action:acme.game").is_err());
        assert!(parse_capability("action:acme.game.import").is_ok());
    }

    #[test]
    fn covers_matches_equal_or_ancestor_paths_only() {
        assert!(covers("field:vosu.osu", "field:vosu.osu"));
        assert!(covers("field:vosu.osu", "field:vosu.osu.play.pp.current"));
        assert!(!covers("field:vosu.osu.play.pp.current", "field:vosu.osu"));
        // "vosu.os" is a string prefix of "vosu.osu" but not a dotted
        // ancestor path of it: the next character is "u", not ".".
        assert!(!covers("field:vosu.os", "field:vosu.osu.play"));
    }

    #[test]
    fn field_to_pointer_converts_dotted_path_or_bare_namespace() {
        assert_eq!(
            field_to_pointer("vosu.osu", "field:vosu.osu.play.pp").as_deref(),
            Some("/play/pp")
        );
        assert_eq!(
            field_to_pointer("vosu.osu", "field:vosu.osu").as_deref(),
            Some("")
        );
        assert_eq!(field_to_pointer("vosu.osu", "field:vosu.play.combo"), None);
        assert_eq!(
            field_to_pointer("vosu.osu", "resource:chart.background"),
            None
        );
    }
}

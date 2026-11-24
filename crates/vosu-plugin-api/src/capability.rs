//! Capability id grammar: `field:`/`resource:`/`action:`/`game:` prefixes
//! over a dotted path (`docs/v2/naming.md` "Capability id grammar",
//! `docs/v2/catalog-v2.md` "Capability fields"). A capability id's
//! namespace boundary is ambiguous from the string alone — resolving it
//! needs the full installed/catalogued namespace set, which is M6a's
//! "Namespace and kind parsing" in full (`docs/v2/catalog-v2.md:150-187`).
//! This module only splits the reserved prefix from its dotted suffix and
//! checks that suffix's own segment grammar; it never locates a namespace
//! boundary inside it.

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

/// One dotted segment of a `field:`/`resource:`/`action:` suffix. Such a
/// segment is either a namespace/kind/action-name segment
/// (`[a-z0-9][a-z0-9-]{0,31}`, `docs/v2/naming.md` "Kinds") or, below a
/// `field:` namespace, a camelCase schema-key segment
/// (`docs/v2/state-schema.md` "Naming"); without the namespace boundary
/// this module cannot tell which a given segment is (module doc comment),
/// so it accepts either grammar's segments here.
fn valid_suffix_segment(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return false;
    }
    s.len() <= 63 && chars.all(|c| c.is_ascii_alphanumeric() || c == '-')
}

fn valid_suffix(suffix: &str) -> bool {
    !suffix.is_empty() && suffix.split('.').all(valid_suffix_segment)
}

/// `^[a-z][a-z0-9-]{0,31}$` (`docs/v2/naming.md` "Game ids").
fn valid_game_id(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase())
        && s.len() <= 32
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Parses one capability id into its prefix and suffix, checking the
/// suffix's own grammar (`docs/v2/naming.md` "Capability id grammar",
/// "Kinds", "Game ids"). Namespace-boundary resolution is out of scope
/// (module doc comment) — the caller (`crate::manifest`, and eventually
/// M6a's resolver) checks anything that needs it.
pub fn parse_capability(id: &str) -> Result<CapabilityId<'_>, CapabilityError> {
    let err = || CapabilityError(id.to_string());
    if let Some(suffix) = id.strip_prefix("field:") {
        return valid_suffix(suffix)
            .then_some(CapabilityId::Field(suffix))
            .ok_or_else(err);
    }
    if let Some(suffix) = id.strip_prefix("resource:") {
        return valid_suffix(suffix)
            .then_some(CapabilityId::Resource(suffix))
            .ok_or_else(err);
    }
    if let Some(suffix) = id.strip_prefix("action:") {
        return valid_suffix(suffix)
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

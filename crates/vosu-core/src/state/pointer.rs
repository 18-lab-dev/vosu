//! RFC 6901 JSON Pointers below a namespace (abi:154; `docs/v2/plugin-abi.md`
//! §3): at most 512 bytes of wire text and 32 decoded tokens, matching 1.x
//! subscription pointers (`src/server/v2_subscription.rs:237-247`). The
//! empty pointer selects the whole namespace/value.

use std::fmt;

/// At most 512 bytes of wire text (abi:154).
const MAX_BYTES: usize = 512;
/// At most 32 decoded reference tokens (abi:154).
const MAX_TOKENS: usize = 32;

/// A decoded, validated RFC 6901 pointer. `Pointer::root()` (`""`) selects
/// the whole document; `/internal/paths` decodes to `["internal", "paths"]`.
#[derive(Clone, Default, PartialEq, Eq, Hash, Debug)]
pub struct Pointer(Box<[Box<str>]>);

/// Every way a pointer string can fail to decode (abi:154).
#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum PointerError {
    #[error("pointer exceeds the 512-byte wire bound (abi:154): {0:?}")]
    TooLong(String),
    #[error("pointer has more than 32 decoded tokens (abi:154): {0:?}")]
    TooManyTokens(String),
    #[error("a non-empty pointer must start with '/': {0:?}")]
    MissingLeadingSlash(String),
    #[error("invalid '~' escape in pointer token {0:?}")]
    InvalidEscape(String),
}

impl Pointer {
    /// The empty pointer: selects the whole namespace or value.
    pub fn root() -> Self {
        Self(Box::new([]))
    }

    /// Parses an RFC 6901 pointer string: `""` is the root; otherwise every
    /// reference token is `/`-prefixed, with `~1`/`~0` escaping `/`/`~`.
    pub fn parse(s: &str) -> Result<Self, PointerError> {
        if s.len() > MAX_BYTES {
            return Err(PointerError::TooLong(s.to_string()));
        }
        if s.is_empty() {
            return Ok(Self::root());
        }
        if !s.starts_with('/') {
            return Err(PointerError::MissingLeadingSlash(s.to_string()));
        }
        let mut tokens = Vec::new();
        for raw in s[1..].split('/') {
            tokens.push(unescape(raw)?);
        }
        if tokens.len() > MAX_TOKENS {
            return Err(PointerError::TooManyTokens(s.to_string()));
        }
        Ok(Self(tokens.into_boxed_slice()))
    }

    /// The decoded reference tokens, in order.
    pub fn tokens(&self) -> &[Box<str>] {
        &self.0
    }

    /// Whether this is the empty (whole-document) pointer.
    pub fn is_root(&self) -> bool {
        self.0.is_empty()
    }

    /// Whether `self`'s tokens are a prefix of (or equal to) `other`'s.
    pub fn is_prefix_of(&self, other: &Self) -> bool {
        tokens_prefix(&self.0, &other.0)
    }

    /// Whether `self` and `other` name overlapping subtrees: one is a
    /// prefix of the other, in either direction (MJ).
    pub fn overlaps(&self, other: &Self) -> bool {
        self.is_prefix_of(other) || other.is_prefix_of(self)
    }
}

/// Whether `a` is a prefix of (or equal to) `b`, token by token.
pub(super) fn tokens_prefix(a: &[Box<str>], b: &[Box<str>]) -> bool {
    a.len() <= b.len() && a.iter().zip(b.iter()).all(|(x, y)| x == y)
}

impl fmt::Display for Pointer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for token in &self.0 {
            f.write_str("/")?;
            for ch in token.chars() {
                match ch {
                    '~' => f.write_str("~0")?,
                    '/' => f.write_str("~1")?,
                    other => write!(f, "{other}")?,
                }
            }
        }
        Ok(())
    }
}

/// Decodes one `~0`/`~1`-escaped reference token (`~0` → `~`, `~1` → `/`).
fn unescape(raw: &str) -> Result<Box<str>, PointerError> {
    if !raw.contains('~') {
        return Ok(Box::from(raw));
    }
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(ch) = chars.next() {
        if ch != '~' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('0') => out.push('~'),
            Some('1') => out.push('/'),
            _ => return Err(PointerError::InvalidEscape(raw.to_string())),
        }
    }
    Ok(out.into_boxed_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_is_empty() {
        let p = Pointer::parse("").unwrap();
        assert!(p.is_root());
        assert_eq!(p.tokens(), &[] as &[Box<str>]);
        assert_eq!(p.to_string(), "");
    }

    #[test]
    fn decodes_and_reencodes_escapes() {
        let p = Pointer::parse("/a~1b/c~0d").unwrap();
        assert_eq!(
            p.tokens().iter().map(AsRef::as_ref).collect::<Vec<_>>(),
            vec!["a/b", "c~d"]
        );
        assert_eq!(p.to_string(), "/a~1b/c~0d");
    }

    #[test]
    fn rejects_missing_leading_slash() {
        assert!(matches!(
            Pointer::parse("internal"),
            Err(PointerError::MissingLeadingSlash(_))
        ));
    }

    #[test]
    fn rejects_bad_escape() {
        assert!(matches!(
            Pointer::parse("/a~2b"),
            Err(PointerError::InvalidEscape(_))
        ));
    }

    #[test]
    fn rejects_too_many_tokens() {
        let s = "/x".repeat(MAX_TOKENS + 1);
        assert!(matches!(
            Pointer::parse(&s),
            Err(PointerError::TooManyTokens(_))
        ));
    }

    #[test]
    fn rejects_too_long() {
        let s = format!("/{}", "x".repeat(MAX_BYTES));
        assert!(matches!(Pointer::parse(&s), Err(PointerError::TooLong(_))));
    }

    #[test]
    fn prefix_and_overlap() {
        let root = Pointer::parse("/internal").unwrap();
        let child = Pointer::parse("/internal/paths/stable").unwrap();
        let sibling = Pointer::parse("/beatmap").unwrap();
        assert!(root.is_prefix_of(&child));
        assert!(!child.is_prefix_of(&root));
        assert!(root.overlaps(&child));
        assert!(child.overlaps(&root));
        assert!(!root.overlaps(&sibling));
    }

    proptest::proptest! {
        #[test]
        fn prop_pointer_parse_roundtrip(
            tokens in proptest::collection::vec("[ -~]{0,6}", 0..5)
        ) {
            let boxed: Vec<Box<str>> = tokens.iter().map(|s| Box::from(s.as_str())).collect();
            // Encode independently of `Pointer`'s own `Display` (an oracle,
            // not a round-trip through the type under test): `~` -> `~0`,
            // `/` -> `~1`, everything else verbatim.
            let encoded = tokens.iter().fold(String::new(), |mut s, token| {
                s.push('/');
                for ch in token.chars() {
                    match ch {
                        '~' => s.push_str("~0"),
                        '/' => s.push_str("~1"),
                        other => s.push(other),
                    }
                }
                s
            });
            let parsed = Pointer::parse(&encoded).expect("round-trip parses");
            proptest::prop_assert_eq!(parsed.tokens(), boxed.as_slice());
        }
    }
}

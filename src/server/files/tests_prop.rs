//! Property tests for the root-confined request-path resolution
//! (`decode_request_path` / `safe_child`, ADR 0005).

use proptest::prelude::*;

use super::*;
use vosu_proptest::vosu_proptest_config;

proptest! {
    #![proptest_config(vosu_proptest_config())]

    /// Percent-decoding never panics on arbitrary request text: a malformed
    /// escape or embedded NUL is always a clean `Err`, never a crash.
    #[test]
    fn prop_decode_request_path_never_panics(raw in ".{0,64}") {
        let _ = decode_request_path(&raw);
    }

    /// `decode_request_path` never panics on arbitrary bytes either, not
    /// just valid Unicode text: a malformed or non-UTF-8 percent sequence is
    /// always a clean `Err`, never a crash.
    #[test]
    fn prop_decode_request_path_never_panics_on_arbitrary_bytes(
        raw in prop::collection::vec(any::<u8>(), 0..64)
    ) {
        // `decode_request_path` takes `&str`; lossily converting mirrors how
        // callers only ever reach it with a request's (possibly mangled)
        // UTF-8 text in the first place, while still exercising the full
        // byte space of what that text can contain.
        let text = String::from_utf8_lossy(&raw).into_owned();
        let _ = decode_request_path(&text);
    }

    /// `safe_child` never panics on arbitrary decoded request text, and
    /// whenever it accepts one, the resolved relative path never escapes the
    /// root: no parent-directory, root, or prefix component survives, and
    /// joining it back onto the root stays textually under the root.
    #[test]
    fn prop_safe_child_accepted_paths_never_escape_the_root(decoded in ".{0,64}") {
        let root = tempfile::tempdir().unwrap();
        let result = safe_child(root.path(), &decoded);
        if let Ok(child) = result {
            prop_assert_eq!(&child.root, root.path());
            for component in child.relative.components() {
                prop_assert!(matches!(component, Component::Normal(_) | Component::CurDir));
            }
            prop_assert!(!child.relative.is_absolute());
            let joined = child.root.join(&child.relative);
            prop_assert!(joined.starts_with(&child.root));
        }
    }

    /// Any request whose decoded text would place a literal `..`, `.`-only
    /// root escape, or absolute path in a `Normal` component is always
    /// refused: `safe_child` must reject every `..`/root/prefix component
    /// rather than silently drop or normalize it away.
    #[test]
    fn prop_safe_child_refuses_every_parent_or_absolute_component(
        segments in prop::collection::vec("[a-zA-Z0-9_]{1,8}", 0..4),
        escape_at in 0usize..4,
        absolute in any::<bool>(),
    ) {
        let mut parts: Vec<String> = segments;
        let index = escape_at.min(parts.len());
        parts.insert(index, "..".to_string());
        let mut decoded = parts.join("/");
        if absolute {
            decoded.insert(0, '/');
        }
        prop_assert!(matches!(
            safe_child(tempfile::tempdir().unwrap().path(), &decoded),
            Err(SafePathError::InvalidComponent)
        ));
    }

    /// A request built only from ordinary segment characters and `/`
    /// (no `.`, `..`, leading `/`, or NUL) always round-trips through
    /// `decode_request_path` unchanged and is always accepted by
    /// `safe_child`, with every segment preserved in order.
    #[test]
    fn prop_ordinary_segments_round_trip_and_are_accepted(
        segments in prop::collection::vec("[a-zA-Z0-9_-]{1,8}", 1..5)
    ) {
        let joined = segments.join("/");
        prop_assert_eq!(decode_request_path(&joined).unwrap(), joined.clone());
        let root = tempfile::tempdir().unwrap();
        let child = safe_child(root.path(), &joined).unwrap();
        let rebuilt: Vec<_> = child
            .relative
            .components()
            .map(|component| component.as_os_str().to_string_lossy().into_owned())
            .collect();
        prop_assert_eq!(rebuilt, segments);
    }
}

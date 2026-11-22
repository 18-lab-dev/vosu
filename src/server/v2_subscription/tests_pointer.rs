//! JSON Pointer parsing, compilation, and traversal: examples and properties.

use proptest::prelude::*;

use super::*;
use vosu_proptest::vosu_proptest_config;

#[test]
fn pointer_syntax_and_boundaries_follow_rfc6901() {
    assert_eq!(decode_pointer(""), Some(vec![]));
    assert_eq!(decode_pointer("/"), Some(vec![String::new()]));
    assert_eq!(
        decode_pointer("/日本語/~0/~1"),
        Some(vec!["日本語".into(), "~".into(), "/".into()])
    );
    assert!(decode_pointer("x").is_none());
    assert!(decode_pointer("/~").is_none());
    assert!(decode_pointer("/~2").is_none());

    assert!(compile_pointers(vec!["/a".into()]).is_ok());
    assert!(compile_pointers((0..256).map(|id| format!("/{id}")).collect()).is_ok());
    assert_eq!(
        compile_pointers(Vec::new()),
        Err(SubscriptionRequestError::InvalidRequest)
    );
    assert_eq!(
        compile_pointers((0..257).map(|id| format!("/{id}")).collect()),
        Err(SubscriptionRequestError::InvalidRequest)
    );
    assert!(compile_pointers(vec![format!("/{}", "a".repeat(511))]).is_ok());
    assert_eq!(
        compile_pointers(vec![format!("/{}", "a".repeat(512))]),
        Err(SubscriptionRequestError::InvalidRequest)
    );
    assert!(compile_pointers(vec![format!("/{}", vec!["a"; 32].join("/"))]).is_ok());
    assert_eq!(
        compile_pointers(vec![format!("/{}", vec!["a"; 33].join("/"))]),
        Err(SubscriptionRequestError::InvalidRequest)
    );
}

#[test]
fn decoded_duplicates_and_ancestor_overlaps_are_rejected() {
    for pointers in [
        vec!["/a".into(), "/a".into()],
        vec!["/a~1b".into(), "/a~1b/c".into()],
        vec!["".into(), "/a".into()],
        vec!["/a/b".into(), "/a".into()],
    ] {
        assert_eq!(
            compile_pointers(pointers),
            Err(SubscriptionRequestError::InvalidPointer)
        );
    }
}

#[test]
fn traversal_distinguishes_object_keys_array_indices_null_and_absence() {
    let document = json!({"": 1, "0": "object", "array": [null, "one"], "01": 4, "-": 5});
    assert_eq!(traverse(&document, &[String::new()]), Some(&json!(1)));
    assert_eq!(traverse(&document, &["0".into()]), Some(&json!("object")));
    assert_eq!(traverse(&document, &["01".into()]), Some(&json!(4)));
    assert_eq!(traverse(&document, &["-".into()]), Some(&json!(5)));
    assert_eq!(
        traverse(&document, &["array".into(), "0".into()]),
        Some(&Value::Null)
    );
    assert_eq!(
        traverse(&document, &["array".into(), "1".into()]),
        Some(&json!("one"))
    );
    assert_eq!(traverse(&document, &["array".into(), "01".into()]), None);
    assert_eq!(traverse(&document, &["array".into(), "-".into()]), None);
    assert_eq!(parse_array_index("184467440737095516160000"), None);
}

#[test]
fn object_lookup_agrees_below_and_above_the_linear_search_size() {
    for size in 0..=LINEAR_KEY_SEARCH_MAX + 4 {
        let mut object = serde_json::Map::new();
        for id in 0..size {
            // Same-length keys and a shared prefix make every miss reach the byte comparison.
            let key = if id == 0 {
                String::new()
            } else {
                format!("k{id:02}")
            };
            object.insert(key, json!({"id": id}));
        }
        let document = Value::Object(object);
        for id in 0..size {
            let key = if id == 0 {
                String::new()
            } else {
                format!("k{id:02}")
            };
            assert_eq!(
                traverse(&document, &[key.clone(), "id".into()]),
                Some(&json!(id)),
                "size {size} key {key:?}"
            );
        }
        for missing in ["k99", "k0", "K01", "k01 ", "id"] {
            assert_eq!(
                traverse(&document, &[missing.into()]),
                None,
                "size {size} {missing:?}"
            );
        }
    }
}

/// RFC 6901 escaping: `~` becomes `~0`, `/` becomes `~1`.
fn encode_token(token: &str) -> String {
    token.replace('~', "~0").replace('/', "~1")
}

fn encode_pointer(tokens: &[String]) -> String {
    if tokens.is_empty() {
        return String::new();
    }
    let mut pointer = String::new();
    for token in tokens {
        pointer.push('/');
        pointer.push_str(&encode_token(token));
    }
    pointer
}

fn token() -> impl Strategy<Value = String> {
    // Every printable-ish character class `decode_token` must handle,
    // including the two escaped characters themselves and plain ASCII.
    "[a-zA-Z0-9~/_. -]{0,8}"
}

proptest! {
    #![proptest_config(vosu_proptest_config())]

    /// Any JSON Pointer built by escaping arbitrary tokens the documented
    /// way decodes back to exactly those tokens, in order: encode and decode
    /// are true inverses of each other for every token shape, not just the
    /// ones existing unit tests happen to cover.
    #[test]
    fn prop_decode_pointer_round_trips_encoded_tokens(
        tokens in prop::collection::vec(token(), 0..6)
    ) {
        let pointer = encode_pointer(&tokens);
        prop_assert_eq!(decode_pointer(&pointer), Some(tokens));
    }

    /// `decode_pointer`/`decode_token` never panic on arbitrary request
    /// text, valid pointer syntax or not.
    #[test]
    fn prop_decode_pointer_never_panics(raw in ".{0,64}") {
        let _ = decode_pointer(&raw);
    }

    /// `decode_pointer`/`decode_token` never panic on arbitrary bytes
    /// either, including invalid UTF-8 continuations once lossily decoded
    /// to the `&str` every real caller hands them.
    #[test]
    fn prop_decode_pointer_never_panics_on_arbitrary_bytes(
        raw in prop::collection::vec(any::<u8>(), 0..64)
    ) {
        let text = String::from_utf8_lossy(&raw).into_owned();
        let _ = decode_pointer(&text);
    }

    /// A pointer is always refused (not silently truncated or misparsed)
    /// when a `~` is not immediately followed by `0` or `1`: every other
    /// escape character after `~` is invalid per RFC 6901.
    #[test]
    fn prop_decode_pointer_refuses_an_invalid_escape(
        prefix in "[a-zA-Z0-9]{0,4}",
        suffix in "[a-zA-Z0-9]{0,4}",
        bad_escape in prop::char::any().prop_filter("must not be a valid escape", |c| {
            *c != '0' && *c != '1'
        }),
    ) {
        let mut pointer = String::from("/");
        pointer.push_str(&prefix);
        pointer.push('~');
        pointer.push(bad_escape);
        pointer.push_str(&suffix);
        prop_assert_eq!(decode_pointer(&pointer), None);
    }

    /// A pointer ending in a bare trailing `~` (no following character to
    /// decide the escape) is always refused, never panics or silently drops
    /// the trailing tilde.
    #[test]
    fn prop_decode_pointer_refuses_a_trailing_tilde(prefix in "[a-zA-Z0-9]{0,8}") {
        let mut pointer = String::from("/");
        pointer.push_str(&prefix);
        pointer.push('~');
        prop_assert_eq!(decode_pointer(&pointer), None);
    }

    /// Every decoded pointer, when accepted, re-encodes back to character-
    /// for-character the same pointer string: `decode_pointer` never
    /// collapses two different valid pointers to the same token sequence.
    #[test]
    fn prop_decode_pointer_accepted_tokens_re_encode_to_the_same_pointer(
        tokens in prop::collection::vec(token(), 0..6)
    ) {
        let pointer = encode_pointer(&tokens);
        if let Some(decoded) = decode_pointer(&pointer) {
            prop_assert_eq!(encode_pointer(&decoded), pointer);
        }
    }
}

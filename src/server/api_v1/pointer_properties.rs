//! Independent token oracle across HTTP decoding, API demand plans and wire selection.
use crate::server::subscription_wire::Selection;
use proptest::prelude::*;
use vosu_core::api::ApiPlan;

fn encode(tokens: &[String]) -> String {
    let mut pointer = String::new();
    for token in tokens {
        pointer.push('/');
        for ch in token.chars() {
            match ch {
                '~' => pointer.push_str("~0"),
                '/' => pointer.push_str("~1"),
                other => pointer.push(other),
            }
        }
    }
    pointer
}

proptest! {
    #![proptest_config(vosu_proptest::vosu_proptest_config())]

    #[test]
    fn prop_api_pointer_http_wire_and_core_share_decoded_tokens(
        tokens in proptest::collection::vec("[a-z/~ +é]{0,8}", 0..12)
    ) {
        let pointer = encode(&tokens);
        let form: String = url::form_urlencoded::byte_serialize(pointer.as_bytes()).collect();
        let decoded = super::router::decode_form_component(&form).unwrap();
        prop_assert_eq!(&decoded, &pointer);
        let wire = Selection::compile(vec![decoded.clone()]).unwrap();
        prop_assert_eq!(&wire.pointers()[0], &tokens);
        let api = ApiPlan::new(&[decoded]).unwrap();
        let core: Vec<_> = api.pointers()[0].tokens().iter().map(|v| v.as_ref().to_owned()).collect();
        prop_assert_eq!(core, tokens);
    }

    #[test]
    fn prop_api_pointer_overlap_uses_decoded_token_boundaries(
        prefix in proptest::collection::vec("[a-z/~]{1,6}", 1..8),
        leaf in "[a-z/~]{1,6}"
    ) {
        let parent = encode(&prefix);
        let mut child_tokens = prefix;
        child_tokens.push(leaf);
        let child = encode(&child_tokens);
        prop_assert!(ApiPlan::new(&[&parent, &child]).is_err());
        prop_assert!(ApiPlan::new(&[&child, &parent]).is_err());
        prop_assert!(Selection::compile(vec![parent.clone(), child]).is_err());
        // A textual prefix inside the final token is not an ancestor.
        let sibling = format!("{parent}x");
        prop_assert!(ApiPlan::new(&[&parent, &sibling]).is_ok());
        prop_assert!(Selection::compile(vec![parent, sibling]).is_ok());
    }

    #[test]
    fn prop_api_pointer_exact_byte_and_token_limits(bytes in 0usize..530, tokens in 1usize..36) {
        let text = format!("/{}", "a".repeat(bytes));
        prop_assert_eq!(ApiPlan::new(&[&text]).is_ok(), text.len() <= 512);
        prop_assert_eq!(Selection::compile(vec![text.clone()]).is_ok(), text.len() <= 512);
        let deep = "/a".repeat(tokens);
        prop_assert_eq!(ApiPlan::new(&[&deep]).is_ok(), tokens <= 32);
        prop_assert_eq!(Selection::compile(vec![deep]).is_ok(), tokens <= 32);
    }

    #[test]
    fn prop_api_pointer_rejects_non_rfc_escapes(prefix in "[a-z]{0,16}", suffix in "[a-z]{0,16}", bad in "[2-9a-z]") {
        let pointer = format!("/{prefix}~{bad}{suffix}");
        prop_assert!(ApiPlan::new(&[&pointer]).is_err());
        prop_assert!(Selection::compile(vec![pointer]).is_err());
    }
}

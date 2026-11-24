//! Exact `/api/catalog/*` grammar and single segment decoding.
use super::*;
use proptest::prelude::*;
use vosu_proptest::vosu_proptest_config;

/// Component encoding as the dashboard's `encodeURIComponent` produces it.
fn encode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
                char::from(byte).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}

#[test]
fn segments_decode_exactly_once_and_reject_malformed_escapes() {
    assert_eq!(decode_segment("acme%2Fplugins").as_deref(), Some("acme/plugins"));
    assert_eq!(
        decode_segment("https%3A%2F%2Fcatalog.example%2Fcatalog-v2.json").as_deref(),
        Some("https://catalog.example/catalog-v2.json")
    );
    // Double encoding stays one level encoded: a different exact identity.
    assert_eq!(decode_segment("acme%252Fplugins").as_deref(), Some("acme%2Fplugins"));
    for malformed in ["", "%", "%2", "%zz", "%+f", "%-1", "acme%2", "%C3%28", "%FF"] {
        assert_eq!(decode_segment(malformed), None, "{malformed:?}");
    }
}

#[test]
fn routes_are_exact_and_a_raw_slash_never_splits_a_source() {
    assert_eq!(
        route("/api/catalog/sources/acme%2Fplugins/credential").map(|r| r.0),
        Some(Route::Credential("acme/plugins".into()))
    );
    assert_eq!(
        route("/api/catalog/sources/acme%252Fplugins/trust").map(|r| r.0),
        Some(Route::Trust("acme%2Fplugins".into()))
    );
    for near in [
        "/api/catalog",
        "/api/catalog/",
        "/api/catalog/sources/",
        "/api/catalog/sources/acme/plugins/credential",
        "/api/catalog/sources//credential",
        "/api/catalog/sources/acme%2Fplugins/credential/",
        "/api/catalog/sources/acme%2Fplugins/credentials",
        "/api/catalog/sources/acme%2Fplugins",
        "/api/catalog/sources/%zz/trust",
        "/api/catalog/statusx",
        "/api/catalog/status/",
        "/api/catalog/plans/abc",
        "/api/catalog/plans/abc/confirm/",
        "/api/catalog/jobs/",
        "/api/catalog/jobs/a/b",
        "/api/catalog/uninstall/preview/x",
        "/api/catalog/offers/x/accept/now",
        "/api/catalog/Install",
    ] {
        assert_eq!(route(near).map(|r| r.0), None, "{near}");
    }
    assert_eq!(route("/api/catalog/install").map(|r| r.1), Some(POST));
    assert_eq!(route("/api/catalog/status").map(|r| r.1), Some(GET));
}

proptest! {
    #![proptest_config(vosu_proptest_config())]

    /// Any exact source string survives one component encoding and one
    /// decoding unchanged, and the encoded segment never contains a raw `/`.
    #[test]
    fn prop_catalog_segment_round_trips_one_component_encoding(source in ".{1,64}") {
        let encoded = encode(&source);
        prop_assert!(!encoded.contains('/'));
        prop_assert_eq!(decode_segment(&encoded), Some(source.clone()));
        let path = format!("/api/catalog/sources/{encoded}/credential");
        prop_assert_eq!(route(&path).map(|r| r.0), Some(Route::Credential(source)));
    }

    /// Decoding never panics, and never decodes twice: re-encoding the `%`
    /// of an encoded segment yields the encoded text, not the original.
    #[test]
    fn prop_catalog_segment_is_never_decoded_twice(source in "[a-z/]{1,24}") {
        let twice = encode(&encode(&source));
        prop_assert_eq!(decode_segment(&twice), Some(encode(&source)));
    }
}

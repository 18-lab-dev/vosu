use super::*;

#[test]
fn api_v1_http_queries_are_strict_and_decode_once() {
    for query in [
        "pointer=/a&pointer=/b",
        "unknown=/a",
        "pointer=/%GG",
        "pointer=/%FF",
        "pointer=/%",
    ] {
        assert!(pointer_query(Some(query)).is_err(), "{query}");
    }
    assert!(pointer_query(Some("pointer=not-a-pointer")).is_err());
    assert!(pointer_query(None).unwrap().is_none());
    let pointer = pointer_query(Some("pointer=%2Fa%252Fb+c"))
        .unwrap()
        .unwrap();
    assert_eq!(pointer.tokens().len(), 1);
    assert_eq!(pointer.tokens()[0].as_ref(), "a%2Fb c");
    assert!(pointer_query(Some("pointer=")).unwrap().unwrap().is_root());
}

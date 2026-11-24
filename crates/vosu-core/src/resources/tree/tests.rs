use super::*;
use vosu_plugin_api::TreeEntry;
fn entry(path: &str) -> TreeEntry {
    TreeEntry {
        path: path.into(),
        size: 17,
        hash: None,
    }
}
fn index(entries: Vec<TreeEntry>, truncated: bool) -> Result<TreeIndex, ResourceRequestError> {
    TreeIndex::validate(entries, truncated, "revision-7".into(), &["image".into()])
}
#[test]
fn canonical_membership_type_hash_and_truncation() {
    let tree = index(vec![entry("Folder/Image.PNG"), entry("script.html")], true).unwrap();
    assert!(tree.truncated);
    assert_eq!(
        tree.member("Folder/Image.PNG").unwrap().media_type,
        "image/png"
    );
    assert_eq!(
        tree.member("script.html").unwrap().media_type,
        "application/octet-stream"
    );
    assert!(tree.member("fallback.png").is_err());
    assert!(tree.member("folder/image.png").is_err());
    let json = serde_json::to_value(&tree).unwrap();
    assert!(json["entries"][0].get("hash").is_none());
    assert_eq!(json["truncated"], true);
    assert_ne!(
        tree.etag,
        index(vec![entry("Folder/Image.PNG"), entry("script.html")], false)
            .unwrap()
            .etag
    );
}
#[test]
fn rejects_noncanonical_duplicates_and_oversize_frames() {
    for path in [
        "../secret",
        "a/../b",
        "/absolute",
        "C:/file",
        "a\\b",
        "a//b",
        "./a",
        "a/",
        "a.",
        "a ",
        "",
        "a\0b",
    ] {
        assert!(
            index(vec![entry(path)], false).is_err(),
            "accepted {path:?}"
        );
    }
    assert!(index(vec![entry("same.png"), entry("same.png")], false).is_err());
    assert!(
        index(
            vec![TreeEntry {
                hash: Some("x".repeat(1_048_576)),
                ..entry("image.png")
            }],
            true
        )
        .is_err()
    );
    assert!(
        index(
            (0..=super::super::index::MAX_TREE_ENTRIES)
                .map(|n| entry(&format!("{n}.png")))
                .collect(),
            true
        )
        .is_err()
    );
    let first = index(vec![entry("a.png")], false).unwrap();
    let mut changed = entry("a.png");
    changed.size += 1;
    assert_ne!(first.etag, index(vec![changed], false).unwrap().etag);
}
proptest::proptest! {
    #![proptest_config(vosu_proptest::vosu_proptest_config())]
    #[test]
    fn prop_tree_path_safe_segments_round_trip(parts in proptest::collection::vec("[a-zA-Z0-9_-]{1,16}", 1..12)) {
        let path = parts.join("/");
        proptest::prop_assert!(super::super::path::validate(&path).is_ok());
        let tree = index(vec![entry(&path)], false).unwrap();
        proptest::prop_assert_eq!(&tree.member(&path).unwrap().path, &path);
    }
    #[test]
    fn prop_tree_path_parent_is_rejected(prefix in "[a-z]{1,16}", suffix in "[a-z]{1,16}") {
        let path = format!("{prefix}/../{suffix}");
        proptest::prop_assert!(super::super::path::validate(&path).is_err());
    }
}

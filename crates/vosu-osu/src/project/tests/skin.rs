use super::o5_tests::{Collect, facts, sample};
use super::*;
use serde_json::json;
#[test]
fn stable_skin_files_reflects_directory_and_changes_only_with_inputs() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("Skins/first")).unwrap();
    let mut s = sample();
    let r = Arc::make_mut(&mut s.regular);
    r.global.game_folder = dir.path().to_string_lossy().into_owned();
    r.global.skin_folder = "first".into();
    r.settings.values = serde_json::from_value(json!({"skin":{"name":"first"}})).unwrap();
    let f = facts(ClientType::Stable);
    let mut cache = skin::SkinCache::default();
    let mut out = Collect::default();
    cache.project(&f, &s, &mut out);
    assert_eq!(out.get(OsuPointer::OsuSkinFiles), true);
    assert_eq!(out.get(OsuPointer::OsuSkinBase), "legacy");
    dir.close().unwrap();
    out.0.clear();
    cache.project(&f, &s, &mut out);
    assert_eq!(out.get(OsuPointer::OsuSkinFiles), true);
    assert!(cache.take_error().is_none());
    Arc::make_mut(&mut s.regular).global.skin_folder = "second".into();
    out.0.clear();
    cache.project(&f, &s, &mut out);
    assert_eq!(out.get(OsuPointer::OsuSkinFiles), false);
    assert!(cache.take_error().is_some());
    out.0.clear();
    cache.project(&f, &s, &mut out);
    assert!(cache.take_error().is_none());
    let other = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(other.path().join("Skins/second")).unwrap();
    Arc::make_mut(&mut s.regular).global.game_folder = other.path().to_string_lossy().into_owned();
    out.0.clear();
    cache.project(&f, &s, &mut out);
    assert_eq!(
        out.get(OsuPointer::OsuSkinFiles),
        true,
        "root changes invalidate the cache"
    );
    assert!(cache.take_error().is_none());
    let mut last = LastValues::default();
    let mut writer = vosu_plugin_api::PublishBatchWriter::new();
    writer.begin(71);
    cache.project(&f, &s, &mut DiffSink::new(&mut last, &mut writer));
    writer.begin(71);
    let (_, allocations) = crate::test_alloc::allocations_during(|| {
        cache.project(&f, &s, &mut DiffSink::new(&mut last, &mut writer));
    });
    assert_eq!(allocations, 0);
    assert!(writer.is_empty());
}
#[test]
fn lazer_skin_metadata_matches_current_skin() {
    let mut s = sample();
    Arc::make_mut(&mut s.regular).global.current_skin =
        Some(Arc::new(vosu_osu_model::memory::CurrentSkin {
            name: "skin".into(),
            builtin: Some("argon".into()),
            base: Some("argon-pro".into()),
            files: Default::default(),
        }));
    let mut out = Collect::default();
    skin::SkinCache::default().project(&facts(ClientType::Lazer), &s, &mut out);
    assert_eq!(out.get(OsuPointer::OsuSkinBuiltin), "argon");
    assert_eq!(out.get(OsuPointer::OsuSkinBase), "argon-pro");
    assert_eq!(out.get(OsuPointer::OsuSkinVariant), "lazer");
    assert_eq!(out.get(OsuPointer::OsuSkinFiles), false);
}

use super::o5_tests::{Collect, facts, sample};
use super::*;
use serde_json::json;
#[test]
fn stable_skin_files_reflects_directory_and_changes_only_with_name() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = sample();
    let r = Arc::make_mut(&mut s.regular);
    r.global.skin_folder = dir.path().to_string_lossy().into_owned();
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
    Arc::make_mut(&mut s.regular).settings.values =
        serde_json::from_value(json!({"skin":{"name":"second"}})).unwrap();
    out.0.clear();
    cache.project(&f, &s, &mut out);
    assert_eq!(out.get(OsuPointer::OsuSkinFiles), false);
    assert!(cache.take_error().is_some());
    out.0.clear();
    cache.project(&f, &s, &mut out);
    assert!(cache.take_error().is_none());
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

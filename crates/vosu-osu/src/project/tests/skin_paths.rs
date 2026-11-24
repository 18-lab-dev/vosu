use super::o5_tests::{Collect, facts, sample};
use super::*;

#[test]
fn stable_skin_rejects_non_plain_names_and_relative_roots_before_io() {
    let dir = tempfile::tempdir().unwrap();
    let game = dir.path().join("game");
    std::fs::create_dir_all(game.join("Skins/nested/skin")).unwrap();
    std::fs::create_dir_all(game.join("outside")).unwrap();
    let absolute = game.join("outside").to_string_lossy().into_owned();
    let f = facts(ClientType::Stable);
    let mut snap = sample();
    let mut cache = skin::SkinCache::default();
    for name in [
        "../outside",
        "nested/skin",
        ".",
        "..",
        "",
        absolute.as_str(),
    ] {
        let r = Arc::make_mut(&mut snap.regular);
        r.global.game_folder = game.to_string_lossy().into_owned();
        r.global.skin_folder = name.into();
        let mut out = Collect::default();
        cache.project(&f, &snap, &mut out);
        assert_eq!(out.get(OsuPointer::OsuSkinFiles), false, "{name}");
        assert!(
            cache.take_error().is_none(),
            "invalid names must not attempt IO: {name}"
        );
    }
    let r = Arc::make_mut(&mut snap.regular);
    r.global.game_folder = "relative-game".into();
    r.global.skin_folder = "missing-skin".into();
    let mut out = Collect::default();
    cache.project(&f, &snap, &mut out);
    assert_eq!(out.get(OsuPointer::OsuSkinFiles), false);
    assert!(
        cache.take_error().is_none(),
        "relative roots must not attempt IO"
    );
}

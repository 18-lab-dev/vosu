#![expect(
    clippy::disallowed_methods,
    reason = "confinement fixtures create and mutate scratch files"
)]
mod support;
use support::{Case, MAP};
use vosu_adapter_upstream::{
    calculate::{PpQuery, ResultBody},
    files::{Outcome, Route},
};

fn bytes(case: &Case, route: Route, path: &str) -> Vec<u8> {
    match case.run(
        case.provider
            .resolve(route, path, &format!("/Songs/{path}")),
    ) {
        Outcome::File { file, .. } => case.run(file.read(0, 65536)).unwrap(),
        _ => panic!("expected declared-root file"),
    }
}
#[test]
fn declared_files_ignore_echo_roots_and_cover_current_browsable_and_skin_paths() {
    let case = Case::new(false);
    assert_eq!(bytes(&case, Route::Background, ""), b"stable-bg");
    assert_eq!(bytes(&case, Route::Audio, ""), b"0123456789");
    assert_eq!(bytes(&case, Route::Beatmap, ""), MAP);
    assert_eq!(bytes(&case, Route::Songs, "Set/bg.jpg"), b"stable-bg");
    assert_eq!(bytes(&case, Route::BeatmapPath, "Set/bg.jpg"), b"stable-bg");
    assert_eq!(bytes(&case, Route::Skin, "subdir/mixed.png"), b"skin-exact");
    assert!(
        matches!(case.run(case.provider.resolve(Route::Songs, "Set", "/Songs/Set?q=1")), Outcome::Redirect(location) if location == "/Songs/Set/?q=1")
    );
    assert!(
        matches!(case.run(case.provider.resolve(Route::Songs, "Set/", "/Songs/Set/")), Outcome::Directory { entries, truncated: false } if entries.len() == 3)
    );
    assert!(
        matches!(case.run(case.provider.resolve(Route::Skin, "SubDir/", "/files/skin/SubDir/")), Outcome::Directory { entries, .. } if entries.len() == 1)
    );
    assert!(matches!(
        case.run(case.provider.resolve(Route::Skin, "", "/files/skin/")),
        Outcome::Missing
    ));
}
#[test]
fn decoding_and_platform_spellings_never_escape_declared_roots() {
    let case = Case::new(false);
    std::fs::write(case.songs().join("%2e%2e"), b"decode-once").unwrap();
    assert_eq!(bytes(&case, Route::Songs, "%252e%252e"), b"decode-once");
    for spelling in [
        "../outside",
        "%2e%2e/outside",
        "..%5coutside",
        "%",
        "%ff",
        "C:/outside",
        "C:%5coutside",
        "%2fetc/passwd",
        "%5c%5chost%5cshare",
    ] {
        assert!(
            matches!(
                case.run(
                    case.provider
                        .resolve(Route::Songs, spelling, "/Songs/invalid")
                ),
                Outcome::Missing
            ),
            "{spelling}"
        );
    }
    #[cfg(windows)]
    for spelling in [
        "Set/bg.jpg:secret",
        "Set/bg.jpg.",
        "Set/bg.jpg%20",
        "Set/bg.jpg::$DATA",
    ] {
        assert!(
            matches!(
                case.run(
                    case.provider
                        .resolve(Route::Songs, spelling, "/Songs/invalid")
                ),
                Outcome::Missing
            ),
            "{spelling}"
        );
    }
}
#[test]
fn lazer_legacy_skin_is_missing_while_other_files_and_calculation_still_work() {
    let case = Case::new(true);
    assert_eq!(bytes(&case, Route::Background, ""), b"lazer-bg!");
    assert!(matches!(
        case.run(case.provider.resolve(
            Route::Skin,
            "SubDir/MiXeD.PNG",
            "/files/skin/SubDir/MiXeD.PNG"
        )),
        Outcome::Missing
    ));
    assert!(matches!(
        case.run(case.provider.calculate(PpQuery {
            path: Some("Set/map.osu".into()),
            ..Default::default()
        })),
        ResultBody::Ready(_)
    ));
}
#[test]
fn legacy_calculation_preserves_engine_bytes_for_current_relative_and_contained_absolute_paths() {
    let case = Case::new(false);
    let current = case.run(case.provider.calculate(PpQuery::default()));
    let expected = vosu_osu_calc::engine::LegacyRosuEngine
        .calculate(MAP, &Default::default())
        .unwrap();
    assert_eq!(
        current,
        ResultBody::Ready(serde_json::to_vec(&expected).unwrap().into())
    );
    for path in [
        "Set/map.osu".to_owned(),
        case.songs()
            .join("Set/map.osu")
            .to_str()
            .unwrap()
            .to_owned(),
    ] {
        assert_eq!(
            case.run(case.provider.calculate(PpQuery {
                path: Some(path),
                ..Default::default()
            })),
            current
        );
    }
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("outside.osu"), MAP).unwrap();
    for path in [
        outside
            .path()
            .join("outside.osu")
            .to_str()
            .unwrap()
            .to_owned(),
        "../outside.osu".into(),
        "Set/bg.jpg".into(),
        "Set/missing.osu".into(),
    ] {
        assert!(matches!(
            case.run(case.provider.calculate(PpQuery {
                path: Some(path),
                ..Default::default()
            })),
            ResultBody::Failed { status: 400, .. }
        ));
    }
}
#[cfg(target_os = "linux")]
#[test]
fn symlink_escape_and_root_swap_never_rebind_open_objects() {
    use std::os::unix::fs::symlink;
    let case = Case::new(false);
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret"), b"forbidden").unwrap();
    symlink(outside.path().join("secret"), case.songs().join("escape")).unwrap();
    assert!(matches!(
        case.run(
            case.provider
                .resolve(Route::Songs, "escape", "/Songs/escape")
        ),
        Outcome::Missing
    ));
    let outcome = case.run(
        case.provider
            .resolve(Route::Songs, "Set/bg.jpg", "/Songs/Set/bg.jpg"),
    );
    let Outcome::File { file, .. } = outcome else {
        panic!("opened file")
    };
    std::fs::rename(case.songs(), case.root.path().join("retained-songs")).unwrap();
    symlink(outside.path(), case.songs()).unwrap();
    assert_eq!(case.run(file.read(0, 64)).unwrap(), b"stable-bg");
    assert_eq!(bytes(&case, Route::Songs, "Set/bg.jpg"), b"stable-bg");
}

#[cfg(target_os = "linux")]
#[test]
fn stable_case_fallback_sees_new_files_and_backtracks_colliding_ancestors() {
    let case = Case::new(false);
    let root = case.root.path().join("skins/Skin");
    std::fs::write(root.join("SubDir/new.png"), b"new-entry").unwrap();
    assert_eq!(bytes(&case, Route::Skin, "subdir/NEW.PNG"), b"new-entry");
    std::fs::create_dir(root.join("SUBDIR")).unwrap();
    // The lexically first folded ancestor is a dead end, not proof of absence.
    assert_eq!(bytes(&case, Route::Skin, "subdir/NEW.PNG"), b"new-entry");
    std::fs::write(root.join("SUBDIR/NEW.PNG"), b"lexical-first").unwrap();
    assert_eq!(
        bytes(&case, Route::Skin, "subdir/New.Png"),
        b"lexical-first"
    );
    assert_eq!(bytes(&case, Route::Skin, "SubDir/new.png"), b"new-entry");
    std::fs::remove_file(root.join("SUBDIR/NEW.PNG")).unwrap();
    assert_eq!(bytes(&case, Route::Skin, "subdir/New.Png"), b"new-entry");
}

#[cfg(windows)]
#[test]
fn windows_reparse_files_and_directories_cannot_escape_declared_routes() {
    let case = Case::new(false);
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.osu"), MAP).unwrap();
    std::os::windows::fs::symlink_file(
        outside.path().join("secret.osu"),
        case.songs().join("escape.osu"),
    )
    .unwrap();
    std::os::windows::fs::symlink_dir(outside.path(), case.songs().join("escape-dir")).unwrap();
    for path in ["escape.osu", "escape-dir/secret.osu"] {
        assert!(matches!(
            case.run(case.provider.resolve(Route::Songs, path, "/Songs/escape")),
            Outcome::Missing
        ));
        assert!(matches!(
            case.run(case.provider.calculate(PpQuery {
                path: Some(path.into()),
                ..Default::default()
            })),
            ResultBody::Failed { status: 400, .. }
        ));
    }
}

use super::*;

fn embedded_value() -> Value {
    let asset = crate::assets::DaemonAssets::get("offsets.json").unwrap();
    serde_json::from_slice(&asset.data).unwrap()
}

#[test]
fn embedded_schema_is_complete_and_unknown_keys_are_allowed() {
    let mut value = embedded_value();
    value.as_object_mut().unwrap().insert(
        "Future.Type".into(),
        serde_json::json!({"anything": [1, 2]}),
    );
    value["osu.Game.OsuGame"]["futureField"] = serde_json::json!({"nested": true});
    let offsets = LazerOffsets::from_json(&serde_json::to_vec(&value).unwrap()).unwrap();
    assert_eq!(offsets.osu_version, value["OsuVersion"].as_str().unwrap());
    assert!(offsets.extras.contains_key("Future.Type"));
    assert!(offsets.osu_game_osu_game.extras.contains_key("futureField"));
}

#[test]
fn required_fields_are_strict_and_nonzero() {
    let mut missing = embedded_value();
    missing
        .as_object_mut()
        .unwrap()
        .remove("osu.Framework.Game");
    assert!(LazerOffsets::from_json(&serde_json::to_vec(&missing).unwrap()).is_err());

    let mut zero = embedded_value();
    zero["osu.Game.OsuGame"]["osuLogo"] = Value::from(0);
    assert!(matches!(
        LazerOffsets::from_json(&serde_json::to_vec(&zero).unwrap()),
        Err(LazerOffsetsError::Zero("osu.Game.OsuGame.osuLogo"))
    ));

    let mut wrong_type = embedded_value();
    wrong_type["osu.Game.OsuGame"]["osuLogo"] = Value::from("8");
    assert!(matches!(
        LazerOffsets::from_json(&serde_json::to_vec(&wrong_type).unwrap()),
        Err(LazerOffsetsError::Json(_))
    ));
}

#[test]
fn optional_groups_and_legacy_fields_may_be_absent_but_not_zero() {
    let mut value = embedded_value();
    let object = value.as_object_mut().unwrap();
    object.remove("GameBaseVtable");
    object.remove("osu.Game.Screens.Select.SoloSongSelect");
    object.remove("osu.Game.Screens.SelectV2.SoloSongSelect");
    value["osu.Game.OsuGame"]
        .as_object_mut()
        .unwrap()
        .remove("ScreenStack");
    let offsets = LazerOffsets::from_json(&serde_json::to_vec(&value).unwrap()).unwrap();
    assert_eq!(
        offsets.effective_game_base_vtable(),
        FALLBACK_GAME_BASE_VTABLE
    );

    value["osu.Game.OsuGame"]["ScreenStack"] = Value::from(0);
    assert!(LazerOffsets::from_json(&serde_json::to_vec(&value).unwrap()).is_err());
}

#[test]
fn versions_are_canonical_three_component_numbers() {
    for valid in ["0.0.0", "2026.711.0", "18446744073709551615.1.2"] {
        assert!(validate_version(valid).is_ok(), "{valid}");
    }
    for invalid in [
        "", "1", "1.2", "1.2.3.4", "01.2.3", "1.-2.3", "1.2.a", "../1.2.3", "1.2.3\n",
    ] {
        assert!(validate_version(invalid).is_err(), "{invalid:?}");
    }
}

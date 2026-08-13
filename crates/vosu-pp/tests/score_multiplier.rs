use std::collections::BTreeMap;

use vosu_pp::{
    BeatmapDifficultyData, ModValue, Ruleset, ScoreMultiplierScoreContext, StructuredMod,
    score_multiplier,
};

fn game_mod(acronym: &str) -> StructuredMod {
    StructuredMod {
        acronym: acronym.to_owned(),
        settings: BTreeMap::new(),
    }
}

fn configured(
    acronym: &str,
    settings: impl IntoIterator<Item = (&'static str, ModValue)>,
) -> StructuredMod {
    StructuredMod {
        acronym: acronym.to_owned(),
        settings: settings
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect(),
    }
}

fn difficulty() -> BeatmapDifficultyData {
    BeatmapDifficultyData {
        approach_rate: 9.0,
        circle_size: 4.0,
        overall_difficulty: 8.0,
        drain_rate: 6.0,
    }
}

fn close(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() <= 1e-12,
        "expected {expected}, got {actual}"
    );
}

#[test]
fn osu_multiplier_ports_settings_and_combination_consumption() {
    let basics = [
        configured("EZ", [("retries", ModValue::Number(4.0))]),
        game_mod("NF"),
        configured("HT", [("speedChange", ModValue::Number(0.75))]),
        game_mod("HR"),
        configured("DT", [("speedChange", ModValue::Number(1.5))]),
    ];
    close(
        score_multiplier(Ruleset::Osu, difficulty(), &basics, None).unwrap(),
        0.6 * 0.5 * 0.55 * 1.09 * 1.23,
    );

    // The first HD+BL combination consumes HD and BL; TC remains and receives
    // its single multiplier rather than matching the later TC+BL combination.
    let combination = [game_mod("HD"), game_mod("BL"), game_mod("TC")];
    close(
        score_multiplier(Ruleset::Osu, difficulty(), &combination, None).unwrap(),
        1.24 * 1.02,
    );

    let hidden_deflate = [
        configured("HD", [("onlyFadeApproachCircles", ModValue::Bool(true))]),
        configured("DF", [("startScale", ModValue::Number(7.0))]),
    ];
    close(
        score_multiplier(Ruleset::Osu, difficulty(), &hidden_deflate, None).unwrap(),
        1.0 * 0.9,
    );

    let freeze_frame = [game_mod("FL"), game_mod("FF")];
    close(
        score_multiplier(Ruleset::Osu, difficulty(), &freeze_frame, None).unwrap(),
        1.1,
    );
}

#[test]
fn osu_multiplier_ports_conversion_and_fun_settings() {
    let mods = [
        configured("DA", [("circleSize", ModValue::Number(5.0))]),
        configured("CL", [("classicNoteLock", ModValue::Bool(false))]),
        game_mod("WU"),
        configured("MG", [("attractionStrength", ModValue::Number(0.5))]),
        game_mod("SY"),
    ];

    close(
        score_multiplier(Ruleset::Osu, difficulty(), &mods, None).unwrap(),
        0.5 * 0.96 * 1.046 * 0.4 * 0.99,
    );
}

#[test]
fn non_osu_multiplier_tables_match_pinned_target() {
    let taiko = [
        game_mod("SR"),
        game_mod("HR"),
        game_mod("HD"),
        game_mod("FL"),
        game_mod("CS"),
    ];
    close(
        score_multiplier(Ruleset::Taiko, difficulty(), &taiko, None).unwrap(),
        0.6 * 1.06 * 1.06 * 1.12 * 0.9,
    );

    let taiko_custom_flashlight = [configured(
        "FL",
        [("sizeMultiplier", ModValue::Number(1.1))],
    )];
    close(
        score_multiplier(Ruleset::Taiko, difficulty(), &taiko_custom_flashlight, None).unwrap(),
        1.0,
    );

    let catch = [
        game_mod("HR"),
        game_mod("HD"),
        game_mod("FL"),
        game_mod("RX"),
        game_mod("SY"),
    ];
    close(
        score_multiplier(Ruleset::Catch, difficulty(), &catch, None).unwrap(),
        1.12 * 1.06 * 1.12 * 0.1 * 0.8,
    );

    let mania = [
        game_mod("NR"),
        game_mod("DA"),
        game_mod("CS"),
        game_mod("HO"),
        game_mod("AS"),
    ];
    close(
        score_multiplier(Ruleset::Mania, difficulty(), &mania, None).unwrap(),
        0.9 * 0.5 * 0.9 * 0.9 * 0.5,
    );
}

#[test]
fn backwards_compatibility_score_context_is_exact() {
    let old_classic = ScoreMultiplierScoreContext {
        total_score_version: 30_000_016,
        client_version: String::new(),
        date_unix_ms: 1_752_796_799_999,
    };
    close(
        score_multiplier(
            Ruleset::Catch,
            difficulty(),
            &[game_mod("CL")],
            Some(&old_classic),
        )
        .unwrap(),
        0.96,
    );
    close(
        score_multiplier(
            Ruleset::Mania,
            difficulty(),
            &[game_mod("4K")],
            Some(&old_classic),
        )
        .unwrap(),
        1.0,
    );

    let old_client = ScoreMultiplierScoreContext {
        total_score_version: 30_000_017,
        client_version: "2025.717.0".to_owned(),
        date_unix_ms: i64::MAX,
    };
    close(
        score_multiplier(
            Ruleset::Mania,
            difficulty(),
            &[game_mod("7K")],
            Some(&old_client),
        )
        .unwrap(),
        1.0,
    );

    let current_client = ScoreMultiplierScoreContext {
        client_version: "2025.718.0".to_owned(),
        ..old_client
    };
    close(
        score_multiplier(
            Ruleset::Mania,
            difficulty(),
            &[game_mod("7K")],
            Some(&current_client),
        )
        .unwrap(),
        0.9,
    );
    close(
        score_multiplier(Ruleset::Mania, difficulty(), &[game_mod("7K")], None).unwrap(),
        0.9,
    );
}

#[test]
fn osu_score_v1_selects_the_complete_legacy_multiplier_table() {
    let old_score = ScoreMultiplierScoreContext {
        total_score_version: 30_000_016,
        client_version: String::new(),
        date_unix_ms: 0,
    };
    let current_score = ScoreMultiplierScoreContext {
        total_score_version: 30_000_017,
        ..old_score.clone()
    };
    let mods = [
        game_mod("HR"),
        game_mod("HD"),
        game_mod("SO"),
        game_mod("SY"),
    ];

    close(
        score_multiplier(Ruleset::Osu, difficulty(), &mods, Some(&old_score)).unwrap(),
        1.06 * 1.06 * 0.9 * 0.8,
    );
    close(
        score_multiplier(Ruleset::Osu, difficulty(), &mods, Some(&current_score)).unwrap(),
        1.09 * 1.04 * 0.95 * 0.99,
    );

    let configured_hard_rock = configured("HR", [("restart", ModValue::Bool(false))]);
    close(
        score_multiplier(
            Ruleset::Osu,
            difficulty(),
            &[configured_hard_rock],
            Some(&old_score),
        )
        .unwrap(),
        1.0,
    );
}

#[test]
fn snake_case_reader_settings_drive_speed_da_hidden_and_flashlight() {
    let mods = [
        configured("DT", [("speed_change", ModValue::Number(1.2))]),
        configured("DA", [("circle_size", ModValue::Number(5.0))]),
        configured("HD", [("only_fade_approach_circles", ModValue::Bool(true))]),
        configured(
            "FL",
            [
                ("size_multiplier", ModValue::Number(1.5)),
                ("combo_based_size", ModValue::Bool(false)),
            ],
        ),
    ];

    close(
        score_multiplier(Ruleset::Osu, difficulty(), &mods, None).unwrap(),
        1.082 * 0.5 * 1.02 * 1.02,
    );
}

#[test]
fn duplicate_concrete_mod_types_are_rejected_like_source_dictionary() {
    let error = score_multiplier(
        Ruleset::Osu,
        difficulty(),
        &[game_mod("HD"), game_mod("hd")],
        None,
    )
    .unwrap_err();
    assert!(error.to_string().contains("duplicate mod type"));
}

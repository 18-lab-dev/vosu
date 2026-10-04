use std::collections::BTreeMap;

use vosu_pp::{DifficultyConfig, LiveLazerEngine, ModValue, StructuredMod};

const MAP: &[u8] = br#"osu file format v14

[General]
Mode:0

[Metadata]
Title:engine fixture
Artist:vosu
Creator:vosu
Version:test

[Difficulty]
HPDrainRate:5
CircleSize:4
OverallDifficulty:8
ApproachRate:9
SliderMultiplier:1.4
SliderTickRate:1

[TimingPoints]
0,500,4,2,1,100,1,0

[HitObjects]
64,192,1000,1,0,0:0:0:0:
256,192,1500,2,0,B|320:192,1,140
448,192,2200,1,0,0:0:0:0:
"#;

#[test]
fn structured_speed_mods_reach_live_engine() {
    let dt = StructuredMod {
        acronym: "DT".to_owned(),
        settings: BTreeMap::from([("speedChange".to_owned(), ModValue::Number(1.2))]),
    };

    let engine = LiveLazerEngine;
    let mut parsed = engine.parse(MAP).unwrap();
    engine.apply_mods(&mut parsed, &[dt]).unwrap();
    let attrs = engine
        .difficulty(&parsed, &DifficultyConfig::default())
        .unwrap();

    assert!(attrs.stars().is_finite());
}

#[test]
fn tiny_structured_rate_and_explicit_rate_are_rejected_before_difficulty() {
    let engine = LiveLazerEngine;
    let map = engine.parse(MAP).unwrap();
    let tiny_mod = StructuredMod {
        acronym: "DT".to_owned(),
        settings: BTreeMap::from([("speedChange".to_owned(), ModValue::Number(1.0e-12))]),
    };
    let mut modded = map.clone();
    engine.apply_mods(&mut modded, &[tiny_mod]).unwrap();
    assert!(
        engine
            .difficulty(&modded, &DifficultyConfig::default())
            .is_err()
    );
    assert!(
        engine
            .difficulty(
                &map,
                &DifficultyConfig {
                    clock_rate: Some(1.0e-12),
                    ..DifficultyConfig::default()
                },
            )
            .is_err()
    );
    assert!(
        engine
            .difficulty(
                &map,
                &DifficultyConfig {
                    clock_rate: Some(1.0),
                    ..DifficultyConfig::default()
                },
            )
            .is_ok()
    );
}

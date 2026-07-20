//! Machine-readable provenance for the pinned live-calculator forward port.
//!
//! Each entry names the authoritative baseline or target source and the Rust
//! implementation that carries it. The list is intentionally kept in code so
//! parity reviews can fail when a ruleset or wrapper branch is omitted.

/// One independently reviewable source-to-Rust port surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourcePort {
    pub scope: &'static str,
    pub baseline: &'static str,
    pub target: &'static str,
    pub rust: &'static str,
}

const ROSU: &str = "rosu-pp v4.0.1 (osu! 28c846b4)";
const OSU: &str = "osu! 97e94babde9d5ecbe574ecdd33509db27452cd0f";
const WRAPPER: &str = "tosuapp/lazer-calculator 9a22c4c74f8e584fa64133a79a204dd045646bac";

/// Exhaustive high-level map of the two authoritative forward-port inputs.
pub const SOURCE_PORTS: &[SourcePort] = &[
    SourcePort {
        scope: "shared.forward-port-revision",
        baseline: ROSU,
        target: OSU,
        rust: "vendor/rosu-pp/src; src/{lib,data,score_multiplier,signed_osu,signed_taiko}.rs",
    },
    SourcePort {
        scope: "shared.harmonic-variable-skills",
        baseline: ROSU,
        target: "osu.Game/Rulesets/Difficulty/Skills/{HarmonicSkill,VariableSkill}.cs",
        rust: "vendor/rosu-pp/src/any/difficulty/skills/{harmonic,variable}.rs",
    },
    SourcePort {
        scope: "shared.beatmap-bpm",
        baseline: ROSU,
        target: "osu.Game/Beatmaps/Beatmap.cs timing-point BPM selection",
        rust: "vendor/rosu-pp/src/model/beatmap/bpm.rs",
    },
    SourcePort {
        scope: "shared.score-state-hit-results",
        baseline: ROSU,
        target: "osu.Game/Rulesets/Scoring/{ScoreInfo,HitResult}.cs",
        rust: "vendor/rosu-pp/src/any/{score_state,hit_result,hitresult_generator}.rs; src/lib.rs ScoreInfoData",
    },
    SourcePort {
        scope: "shared.structured-mods",
        baseline: ROSU,
        target: "osu.Game/Rulesets/Mods and osu.Game/Rulesets/Scoring mod settings",
        rust: "vendor/rosu-pp/src/model/mods.rs; src/lib.rs StructuredMod",
    },
    SourcePort {
        scope: "shared.score-multiplier-osu",
        baseline: ROSU,
        target: "osu.Game.Rulesets.Osu/Scoring/OsuScoreMultiplierCalculatorV2.cs",
        rust: "src/score_multiplier.rs",
    },
    SourcePort {
        scope: "shared.score-multiplier-taiko",
        baseline: ROSU,
        target: "osu.Game.Rulesets.Taiko/Scoring/TaikoScoreMultiplierCalculatorV2.cs",
        rust: "src/score_multiplier.rs",
    },
    SourcePort {
        scope: "shared.score-multiplier-catch",
        baseline: ROSU,
        target: "osu.Game.Rulesets.Catch/Scoring/CatchScoreMultiplierCalculatorV2.cs",
        rust: "src/score_multiplier.rs",
    },
    SourcePort {
        scope: "shared.score-multiplier-mania",
        baseline: ROSU,
        target: "osu.Game.Rulesets.Mania/Scoring/ManiaScoreMultiplierCalculatorV2.cs; osu.Game/Rulesets/Scoring/ScoreMultiplierCalculator.cs",
        rust: "src/score_multiplier.rs",
    },
    SourcePort {
        scope: "osu.playable-hitobject-preprocessing-stacking",
        baseline: ROSU,
        target: "osu.Game/Beatmaps/WorkingBeatmap.cs; osu.Game.Rulesets.Osu/Beatmaps/OsuBeatmapProcessor.cs; osu.Game.Rulesets.Osu/Objects/OsuHitObject.cs; osu.Game.Rulesets.Osu/Difficulty",
        rust: "vendor/rosu-pp/src/osu/{convert,object,preprocessing}.rs; vendor/rosu-pp/src/osu/difficulty/{object,gradual}.rs",
    },
    SourcePort {
        scope: "osu.difficulty-skills",
        baseline: ROSU,
        target: "osu.Game.Rulesets.Osu/Difficulty/Skills/{Aim,Speed,Flashlight,Reading}.cs",
        rust: "vendor/rosu-pp/src/osu/difficulty/skills/{aim,speed,flashlight,reading}.rs",
    },
    SourcePort {
        scope: "osu.difficulty-evaluators",
        baseline: ROSU,
        target: "osu.Game.Rulesets.Osu/Difficulty/Evaluators/{Aim,Speed,Flashlight,Reading,Rhythm}.cs",
        rust: "vendor/rosu-pp/src/osu/difficulty/evaluators/{aim,speed,flashlight,reading,rhythm}.rs",
    },
    SourcePort {
        scope: "osu.difficulty-attributes-rating",
        baseline: ROSU,
        target: "osu.Game.Rulesets.Osu/Difficulty/OsuDifficultyCalculator.cs; osu.Game.Rulesets.Osu/Difficulty/OsuDifficultyAttributes.cs",
        rust: "vendor/rosu-pp/src/osu/{attributes,difficulty/mod,difficulty/rating}.rs",
    },
    SourcePort {
        scope: "osu.performance-reading-legacy-misses",
        baseline: ROSU,
        target: "osu.Game.Rulesets.Osu/Difficulty/OsuPerformanceCalculator.cs; osu.Game.Rulesets.Osu/Utils/OsuLegacyScoreSimulator.cs",
        rust: "vendor/rosu-pp/src/osu/performance; vendor/rosu-pp/src/osu/legacy_score_miss_calc.rs",
    },
    SourcePort {
        scope: "osu.signed-wrapper-results",
        baseline: WRAPPER,
        target: "lib/native/Internal/{ScoreSimulator,AccuracyCalculator}.cs; lib/native/PlayBeatmap.cs",
        rust: "src/signed_osu.rs; src/lib.rs",
    },
    SourcePort {
        scope: "taiko.conversion-difficulty-objects",
        baseline: ROSU,
        target: "osu.Game.Rulesets.Taiko/Beatmaps/TaikoBeatmapConverter.cs; osu.Game.Rulesets.Taiko/Difficulty/TaikoDifficultyCalculator.cs",
        rust: "vendor/rosu-pp/src/taiko/{convert,object,difficulty}.rs",
    },
    SourcePort {
        scope: "taiko.rhythm-colour-reading-stamina",
        baseline: ROSU,
        target: "osu.Game.Rulesets.Taiko/Difficulty/Skills and osu.Game.Rulesets.Taiko/Difficulty/Evaluators",
        rust: "vendor/rosu-pp/src/taiko/difficulty/skills; vendor/rosu-pp/src/taiko/difficulty/evaluators",
    },
    SourcePort {
        scope: "taiko.performance",
        baseline: ROSU,
        target: "osu.Game.Rulesets.Taiko/Difficulty/TaikoPerformanceCalculator.cs",
        rust: "vendor/rosu-pp/src/taiko/performance; src/signed_taiko.rs",
    },
    SourcePort {
        scope: "catch.conversion-linear-spacing",
        baseline: ROSU,
        target: "osu.Game.Rulesets.Catch/Beatmaps/CatchBeatmapConverter.cs; osu.Game.Rulesets.Catch/Difficulty/CatchDifficultyHitObject.cs",
        rust: "vendor/rosu-pp/src/catch/{convert,difficulty/object}.rs",
    },
    SourcePort {
        scope: "catch.difficulty-movement",
        baseline: ROSU,
        target: "osu.Game.Rulesets.Catch/Difficulty/{CatchDifficultyCalculator,Skills/Movement}.cs",
        rust: "vendor/rosu-pp/src/catch/difficulty",
    },
    SourcePort {
        scope: "catch.performance-signed-wrapper-mapping",
        baseline: ROSU,
        target: "osu.Game.Rulesets.Catch/Difficulty/CatchPerformanceCalculator.cs; lib/native/Internal/ScoreSimulator.cs",
        rust: "vendor/rosu-pp/src/catch/performance; src/lib.rs",
    },
    SourcePort {
        scope: "mania.conversion-patterns",
        baseline: ROSU,
        target: "osu.Game.Rulesets.Mania/Beatmaps/ManiaBeatmapConverter.cs; osu.Game.Rulesets.Mania/Beatmaps/Patterns",
        rust: "vendor/rosu-pp/src/mania/{convert,convert/pattern}.rs",
    },
    SourcePort {
        scope: "mania.difficulty-strain",
        baseline: ROSU,
        target: "osu.Game.Rulesets.Mania/Difficulty/{ManiaDifficultyCalculator,Skills/Strain}.cs",
        rust: "vendor/rosu-pp/src/mania/difficulty",
    },
    SourcePort {
        scope: "mania.performance",
        baseline: ROSU,
        target: "osu.Game.Rulesets.Mania/Difficulty/ManiaPerformanceCalculator.cs",
        rust: "vendor/rosu-pp/src/mania/performance",
    },
    SourcePort {
        scope: "wrapper.play-beatmap-parse-convert-mods",
        baseline: ROSU,
        target: "lib/native/{PlayBeatmap,LazerMod}.cs; lib/native/Internal/{Rulesets,DiffWorkingBeatmap}.cs",
        rust: "src/lib.rs ParsedBeatmap/StructuredMod",
    },
    SourcePort {
        scope: "wrapper.full-difficulty-performance",
        baseline: ROSU,
        target: "lib/native/PlayBeatmap.cs CalculateDifficulty/CalculatePerformance",
        rust: "src/lib.rs ParsedBeatmap::{difficulty,performance,performance_from_attrs}",
    },
    SourcePort {
        scope: "wrapper.gradual-prefix-rewind",
        baseline: ROSU,
        target: "lib/patches/0001-Gradual-diff-calculator.patch; lib/native/GradualDifficulty.cs",
        rust: "vendor/rosu-pp/src/any/{difficulty,performance}/gradual.rs; src/lib.rs GradualCalculator",
    },
    SourcePort {
        scope: "wrapper.score-simulator",
        baseline: ROSU,
        target: "lib/native/Internal/ScoreSimulator.cs",
        rust: "src/lib.rs simulate_score and per-ruleset score generation",
    },
    SourcePort {
        scope: "wrapper.accuracy-calculator",
        baseline: ROSU,
        target: "lib/native/Internal/AccuracyCalculator.cs",
        rust: "src/lib.rs calculate_accuracy/progressive_accuracy",
    },
    SourcePort {
        scope: "wrapper.hit-windows",
        baseline: ROSU,
        target: "lib/native/HitWindows.cs; osu.Game.Rulesets.Catch/UI/Catcher.cs",
        rust: "src/scoring.rs {hit_window,hit_window_for_map,hit_windows_for_map,catch_spatial_window_for_map}; vendor/rosu-pp/src/catch/catcher.rs",
    },
    SourcePort {
        scope: "wrapper.data-attribute-strain-conversion",
        baseline: ROSU,
        target: "lib/native/Data/*.cs; lib/native/DifficultyAttrs.cs; lib/native/PlayBeatmap.cs",
        rust: "src/data.rs; src/lib.rs",
    },
    SourcePort {
        scope: "wrapper.api-mod-conversion",
        baseline: ROSU,
        target: "lib/native/LazerMod.cs APIMod conversion and settings coercion",
        rust: "src/lib.rs deserialize_mods; vendor/rosu-pp/src/model/mods.rs",
    },
];

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::{CALCULATOR_REVISION, OSU_FORWARD_REVISION, ROSU_BASELINE};

    #[test]
    fn source_map_is_complete_and_pinned() {
        assert_eq!(ROSU_BASELINE, "v4.0.1 / osu! 28c846b4");
        assert_eq!(
            OSU_FORWARD_REVISION,
            "97e94babde9d5ecbe574ecdd33509db27452cd0f"
        );
        assert_eq!(
            CALCULATOR_REVISION,
            "9a22c4c74f8e584fa64133a79a204dd045646bac"
        );

        let mut scopes = BTreeSet::new();

        for port in SOURCE_PORTS {
            assert!(!port.scope.is_empty());
            assert!(!port.baseline.is_empty());
            assert!(!port.target.is_empty());
            assert!(!port.rust.is_empty());
            assert!(
                scopes.insert(port.scope),
                "duplicate source-map scope: {}",
                port.scope
            );
        }

        for required in [
            "shared.score-multiplier-osu",
            "shared.score-multiplier-taiko",
            "shared.score-multiplier-catch",
            "shared.score-multiplier-mania",
            "osu.performance-reading-legacy-misses",
            "taiko.rhythm-colour-reading-stamina",
            "catch.conversion-linear-spacing",
            "mania.conversion-patterns",
            "wrapper.gradual-prefix-rewind",
            "wrapper.score-simulator",
            "wrapper.accuracy-calculator",
            "wrapper.hit-windows",
            "wrapper.data-attribute-strain-conversion",
            "wrapper.api-mod-conversion",
        ] {
            assert!(
                scopes.contains(required),
                "missing source-map scope: {required}"
            );
        }

        assert!(
            SOURCE_PORTS
                .iter()
                .any(|port| port.baseline.contains("28c846b4"))
        );
        assert!(
            SOURCE_PORTS
                .iter()
                .any(|port| port.target.contains(OSU_FORWARD_REVISION))
        );
        assert!(
            SOURCE_PORTS
                .iter()
                .any(|port| port.baseline.contains(CALCULATOR_REVISION))
        );
    }
}

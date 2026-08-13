/// Kind of evidence used to implement a calculator behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceAuthority {
    /// A direct Rust implementation of behavior documented by a pinned MIT source.
    MitUpstream,
    /// Behavior independently specified by the public API and black-box compatibility vectors.
    IndependentSpecification,
    /// Pinned MIT ruleset semantics combined with independently specified wrapper behavior.
    Mixed,
}

/// Machine-readable clean-room attribution for one externally observed behavior scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourcePort {
    pub scope: &'static str,
    pub authority: SourceAuthority,
    /// Pinned MIT source paths, or `None` when the behavior was specified independently.
    pub upstream: Option<&'static str>,
    /// Independent specification evidence, when it contributes to the behavior.
    pub specification: Option<&'static str>,
    pub implementation: &'static str,
}

const BLACK_BOX_SPEC: &str =
    "public Rust/downstream contracts and committed black-box compatibility vectors";

/// Canonical clean-room behavior attribution.
///
/// Historical oracle files are evidence only. They are deliberately absent from every
/// `upstream` field and are not production sources, build inputs, or dependencies.
pub const SOURCE_PORTS: &[SourcePort] = &[
    SourcePort {
        scope: "catch.conversion-linear-spacing",
        authority: SourceAuthority::MitUpstream,
        upstream: Some(
            "MaxOhn/rosu-pp@27a67242 src/catch/{convert.rs,object/juice_stream.rs}; ppy/osu@97e94bab osu.Game.Rulesets.Catch/{Beatmaps/CatchBeatmapConverter.cs,Objects/JuiceStream.cs}",
        ),
        specification: None,
        implementation: "crates/vosu-pp/rosu-pp-api/src/catch/{convert.rs,object/juice_stream.rs}",
    },
    SourcePort {
        scope: "catch.difficulty-movement",
        authority: SourceAuthority::MitUpstream,
        upstream: Some(
            "MaxOhn/rosu-pp@27a67242 src/catch/difficulty; ppy/osu@97e94bab osu.Game.Rulesets.Catch/Difficulty/{CatchDifficultyCalculator.cs,Preprocessing,Evaluators,Skills}",
        ),
        specification: None,
        implementation: "crates/vosu-pp/rosu-pp-api/src/catch/difficulty",
    },
    SourcePort {
        scope: "catch.performance-signed-wrapper-mapping",
        authority: SourceAuthority::Mixed,
        upstream: Some(
            "MaxOhn/rosu-pp@27a67242 src/catch/performance; ppy/osu@97e94bab osu.Game.Rulesets.Catch/Difficulty/CatchPerformanceCalculator.cs",
        ),
        specification: Some(BLACK_BOX_SPEC),
        implementation: "crates/vosu-pp/rosu-pp-api/src/catch/performance; crates/vosu-pp/src/performance.rs",
    },
    SourcePort {
        scope: "mania.conversion-patterns",
        authority: SourceAuthority::MitUpstream,
        upstream: Some(
            "MaxOhn/rosu-pp@27a67242 src/mania/convert; ppy/osu@97e94bab osu.Game.Rulesets.Mania/Beatmaps/ManiaBeatmapConverter.cs",
        ),
        specification: None,
        implementation: "crates/vosu-pp/rosu-pp-api/src/mania/convert",
    },
    SourcePort {
        scope: "mania.difficulty-strain",
        authority: SourceAuthority::MitUpstream,
        upstream: Some(
            "MaxOhn/rosu-pp@27a67242 src/mania/difficulty; ppy/osu@97e94bab osu.Game.Rulesets.Mania/Difficulty/{ManiaDifficultyCalculator.cs,Preprocessing,Evaluators,Skills}",
        ),
        specification: None,
        implementation: "crates/vosu-pp/rosu-pp-api/src/mania/difficulty",
    },
    SourcePort {
        scope: "mania.performance",
        authority: SourceAuthority::MitUpstream,
        upstream: Some(
            "MaxOhn/rosu-pp@27a67242 src/mania/performance; ppy/osu@97e94bab osu.Game.Rulesets.Mania/Difficulty/ManiaPerformanceCalculator.cs",
        ),
        specification: None,
        implementation: "crates/vosu-pp/rosu-pp-api/src/mania/performance",
    },
    SourcePort {
        scope: "osu.difficulty-attributes-rating",
        authority: SourceAuthority::MitUpstream,
        upstream: Some(
            "MaxOhn/rosu-pp@27a67242 src/osu/difficulty; ppy/osu@97e94bab osu.Game.Rulesets.Osu/Difficulty/{OsuDifficultyCalculator.cs,OsuDifficultyAttributes.cs}",
        ),
        specification: None,
        implementation: "crates/vosu-pp/rosu-pp-api/src/osu/difficulty",
    },
    SourcePort {
        scope: "osu.difficulty-evaluators",
        authority: SourceAuthority::MitUpstream,
        upstream: Some(
            "MaxOhn/rosu-pp@27a67242 src/osu/difficulty/evaluators; ppy/osu@97e94bab osu.Game.Rulesets.Osu/Difficulty/Evaluators",
        ),
        specification: None,
        implementation: "crates/vosu-pp/rosu-pp-api/src/osu/difficulty/evaluators",
    },
    SourcePort {
        scope: "osu.difficulty-skills",
        authority: SourceAuthority::MitUpstream,
        upstream: Some(
            "MaxOhn/rosu-pp@27a67242 src/osu/difficulty/skills; ppy/osu@97e94bab osu.Game.Rulesets.Osu/Difficulty/Skills",
        ),
        specification: None,
        implementation: "crates/vosu-pp/rosu-pp-api/src/osu/difficulty/skills",
    },
    SourcePort {
        scope: "osu.performance-reading-legacy-misses",
        authority: SourceAuthority::MitUpstream,
        upstream: Some(
            "MaxOhn/rosu-pp@27a67242 src/osu/performance; ppy/osu@97e94bab osu.Game.Rulesets.Osu/Difficulty/{OsuPerformanceCalculator.cs,OsuLegacyScoreMissCalculator.cs}",
        ),
        specification: None,
        implementation: "crates/vosu-pp/rosu-pp-api/src/osu/performance",
    },
    SourcePort {
        scope: "osu.playable-hitobject-preprocessing-stacking",
        authority: SourceAuthority::MitUpstream,
        upstream: Some(
            "MaxOhn/rosu-pp@27a67242 src/osu/difficulty/object.rs; ppy/osu@97e94bab osu.Game.Rulesets.Osu/Difficulty/Preprocessing/OsuDifficultyHitObject.cs",
        ),
        specification: None,
        implementation: "crates/vosu-pp/rosu-pp-api/src/osu/{difficulty/object.rs,object.rs}",
    },
    SourcePort {
        scope: "osu.signed-wrapper-results",
        authority: SourceAuthority::IndependentSpecification,
        upstream: None,
        specification: Some(BLACK_BOX_SPEC),
        implementation: "crates/vosu-pp/src/{scoring.rs,performance.rs}",
    },
    SourcePort {
        scope: "shared.beatmap-bpm",
        authority: SourceAuthority::MitUpstream,
        upstream: Some(
            "MaxOhn/rosu-pp@27a67242 src/model/beatmap; MaxOhn/rosu-map@333dd2be src/section/timing_points.rs",
        ),
        specification: None,
        implementation: "crates/vosu-pp/rosu-pp-api/src/model/beatmap; crates/vosu-pp/rosu-map-api/src/section/timing_points/control_points/timing.rs",
    },
    SourcePort {
        scope: "shared.forward-port-revision",
        authority: SourceAuthority::MitUpstream,
        upstream: Some("MaxOhn/rosu-pp@27a67242; ppy/osu@97e94babde9d5ecbe574ecdd33509db27452cd0f"),
        specification: None,
        implementation: "crates/vosu-pp/rosu-pp-api/src",
    },
    SourcePort {
        scope: "shared.harmonic-variable-skills",
        authority: SourceAuthority::MitUpstream,
        upstream: Some(
            "ppy/osu@97e94bab osu.Game/Rulesets/Difficulty/Skills/{HarmonicSkill.cs,VariableLengthStrainSkill.cs}",
        ),
        specification: None,
        implementation: "crates/vosu-pp/rosu-pp-api/src/any/difficulty/skills.rs",
    },
    SourcePort {
        scope: "shared.score-multiplier-catch",
        authority: SourceAuthority::MitUpstream,
        upstream: Some(
            "ppy/osu@97e94bab osu.Game.Rulesets.Catch/Scoring/CatchScoreMultiplierCalculator.cs",
        ),
        specification: None,
        implementation: "crates/vosu-pp/src/score_multiplier.rs",
    },
    SourcePort {
        scope: "shared.score-multiplier-mania",
        authority: SourceAuthority::MitUpstream,
        upstream: Some(
            "ppy/osu@97e94bab osu.Game.Rulesets.Mania/Scoring/ManiaScoreMultiplierCalculator.cs",
        ),
        specification: None,
        implementation: "crates/vosu-pp/src/score_multiplier.rs",
    },
    SourcePort {
        scope: "shared.score-multiplier-osu",
        authority: SourceAuthority::MitUpstream,
        upstream: Some(
            "ppy/osu@97e94bab osu.Game.Rulesets.Osu/Scoring/OsuScoreMultiplierCalculatorV2.cs",
        ),
        specification: None,
        implementation: "crates/vosu-pp/src/score_multiplier.rs",
    },
    SourcePort {
        scope: "shared.score-multiplier-taiko",
        authority: SourceAuthority::MitUpstream,
        upstream: Some(
            "ppy/osu@97e94bab osu.Game.Rulesets.Taiko/Scoring/TaikoScoreMultiplierCalculator.cs",
        ),
        specification: None,
        implementation: "crates/vosu-pp/src/score_multiplier.rs",
    },
    SourcePort {
        scope: "shared.score-state-hit-results",
        authority: SourceAuthority::MitUpstream,
        upstream: Some(
            "MaxOhn/rosu-pp@27a67242 src/any/score_state.rs; ppy/osu@97e94bab osu.Game/{Rulesets/Scoring/{HitResult.cs,ScoreProcessor.cs},Scoring/ScoreInfo.cs}",
        ),
        specification: None,
        implementation: "crates/vosu-pp/rosu-pp-api/src/any/score_state.rs; crates/vosu-pp/src/model.rs",
    },
    SourcePort {
        scope: "shared.structured-mods",
        authority: SourceAuthority::Mixed,
        upstream: Some(
            "rosu-mods 0.4.0 MIT public serde API; ppy/osu@97e94bab osu.Game/Rulesets/Mods and mode-specific Mods",
        ),
        specification: Some(BLACK_BOX_SPEC),
        implementation: "crates/vosu-pp/src/mods.rs",
    },
    SourcePort {
        scope: "taiko.conversion-difficulty-objects",
        authority: SourceAuthority::MitUpstream,
        upstream: Some(
            "MaxOhn/rosu-pp@27a67242 src/taiko/{convert.rs,difficulty}; ppy/osu@97e94bab osu.Game.Rulesets.Taiko/{Beatmaps/TaikoBeatmapConverter.cs,Difficulty}",
        ),
        specification: None,
        implementation: "crates/vosu-pp/rosu-pp-api/src/taiko/{convert.rs,difficulty}",
    },
    SourcePort {
        scope: "taiko.performance",
        authority: SourceAuthority::MitUpstream,
        upstream: Some(
            "MaxOhn/rosu-pp@27a67242 src/taiko/performance; ppy/osu@97e94bab osu.Game.Rulesets.Taiko/Difficulty/TaikoPerformanceCalculator.cs",
        ),
        specification: None,
        implementation: "crates/vosu-pp/rosu-pp-api/src/taiko/performance",
    },
    SourcePort {
        scope: "taiko.rhythm-colour-reading-stamina",
        authority: SourceAuthority::MitUpstream,
        upstream: Some(
            "MaxOhn/rosu-pp@27a67242 src/taiko/difficulty; ppy/osu@97e94bab osu.Game.Rulesets.Taiko/Difficulty/{Preprocessing,Evaluators,Skills}",
        ),
        specification: None,
        implementation: "crates/vosu-pp/rosu-pp-api/src/taiko/difficulty",
    },
    SourcePort {
        scope: "wrapper.accuracy-calculator",
        authority: SourceAuthority::Mixed,
        upstream: Some(
            "ppy/osu@97e94bab osu.Game/Rulesets/Scoring/ScoreProcessor.cs and mode-specific score processors",
        ),
        specification: Some(BLACK_BOX_SPEC),
        implementation: "crates/vosu-pp/src/scoring.rs",
    },
    SourcePort {
        scope: "wrapper.api-mod-conversion",
        authority: SourceAuthority::Mixed,
        upstream: Some("rosu-mods 0.4.0 MIT public serde API; ppy/osu@97e94bab mode-specific Mods"),
        specification: Some(BLACK_BOX_SPEC),
        implementation: "crates/vosu-pp/src/{mods.rs,beatmap.rs}",
    },
    SourcePort {
        scope: "wrapper.data-attribute-strain-conversion",
        authority: SourceAuthority::Mixed,
        upstream: Some(
            "MaxOhn/rosu-pp@27a67242 mode attributes and strains; ppy/osu@97e94bab mode difficulty attributes",
        ),
        specification: Some(BLACK_BOX_SPEC),
        implementation: "crates/vosu-pp/src/data.rs",
    },
    SourcePort {
        scope: "wrapper.full-difficulty-performance",
        authority: SourceAuthority::Mixed,
        upstream: Some(
            "MaxOhn/rosu-pp@27a67242 any and mode difficulty/performance APIs; ppy/osu@97e94bab mode calculators",
        ),
        specification: Some(BLACK_BOX_SPEC),
        implementation: "crates/vosu-pp/src/{difficulty.rs,performance.rs}",
    },
    SourcePort {
        scope: "wrapper.gradual-prefix-rewind",
        authority: SourceAuthority::Mixed,
        upstream: Some(
            "MaxOhn/rosu-pp@27a67242 src/any/{difficulty,performance}/gradual.rs and mode gradual calculators; ppy/osu@97e94bab DifficultyCalculator prefix processing",
        ),
        specification: Some(BLACK_BOX_SPEC),
        implementation: "crates/vosu-pp/src/gradual.rs",
    },
    SourcePort {
        scope: "wrapper.hit-windows",
        authority: SourceAuthority::MitUpstream,
        upstream: Some(
            "ppy/osu@97e94bab osu.Game/Rulesets/Scoring/HitWindows.cs; osu.Game.Rulesets.{Osu,Taiko,Mania}/Scoring/{Osu,Taiko,Mania}HitWindows.cs",
        ),
        specification: None,
        implementation: "crates/vosu-pp/src/hit_windows.rs",
    },
    SourcePort {
        scope: "wrapper.play-beatmap-parse-convert-mods",
        authority: SourceAuthority::Mixed,
        upstream: Some(
            "MaxOhn/rosu-pp@27a67242 Beatmap conversion and Difficulty APIs; rosu-mods 0.4.0 MIT public API",
        ),
        specification: Some(BLACK_BOX_SPEC),
        implementation: "crates/vosu-pp/src/{beatmap.rs,mods.rs}",
    },
    SourcePort {
        scope: "wrapper.score-simulator",
        authority: SourceAuthority::Mixed,
        upstream: Some(
            "MaxOhn/rosu-pp@27a67242 mode hit-result generators; ppy/osu@97e94bab ScoreProcessor.cs and mode legacy score simulators",
        ),
        specification: Some(BLACK_BOX_SPEC),
        implementation: "crates/vosu-pp/src/scoring.rs",
    },
];

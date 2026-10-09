/// Declared authority for a calculator behavior; not proof of source independence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceAuthority {
    /// A direct Rust implementation of behavior documented by a pinned MIT source.
    MitUpstream,
    /// Behavior independently specified by the public API and black-box compatibility vectors.
    IndependentSpecification,
    /// Pinned MIT ruleset semantics combined with black-box-specified compatibility behavior.
    Mixed,
}

/// Machine-readable attribution claim for one externally observed behavior scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourcePort {
    pub scope: &'static str,
    pub authority: SourceAuthority,
    /// Pinned MIT source paths, or `None` when no MIT authority is declared.
    /// Non-MIT port sources are deliberately excluded.
    pub upstream: Option<&'static str>,
    /// Black-box compatibility evidence, when it contributes to the behavior.
    pub specification: Option<&'static str>,
    pub implementation: &'static str,
}

const BLACK_BOX_SPEC: &str =
    "public Rust/downstream contracts and committed black-box compatibility vectors";
const ACCURACY_SPEC: &str = "black-box vectors: Classic omits osu! slider-tail and large-tick accuracy terms; mania perfect weight 300 with Classic, otherwise 305";
const DATA_SPEC: &str = "black-box vectors: camelCase wire fields, zero-filled cross-ruleset fields, Taiko effectiveMissCount = estimated unstable rate or -1, empty strain peaks report section length 0, and osu! aim, speed and reading graphs as fixed 400 ms section peaks of per-object difficulties with 0.15 per-second decay into each new section";
const GRADUAL_SPEC: &str = "black-box vectors: one step per source hit object processes difficulty objects whose base object ends by that object's end time; skip-to-time counts objects ending at or before the time; rewind and negative skips rebuild from the start";

/// Declared behavior attribution; see `docs/provenance-audit.md` for open questions.
///
/// This catalog omits non-MIT authorities because it describes permissive
/// calculation sources and observable contracts, not complete authoring history.
/// Recovered rewrite records also show consultation of the former Rust and LGPL
/// reference; corresponding MIT formulas and passing vectors do not establish
/// implementation independence. See `docs/provenance-audit.md` for those limits.
/// The retained tosuapp/lazer-calculator reference is not a production build
/// input or dependency. Its absence from `upstream` fields must not be read as
/// proof that it was absent from the implementation process.
/// `tools/oracle/calculator-diff-manifest.json` records historical comparison
/// baselines, not a complete determination of current implementation origin.
/// The 2359305 feature cutover additionally uses ppy/osu@fe30ae7e Hidden slider
/// fade-in and performance clamps. Current black-box vectors are pinned in
/// `tools/oracle/oracle-lock.json`; the original source-port inventory and NUnit
/// inputs remain tied to the baseline revision.
// The local banana-shower progress/count guard is defensive rejection/truncation,
// not a new upstream semantics port or an external oracle result. Its ownership
// and ordinary-behavior regressions are recorded in THIRD_PARTY_LICENSES.txt.
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
            "MaxOhn/rosu-pp@27a67242 src/catch/performance; ppy/osu@97e94bab osu.Game.Rulesets.Catch/Difficulty/CatchPerformanceCalculator.cs and osu.Game/Scoring/Legacy/ScoreInfoExtensions.cs count accessors",
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
            "MaxOhn/rosu-pp@27a67242 src/osu/performance; ppy/osu@97e94bab osu.Game.Rulesets.Osu/Difficulty/{OsuPerformanceCalculator.cs,OsuLegacyScoreMissCalculator.cs}; ppy/osu@fe30ae7e OsuPerformanceCalculator.cs accuracy/miss-penalty clamps",
        ),
        specification: None,
        implementation: "crates/vosu-pp/rosu-pp-api/src/osu/performance",
    },
    SourcePort {
        scope: "osu.playable-hitobject-preprocessing-stacking",
        authority: SourceAuthority::MitUpstream,
        upstream: Some(
            "MaxOhn/rosu-pp@27a67242 src/osu/difficulty/object.rs; ppy/osu@97e94bab osu.Game.Rulesets.Osu/Difficulty/Preprocessing/OsuDifficultyHitObject.cs; ppy/osu@fe30ae7e osu.Game.Rulesets.Osu/Mods/OsuModHidden.cs slider fade-in",
        ),
        specification: None,
        implementation: "crates/vosu-pp/rosu-pp-api/src/osu/{difficulty/object.rs,object.rs}",
    },
    SourcePort {
        scope: "osu.signed-wrapper-results",
        authority: SourceAuthority::Mixed,
        upstream: Some(
            "ppy/osu-tools@d82cf2a9 PerformanceCalculator/Simulate/OsuSimulateCommand.cs generateHitResults/GetAccuracy",
        ),
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
        upstream: Some(
            "MaxOhn/rosu-pp@27a67242; ppy/osu@97e94babde9d5ecbe574ecdd33509db27452cd0f; ppy/osu@fe30ae7eea2beeb1f0d3a5ce8dde12bb78ca3f48",
        ),
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
            "ppy/osu-tools@d82cf2a9 PerformanceCalculator/Simulate/*SimulateCommand.cs GetAccuracy and PerformanceCalculatorGUI/RulesetHelper.cs; MaxOhn/rosu-pp@27a67242 src/osu/score_state.rs OsuScoreOrigin::Stable; ppy/osu@97e94bab osu.Game/Rulesets/Scoring/ScoreProcessor.cs and mode-specific score processors",
        ),
        specification: Some(ACCURACY_SPEC),
        implementation: "crates/vosu-pp/src/scoring.rs",
    },
    SourcePort {
        scope: "wrapper.api-mod-conversion",
        authority: SourceAuthority::Mixed,
        upstream: Some("rosu-mods 0.4.0 MIT public serde API; ppy/osu@97e94bab mode-specific Mods"),
        specification: Some(BLACK_BOX_SPEC),
        implementation: "crates/vosu-pp/src/{mods.rs,beatmap.rs,beatmap/random.rs}",
    },
    SourcePort {
        scope: "wrapper.data-attribute-strain-conversion",
        authority: SourceAuthority::Mixed,
        upstream: Some(
            "MaxOhn/rosu-pp@27a67242 mode attributes and strains; ppy/osu@97e94bab mode difficulty attributes, osu.Game/Rulesets/Difficulty/Skills/StrainSkill.cs section peaks and osu.Game.Rulesets.Osu/Difficulty/Skills/Flashlight.cs initial-strain decay",
        ),
        specification: Some(DATA_SPEC),
        implementation: "crates/vosu-pp/src/data.rs; crates/vosu-pp/rosu-pp-api/src/osu/difficulty/skills/{aim,speed,reading}.rs",
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
            "MaxOhn/rosu-pp@27a67242 src/any/{difficulty,performance}/gradual.rs and mode gradual calculators; ppy/osu@97e94bab DifficultyCalculator prefix processing and StrainSkill.cs section peaks",
        ),
        specification: Some(GRADUAL_SPEC),
        implementation: "crates/vosu-pp/src/gradual.rs; crates/vosu-pp/rosu-pp-api/src/{any,osu,taiko,catch,mania}/difficulty/gradual.rs",
    },
    SourcePort {
        scope: "wrapper.hit-windows",
        authority: SourceAuthority::Mixed,
        upstream: Some(
            "ppy/osu@97e94bab osu.Game/Rulesets/Scoring/HitWindows.cs including EmptyHitWindows; osu.Game.Rulesets.{Osu,Taiko,Mania}/Scoring/{Osu,Taiko,Mania}HitWindows.cs including Mania Classic/convert windows; osu.Game.Rulesets.Catch/Objects/CatchHitObject.cs HitWindows.Empty",
        ),
        specification: Some(
            "black-box vectors: the per-judgement lookup reports no Catch window, while map windows report zero for every Catch judgement",
        ),
        implementation: "crates/vosu-pp/src/hit_windows.rs",
    },
    SourcePort {
        scope: "wrapper.play-beatmap-parse-convert-mods",
        authority: SourceAuthority::Mixed,
        upstream: Some(
            "MaxOhn/rosu-pp@27a67242 Beatmap conversion and Difficulty APIs and src/util/random/csharp.rs; rosu-mods 0.4.0 MIT public API; ppy/osu@97e94bab osu.Game.Rulesets.{Osu,Taiko,Mania}/Mods/*Random.cs, OsuHitObjectGenerationUtils.cs, ManiaMod{Mirror,HoldOff,Invert}.cs, TaikoModSimplifiedRhythm.cs and ModDifficultyAdjust.cs; dotnet/runtime@15872212 Random.CompatImpl.cs",
        ),
        specification: Some(BLACK_BOX_SPEC),
        implementation: "crates/vosu-pp/src/{beatmap.rs,beatmap/random.rs,mods.rs}",
    },
    SourcePort {
        scope: "wrapper.score-simulator",
        authority: SourceAuthority::Mixed,
        upstream: Some(
            "ppy/osu-tools@d82cf2a9 PerformanceCalculator/Simulate/{Osu,Taiko,Catch,Mania}SimulateCommand.cs generateHitResults; MaxOhn/rosu-pp@27a67242 mode hit-result generators; ppy/osu@97e94bab ScoreProcessor.cs and mode legacy score simulators",
        ),
        specification: Some(BLACK_BOX_SPEC),
        implementation: "crates/vosu-pp/src/scoring.rs",
    },
];

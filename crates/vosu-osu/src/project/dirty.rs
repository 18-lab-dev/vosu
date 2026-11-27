//! Input dependencies of the fixed publication partition, not sampling lanes.
//! Status/progress deliberately span both capture lanes. Keep this table in sync
//! with projection reads; the dependency/convergence tests exercise isolated inputs.
use super::OsuPointer as P;
use std::sync::Arc;
use vosu_osu_state::StateBundleSnapshot;

const GLOBAL: u16 = 1;
const SETTINGS: u16 = 1 << 1;
const BEATMAP: u16 = 1 << 2;
const MENU: u16 = 1 << 3;
const BASS: u16 = 1 << 4;
const GAMEPLAY: u16 = 1 << 5;
const RESULT: u16 = 1 << 6;
const USER: u16 = 1 << 7;
const TOURNEY: u16 = 1 << 8;
const RANKED: u16 = 1 << 9;
const ROOM: u16 = 1 << 10;
const PRECISE: u16 = 1 << 11;
const STATUS: u16 = 1 << 12;
const SESSION: u16 = 1 << 13;

#[derive(Clone, Copy)]
pub(crate) struct Dirty(u16);
impl Dirty {
    pub(crate) const ALL: Self = Self(u16::MAX);
    pub(crate) fn between(
        previous: Option<&StateBundleSnapshot>,
        next: &StateBundleSnapshot,
    ) -> Self {
        let Some(previous) = previous else {
            return Self::ALL;
        };
        if previous.generation != next.generation || previous.reset_epoch != next.reset_epoch {
            return Self::ALL;
        }
        let mut bits = SESSION;
        if !Arc::ptr_eq(&previous.precise, &next.precise) {
            bits |= PRECISE;
            if previous.precise.status != next.precise.status {
                bits |= STATUS;
            }
        }
        if !Arc::ptr_eq(&previous.regular, &next.regular) {
            // Result equality costs about as much as encoding/diffing it.
            // Keep the lane skip, but do not compare that group on regular ticks.
            bits |= RESULT;
            let a = &previous.regular;
            let b = &next.regular;
            // Cheap/small groups first. Large groups are compared only on a
            // regular-lane change; precise ticks never walk their contents.
            for (changed, bit) in [
                (a.bass_density != b.bass_density, BASS),
                (a.room != b.room, ROOM),
                (a.ranked_play != b.ranked_play, RANKED),
                (a.user != b.user, USER),
                (a.menu != b.menu, MENU),
                (a.global != b.global, GLOBAL),
                (a.settings != b.settings, SETTINGS),
                (
                    a.lazer_multi_spectating != b.lazer_multi_spectating,
                    TOURNEY,
                ),
                (a.beatmap != b.beatmap, BEATMAP),
                (a.gameplay != b.gameplay, GAMEPLAY),
            ] {
                if changed {
                    bits |= bit;
                }
            }
        }
        Self(bits)
    }
    pub(crate) fn contains(self, pointer: P) -> bool {
        self.0 & dependencies(pointer) != 0
    }
}

fn dependencies(p: P) -> u16 {
    use P::*;
    match p {
        StatusStatus => GLOBAL | STATUS,
        ChartTitle | ChartArtist | ChartDifficultyName | ChartCharter => MENU,
        ChartLength | ChartBpm => BEATMAP,
        ChartKeyCount => BEATMAP | MENU | GAMEPLAY | RESULT | STATUS,
        PlayScore | PlayCombo | PlayMaxCombo | PlayAccuracy | PlayAccuracyKind | PlayHealth
        | PlayJudgements => GAMEPLAY | STATUS,
        PlayProgress => BEATMAP | PRECISE,
        PlayRate | OsuBeatmapTimeRate | OsuPlaySource | OsuPlayMods => {
            GLOBAL | GAMEPLAY | RESULT | STATUS
        }
        SkinName => SETTINGS,
        OsuSessionPlayTime | OsuSessionPaused | OsuSessionWatchingReplay | OsuInternalPlayClock => {
            GLOBAL
        }
        OsuSessionPlayCount | OsuSessionFocused | OsuSessionIpcId | OsuSessionServer => SESSION,
        OsuSessionState => STATUS,
        OsuProfile => USER,
        OsuBeatmapIsKiai
        | OsuBeatmapIsBreak
        | OsuBeatmapTimeFirstObject
        | OsuBeatmapTimeLastObject
        | OsuBeatmapTimeFirstHitObject
        | OsuBeatmapTimePreview
        | OsuBeatmapMode
        | OsuBeatmapSource
        | OsuBeatmapTags
        | OsuBeatmapStats
        | OsuPlayPp
        | OsuPerformanceAccuracy
        | OsuPerformanceGraph => BEATMAP,
        OsuBeatmapIsConvert => BEATMAP | MENU | GAMEPLAY | RESULT | STATUS,
        OsuBeatmapTimeLive
        | OsuPlayHitErrors
        | OsuPlayHitOffsets
        | OsuPlayKeyHitErrors
        | OsuPlayCatcher
        | OsuKeysAll
        | OsuKeysOverlay
        | OsuPlayPreciseUnstableRate => PRECISE,
        OsuBeatmapTimeRunning | OsuSkinBuiltin | OsuSkinBase | OsuSkinVariant | OsuSkinFiles => {
            GLOBAL
        }
        OsuBeatmapTimeMp3Length
        | OsuBeatmapSelectionMode
        | OsuBeatmapStatus
        | OsuBeatmapChecksum
        | OsuBeatmapId
        | OsuBeatmapSet
        | OsuBeatmapArtist
        | OsuBeatmapArtistUnicode
        | OsuBeatmapTitle
        | OsuBeatmapTitleUnicode
        | OsuBeatmapMapper
        | OsuBeatmapVersion => MENU,
        OsuPlayFailed
        | OsuPlayPlayerName
        | OsuPlayMode
        | OsuPlayHealthBar
        | OsuPlayStatistics
        | OsuPlayRank
        | OsuPlayUnstableRate
        | OsuLeaderboard
        | OsuLeaderboardState
        | OsuPlayScore
        | OsuPlayCombo
        | OsuPlayAccuracyPercent
        | OsuPlayHealthRaw
        | OsuPlayJudgements
        | OsuPlayRetries => GAMEPLAY,
        OsuResult => RESULT,
        OsuSettingsInterfaceVisible
        | OsuSettingsReplayUIVisible
        | OsuSettingsChatVisibilityStatus => GLOBAL,
        OsuSettingsLeaderboard => SETTINGS | GAMEPLAY,
        OsuSettingsBassDensity => BASS,
        OsuSettingsMode => MENU,
        OsuSettingsProgressBar
        | OsuSettingsResolution
        | OsuSettingsClient
        | OsuSettingsScoreMeter
        | OsuSettingsCursor
        | OsuSettingsMouse
        | OsuSettingsTablet
        | OsuSettingsMania
        | OsuSettingsSort
        | OsuSettingsGroup
        | OsuSettingsAudio
        | OsuSettingsBackground
        | OsuSettingsKeybinds
        | OsuSettingsVisual
        | OsuSettingsSkin => SETTINGS,
        OsuRankedPlay => RANKED,
        OsuRoom => ROOM,
        OsuTourney | OsuTourneyAvailability => TOURNEY | GLOBAL | BEATMAP | STATUS | SESSION,
        OsuInternalPaths => GLOBAL | MENU,
    }
}

#[cfg(test)]
#[path = "tests/group_cost.rs"]
mod group_cost;

#[cfg(test)]
#[test]
fn untouched_groups_never_invoke_the_encoder() {
    use super::{DiffSink, LastValues, ProjectionSink, o5_tests};
    use vosu_plugin_api::PublishBatchWriter;
    let initial = o5_tests::sample();
    let mut next = initial.clone();
    Arc::make_mut(&mut next.regular).menu.title = Some("changed".into());
    let dirty = Dirty::between(Some(&initial), &next);
    assert!(dirty.contains(P::ChartTitle));
    assert!(!dirty.contains(P::OsuBeatmapStats));
    assert!(!dirty.contains(P::OsuProfile));
    assert!(!dirty.contains(P::OsuKeysAll));
    assert!(!dirty.contains(P::OsuSettingsResolution));
    let mut last = LastValues::default();
    let mut writer = PublishBatchWriter::new();
    writer.begin(10);
    let mut sink = DiffSink::filtered(&mut last, &mut writer, dirty);
    let mut called = false;
    sink.put_cbor(P::OsuBeatmapStats, &mut |_| called = true);
    assert!(!called, "unchanged groups must not encode before diffing");
    sink.put_cbor(P::ChartTitle, &mut |out| {
        called = true;
        out.push(0xf6);
    });
    assert!(called, "changed groups must reach their encoder");
}

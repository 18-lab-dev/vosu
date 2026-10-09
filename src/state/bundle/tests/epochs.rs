use super::*;
use crate::calculation::beatmap::{
    BeatmapCache, BeatmapEngine, BeatmapError, BeatmapInput, EngineOutput, EngineRequest, ModSet,
    Ruleset, ScoringMode,
};
use crate::model::memory::{StableReplayFrame, StableReplayFrameBatch};
use md5::{Digest, Md5};
use std::fmt::Write as _;

struct MetadataTestEngine;

impl BeatmapEngine for MetadataTestEngine {
    fn calculate(&self, _request: EngineRequest<'_>) -> Result<EngineOutput, BeatmapError> {
        Ok(EngineOutput::default())
    }
}

fn native_mania_map(label: &str) -> (String, Arc<ParsedBeatmap>) {
    native_mania_map_with_identity(label, None)
}

fn native_mania_map_with_identity(
    label: &str,
    memory_identity: Option<&str>,
) -> (String, Arc<ParsedBeatmap>) {
    let source = format!(
        r#"osu file format v14
[General]
Mode:3
[Metadata]
Title:{label}
Artist:Artist
Creator:Mapper
Version:Test
[Difficulty]
CircleSize:4
[HitObjects]
128,192,200,128,0,400:0:0:0:0:
"#
    );
    let mut checksum = String::with_capacity(32);
    for byte in Md5::digest(source.as_bytes()) {
        write!(checksum, "{byte:02x}").unwrap();
    }
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("map.osu");
    std::fs::write(&path, source).unwrap();
    let mods = ModSet::new("mods", Vec::new(), 1.0).unwrap();
    let parsed = BeatmapCache::new(MetadataTestEngine)
        .load(
            BeatmapInput::PathWithIdentity {
                path: &path,
                memory_identity: memory_identity.unwrap_or(&checksum),
            },
            Ruleset::Mania,
            &mods,
            ScoringMode::Stable,
        )
        .unwrap();
    (checksum, parsed)
}

#[tokio::test]
async fn changed_beatmap_projection_publishes_a_new_regular_arc() {
    let mut bundle = StateBundle::new(
        ClientType::Stable,
        24,
        Arc::new(ProjectionCalculation {
            calls: std::sync::atomic::AtomicUsize::new(0),
        }),
    );
    let first = RawRegularSnapshotBuilder::new(
        RegularCaptureContext::new(ClientType::Stable, 24, 0, 1),
        Ok(ReadStatus::Ready(raw_global(GameState::Menu, 0))),
    )
    .menu(Ok(ReadStatus::Ready(RawMenu::Full(menu("projected")))))
    .publish();
    let first = bundle
        .apply_regular(first)
        .await
        .expect("first projection")
        .publication;
    let second = RawRegularSnapshotBuilder::new(
        RegularCaptureContext::new(ClientType::Stable, 24, 0, 2),
        Ok(ReadStatus::Ready(raw_global(GameState::Menu, 0))),
    )
    .menu(Ok(ReadStatus::Ready(RawMenu::Checksum(
        crate::model::memory::MenuChecksum {
            gamemode: 0,
            ranked_status: 4.0,
        },
    ))))
    .publish();
    let second = bundle
        .apply_regular(second)
        .await
        .expect("changed projection")
        .publication;

    assert!(!Arc::ptr_eq(&first.regular, &second.regular));
    assert_eq!(first.regular.beatmap.graph.primary, [1.0]);
    assert_eq!(second.regular.beatmap.graph.primary, [2.0]);
    assert_eq!(second.regular.beatmap.event_status.bpm, 2.0);
    assert!(second.regular.beatmap.event_status.kiai);
    assert!(Arc::ptr_eq(&first.precise, &second.precise));
}

#[test]
fn immutable_projection_arc_identity_controls_regular_dirtiness() {
    let mut bundle = StateBundle::new(ClientType::Stable, 25, Arc::new(EchoCalculation));
    let graph = Arc::new(StrainGraph {
        primary: vec![1.0, 2.0],
        series: Vec::new(),
        x_axis: vec![0.0, 1.0],
    });
    bundle.beatmap.graph = graph.clone();
    let first = bundle.publish_regular_state();
    bundle.beatmap.graph = graph;
    let reused = bundle.publish_regular_state();
    assert!(Arc::ptr_eq(&first.regular, &reused.regular));

    bundle.beatmap.graph = Arc::new((*bundle.beatmap.graph).clone());
    let replaced = bundle.publish_regular_state();
    assert!(!Arc::ptr_eq(&reused.regular, &replaced.regular));
    assert_eq!(reused.regular.beatmap.graph, replaced.regular.beatmap.graph);
}

#[tokio::test]
async fn pre_object_poll_preserves_hit_history_cursor_and_keys() {
    let mut bundle = StateBundle::new(ClientType::Stable, 16, Arc::new(EchoCalculation));
    bundle.global.status = GameState::Play.value() as i64;
    bundle.global.play_time = 50.0;
    bundle.menu.checksum = Some("pre-object".into());
    bundle.beatmap.checksum = Some("pre-object".into());
    bundle.beatmap.first_object_time = Some(100);
    bundle.gameplay.is_default_state = false;
    bundle.gameplay.key_overlay = vec![KeyOverlayButton {
        name: "K1".into(),
        is_pressed: true,
        count: 3,
    }];
    bundle.gameplay.is_key_overlay_default_state = false;
    bundle.gameplay.hit_errors = press_hit_errors(vec![4.0]);
    bundle.gameplay.total_hit_errors = 4.0;
    bundle.gameplay.previous_hit_error_index = 1;
    let regular = RawRegularSnapshotBuilder::new(
        RegularCaptureContext::new(ClientType::Stable, 16, 0, 1),
        Ok(ReadStatus::Ready(raw_global(GameState::Play, 50))),
    )
    .menu(Ok(ReadStatus::Ready(RawMenu::Checksum(
        crate::model::memory::MenuChecksum {
            gamemode: 0,
            ranked_status: 4.0,
        },
    ))))
    .publish();

    let committed = bundle
        .apply_regular(regular)
        .await
        .expect("pre-object poll");
    assert_eq!(committed.context.reset_epoch, 0);
    assert_eq!(committed.publication.precise.key_overlay[0].count, 3);
    assert!(committed.publication.precise.key_overlay[0].is_pressed);
    assert_eq!(committed.publication.precise.hit_errors.all, [4.0]);
    assert_eq!(committed.publication.precise.committed_hit_error_index, 1);
    assert_eq!(bundle.calculation_reset_generation, 0);
}
#[tokio::test]
async fn old_map_timing_is_ignored_until_new_map_metadata_is_accepted() {
    let mut bundle = StateBundle::new(ClientType::Stable, 26, Arc::new(MismatchedCalculation));
    bundle.global.status = GameState::Play.value() as i64;
    bundle.global.play_time = 2_000.0;
    bundle.menu.checksum = Some("old".into());
    bundle.beatmap.checksum = Some("old".into());
    bundle.beatmap.first_object_time = Some(1_000);
    bundle.gameplay.is_default_state = false;
    bundle.gameplay.hit_errors = press_hit_errors(vec![7.0]);
    bundle.gameplay.total_hit_errors = 7.0;
    bundle.gameplay.previous_hit_error_index = 1;
    let new_map = RawRegularSnapshotBuilder::new(
        RegularCaptureContext::new(ClientType::Stable, 26, 0, 1),
        Ok(ReadStatus::Ready(raw_global(GameState::Play, 2_000))),
    )
    .menu(Ok(ReadStatus::Ready(RawMenu::Full(menu("new")))))
    .gameplay(Ok(ReadStatus::NotReady))
    .publish();

    let failed_metadata = bundle
        .apply_regular(new_map)
        .await
        .expect("mismatched metadata is rejected");
    assert_eq!(failed_metadata.context.reset_epoch, 0);
    assert_eq!(bundle.gameplay.hit_errors.all, [7.0]);
    assert_eq!(bundle.beatmap.checksum.as_deref(), Some("old"));

    bundle.beatmap.checksum = Some("new".into());
    bundle.beatmap.first_object_time = Some(5_000);
    let accepted_metadata = RawRegularSnapshotBuilder::new(
        RegularCaptureContext::new(ClientType::Stable, 26, 0, 2),
        Ok(ReadStatus::Ready(raw_global(GameState::Play, 2_000))),
    )
    .menu(Ok(ReadStatus::Ready(RawMenu::Checksum(
        crate::model::memory::MenuChecksum {
            gamemode: 0,
            ranked_status: 4.0,
        },
    ))))
    .gameplay(Ok(ReadStatus::NotReady))
    .publish();
    let accepted = bundle
        .apply_regular(accepted_metadata)
        .await
        .expect("accepted metadata does not end the play attempt");
    assert_eq!(accepted.context.reset_epoch, 0);
    assert_eq!(bundle.gameplay.hit_errors.all, [7.0]);
}

#[tokio::test]
async fn stale_regular_global_preserves_precise_time_but_updates_session_clock() {
    let mut bundle = StateBundle::new(ClientType::Stable, 27, Arc::new(EchoCalculation));
    bundle.global.status = GameState::Play.value() as i64;
    bundle.global.play_time = 200.0;
    let precise = RawPreciseSnapshotBuilder::new(
        PreciseCaptureContext::new(
            ClientType::Stable,
            27,
            0,
            0,
            default_calculated_mods(),
            0,
            0,
        ),
        1,
        Ok(ReadStatus::Ready(GlobalPrecise {
            status: GameState::Play.value() as i64,
            time: 250.0,
        })),
    )
    .publish();
    let precise_context = bundle.apply_precise_inner(&precise);
    assert_eq!(precise_context.latest_precise_global_sequence, 1);

    let mut global = raw_global(GameState::Play, 100);
    global.game_time = 523_409;
    let stale = RawRegularSnapshotBuilder::new(
        RegularCaptureContext::new(ClientType::Stable, 27, 0, 1)
            .with_base_precise_global_sequence(0),
        Ok(ReadStatus::Ready(global)),
    )
    .publish();
    let committed = bundle
        .apply_regular(stale)
        .await
        .expect("stale regular snapshot");

    assert_eq!(committed.context.play_time, 250.0);
    assert_eq!(committed.publication.regular.global.game_time, 523_409);
    assert_eq!(committed.context.latest_precise_global_sequence, 1);
    assert_eq!(committed.context.reset_epoch, 0);
}

#[tokio::test]
async fn empty_skin_folder_uses_node_path_normalization() {
    let mut bundle = StateBundle::new(ClientType::Lazer, 28, Arc::new(EchoCalculation));
    let mut global = raw_global(GameState::Menu, 0);
    global.skin_folder.clear();
    let snapshot = RawRegularSnapshotBuilder::new(
        RegularCaptureContext::new(ClientType::Lazer, 28, 0, 1),
        Ok(ReadStatus::Ready(global)),
    )
    .publish();

    bundle
        .apply_regular(snapshot)
        .await
        .expect("empty skin folder snapshot applies");

    assert_eq!(bundle.global.skin_folder, ".");
}

#[test]
fn precise_transition_validates_keys_appends_suffix_once_and_preserves_hits_on_exit() {
    let mut bundle = StateBundle::new(ClientType::Lazer, 3, Arc::new(EchoCalculation));
    bundle.global.status = GameState::Play.value() as i64;
    bundle.global.play_time = 200.0;
    bundle.gameplay.is_default_state = false;
    bundle.gameplay.mode = 0;
    let mut dt = default_calculated_mods();
    dt.number = 64;
    bundle.gameplay.mods = dt.clone();
    bundle.gameplay.unstable_rate = 42.0;
    let capture = PreciseCaptureContext::new(ClientType::Lazer, 3, 0, 0, dt.clone(), 0, 0);
    let precise = RawPreciseSnapshotBuilder::new(
        capture,
        1,
        Ok(ReadStatus::Ready(GlobalPrecise {
            status: GameState::Play.value() as i64,
            time: 250.0,
        })),
    )
    .key_overlay(Ok(ReadStatus::Ready(vec![KeyOverlayButton {
        name: "K1".into(),
        is_pressed: true,
        count: 1_000_001,
    }])))
    .hit_errors(Ok(ReadStatus::Ready(hit_error_suffix(2, vec![-5.0, 5.0]))))
    .publish();
    let context = bundle.apply_precise_inner(&precise);
    assert_eq!(bundle.gameplay.hit_errors.all, [-5.0, 5.0]);
    assert_eq!(bundle.gameplay.key_overlay[0].count, 0);
    assert_eq!(bundle.gameplay.keypresses[0].count, 0);
    assert_eq!(bundle.gameplay.unstable_rate, 42.0);
    let epoch = context.reset_epoch;
    assert!(!bundle.gameplay.key_overlay[0].is_pressed);
    assert!(!bundle.gameplay.keypresses[0].is_pressed);
    assert_eq!(
        (
            context.committed_hit_error_index,
            context.accepted_precise_sequence
        ),
        (2, 1)
    );

    let exit = RawPreciseSnapshotBuilder::new(
        PreciseCaptureContext::new(ClientType::Lazer, 3, context.reset_epoch, 0, dt, 2, 1),
        2,
        Ok(ReadStatus::Ready(GlobalPrecise {
            status: GameState::Menu.value() as i64,
            time: 0.0,
        })),
    )
    .publish();
    let exited = bundle.apply_precise_inner(&exit);
    assert_eq!(bundle.gameplay.key_overlay[0].count, 0);
    assert_eq!(exited.reset_epoch, epoch);
    assert_eq!(bundle.gameplay.hit_errors.all, [-5.0, 5.0]);
    assert_eq!(exited.committed_hit_error_index, 2);
    assert_eq!(exited.accepted_precise_sequence, 2);
}

#[test]
fn hit_event_list_rewind_preserves_history_and_resumes_appending() {
    for source in [ClientType::Stable, ClientType::Lazer] {
        let mut bundle = StateBundle::new(source, 3, Arc::new(EchoCalculation));
        bundle.global.status = GameState::Play.value() as i64;
        bundle.global.play_time = 24_301.0;
        bundle.gameplay.hit_errors = HitErrorArray {
            all: vec![-12.0, 5.0],
            press: vec![-12.0],
            release: vec![5.0],
        };
        bundle.gameplay.total_hit_errors = -7.0;
        bundle.gameplay.total_hit_error_squares = 169.0;
        bundle.gameplay.previous_hit_error_index = 10;
        bundle.gameplay.unstable_rate = 15.0;

        let rewind = RawPreciseSnapshotBuilder::new(
            PreciseCaptureContext::new(source, 3, 0, 0, default_calculated_mods(), 10, 0),
            1,
            Ok(ReadStatus::Ready(GlobalPrecise {
                status: GameState::Play.value() as i64,
                time: 24_301.0,
            })),
        )
        .hit_errors(Ok(ReadStatus::Ready(hit_error_suffix(2, Vec::new()))))
        .publish();
        let rewound = bundle.apply_precise_inner(&rewind);
        assert_eq!(bundle.gameplay.hit_errors.all, [-12.0, 5.0]);
        assert_eq!(bundle.gameplay.hit_errors.press, [-12.0]);
        assert_eq!(bundle.gameplay.hit_errors.release, [5.0]);
        assert_eq!(bundle.gameplay.total_hit_errors, -7.0);
        assert_eq!(bundle.gameplay.total_hit_error_squares, 169.0);
        assert_eq!(bundle.gameplay.unstable_rate, 15.0);
        assert_eq!(rewound.committed_hit_error_index, 2);

        let next = RawPreciseSnapshotBuilder::new(
            PreciseCaptureContext::new(source, 3, 0, 0, default_calculated_mods(), 2, 1),
            2,
            Ok(ReadStatus::Ready(GlobalPrecise {
                status: GameState::Play.value() as i64,
                time: 24_400.0,
            })),
        )
        .hit_errors(Ok(ReadStatus::Ready(hit_error_suffix(3, vec![-8.0]))))
        .publish();
        let appended = bundle.apply_precise_inner(&next);
        assert_eq!(bundle.gameplay.hit_errors.all, [-12.0, 5.0, -8.0]);
        assert_eq!(bundle.gameplay.hit_errors.press, [-12.0, -8.0]);
        assert_eq!(bundle.gameplay.hit_errors.release, [5.0]);
        assert_eq!(bundle.gameplay.total_hit_errors, -15.0);
        assert_eq!(bundle.gameplay.total_hit_error_squares, 233.0);
        assert_eq!(appended.committed_hit_error_index, 3);
    }
}

#[tokio::test]
async fn hit_history_appends_during_play_and_survives_pause_and_results_until_map_exit() {
    for source in [ClientType::Stable, ClientType::Lazer] {
        let mut bundle = StateBundle::new(source, 31, Arc::new(EchoCalculation));
        bundle.global.status = GameState::Play.value() as i64;
        bundle.global.play_time = 200.0;
        bundle.gameplay.is_default_state = false;
        bundle.menu.checksum = Some("early-hit".into());
        bundle.beatmap.checksum = Some("early-hit".into());
        bundle.beatmap.first_object_time = Some(400);
        let unreadable = RawPreciseSnapshotBuilder::new(
            PreciseCaptureContext::new(source, 31, 0, 0, default_calculated_mods(), 0, 0),
            1,
            Ok(ReadStatus::NotReady),
        )
        .hit_errors(Ok(ReadStatus::Ready(hit_error_suffix(1, vec![88.0]))))
        .publish();
        let unreadable = bundle.apply_precise(unreadable).await.unwrap();
        assert!(unreadable.publication.precise.hit_errors.all.is_empty());
        assert_eq!(unreadable.publication.precise.committed_hit_error_index, 0);

        let early = RawPreciseSnapshotBuilder::new(
            PreciseCaptureContext::new(source, 31, 0, 0, default_calculated_mods(), 0, 1),
            2,
            Ok(ReadStatus::Ready(GlobalPrecise {
                status: GameState::Play.value() as i64,
                time: 50.0,
            })),
        )
        .hit_errors(Ok(ReadStatus::Ready(hit_error_suffix(1, vec![-4.0]))))
        .publish();
        let played = bundle.apply_precise(early).await.unwrap();
        assert_eq!(played.publication.precise.hit_errors.all, [-4.0]);
        assert_eq!(played.publication.precise.hit_errors.press, [-4.0]);

        let mut paused_global = raw_global(GameState::Play, 50);
        paused_global.paused = Some(true);
        let paused = RawRegularSnapshotBuilder::new(
            RegularCaptureContext::new(source, 31, 0, 1).with_base_precise_global_sequence(2),
            Ok(ReadStatus::Ready(paused_global)),
        )
        .publish();
        let paused = bundle.apply_regular(paused).await.unwrap();
        assert_eq!(paused.publication.precise.hit_errors.all, [-4.0]);
        assert_eq!(paused.publication.precise.committed_hit_error_index, 1);

        let result = RawPreciseSnapshotBuilder::new(
            PreciseCaptureContext::new(source, 31, 0, 0, default_calculated_mods(), 1, 2),
            3,
            Ok(ReadStatus::Ready(GlobalPrecise {
                status: GameState::ResultScreen.value() as i64,
                time: 1_000.0,
            })),
        )
        .hit_errors(Ok(ReadStatus::Ready(hit_error_suffix(2, vec![99.0]))))
        .publish();
        let result = bundle.apply_precise(result).await.unwrap();
        assert_eq!(result.publication.precise.hit_errors.all, [-4.0]);
        assert_eq!(result.publication.precise.hit_errors.press, [-4.0]);
        assert_eq!(result.publication.precise.committed_hit_error_index, 1);

        let exit = RawRegularSnapshotBuilder::new(
            RegularCaptureContext::new(source, 31, 0, 2).with_base_precise_global_sequence(3),
            Ok(ReadStatus::Ready(raw_global(GameState::SelectPlay, 1_000))),
        )
        .publish();
        let exit = bundle.apply_regular(exit).await.unwrap();
        assert!(exit.publication.precise.hit_errors.all.is_empty());
        assert!(exit.publication.precise.hit_errors.press.is_empty());
        assert!(exit.publication.precise.hit_errors.release.is_empty());
        assert_eq!(exit.publication.precise.committed_hit_error_index, 0);
        assert_eq!(exit.publication.precise.total_hit_errors, 0.0);
    }
}

#[test]
fn boundary_corrections_reclassify_exact_prior_presses_in_timing_order() {
    let mut bundle = StateBundle::new(ClientType::Lazer, 3, Arc::new(EchoCalculation));
    bundle.global.status = GameState::Play.value() as i64;
    bundle.global.play_time = 200.0;
    bundle.gameplay.is_default_state = false;
    let simultaneous_tail_and_ordinary_error = -2.0;
    let all = vec![
        11.0,
        simultaneous_tail_and_ordinary_error,
        simultaneous_tail_and_ordinary_error,
        7.0,
        13.0,
    ];
    bundle.gameplay.hit_errors = press_hit_errors(all.clone());
    bundle.gameplay.total_hit_errors = all.iter().sum();
    bundle.gameplay.previous_hit_error_index = 40;
    let precise = RawPreciseSnapshotBuilder::new(
        PreciseCaptureContext::new(ClientType::Lazer, 3, 0, 0, default_calculated_mods(), 0, 40),
        1,
        Ok(ReadStatus::Ready(GlobalPrecise {
            status: GameState::Play.value() as i64,
            time: 250.0,
        })),
    )
    .hit_errors(Ok(ReadStatus::Ready(HitErrors {
        index: 42,
        errors: HitErrorArray::default(),
        release_corrections: vec![
            ReleaseCorrection {
                value: 7.0,
                press_offset: 1,
            },
            ReleaseCorrection {
                value: simultaneous_tail_and_ordinary_error,
                press_offset: 3,
            },
        ],
        stable_replay_frames: None,
    })))
    .publish();

    bundle.apply_precise_inner(&precise);

    assert_eq!(bundle.gameplay.hit_errors.all, all);
    assert_eq!(
        bundle.gameplay.hit_errors.press,
        [11.0, simultaneous_tail_and_ordinary_error, 13.0]
    );
    assert_eq!(
        bundle.gameplay.hit_errors.release,
        [simultaneous_tail_and_ordinary_error, 7.0]
    );
    assert_eq!(bundle.gameplay.hit_errors.press.len(), 3);
    assert_eq!(bundle.gameplay.hit_errors.release.len(), 2);
    assert_eq!(bundle.gameplay.total_hit_errors, 27.0);
    assert_eq!(bundle.gameplay.previous_hit_error_index, 42);
}

#[test]
fn invalid_boundary_corrections_leave_hit_errors_unchanged() {
    let mut bundle = StateBundle::new(ClientType::Lazer, 3, Arc::new(EchoCalculation));
    bundle.global.status = GameState::Play.value() as i64;
    bundle.global.play_time = 200.0;
    bundle.gameplay.is_default_state = false;
    let all = vec![1.0, -0.0, 3.0];
    bundle.gameplay.hit_errors = press_hit_errors(all.clone());
    bundle.gameplay.total_hit_errors = 4.0;
    bundle.gameplay.previous_hit_error_index = 8;
    let precise = RawPreciseSnapshotBuilder::new(
        PreciseCaptureContext::new(ClientType::Lazer, 3, 0, 0, default_calculated_mods(), 0, 8),
        1,
        Ok(ReadStatus::Ready(GlobalPrecise {
            status: GameState::Play.value() as i64,
            time: 250.0,
        })),
    )
    .hit_errors(Ok(ReadStatus::Ready(HitErrors {
        index: 10,
        errors: HitErrorArray::default(),
        release_corrections: vec![
            ReleaseCorrection {
                value: 0.0,
                press_offset: 1,
            },
            ReleaseCorrection {
                value: 3.0,
                press_offset: usize::MAX,
            },
        ],
        stable_replay_frames: None,
    })))
    .publish();

    bundle.apply_precise_inner(&precise);

    assert_eq!(bundle.gameplay.hit_errors.all, all);
    assert_eq!(bundle.gameplay.hit_errors.press, [1.0, -0.0, 3.0]);
    assert_eq!(
        bundle.gameplay.hit_errors.press[1].to_bits(),
        (-0.0_f64).to_bits()
    );
    assert_eq!(bundle.gameplay.hit_errors.press.len(), 3);
    assert!(bundle.gameplay.hit_errors.release.is_empty());
    assert_eq!(bundle.gameplay.total_hit_errors, 4.0);
    assert_eq!(bundle.gameplay.previous_hit_error_index, 10);
}

#[test]
fn stable_precise_keys_keep_legacy_projection_and_complete_keypresses() {
    let mut bundle = StateBundle::new(ClientType::Stable, 4, Arc::new(EchoCalculation));
    bundle.global.status = GameState::Play.value() as i64;
    bundle.global.play_time = 200.0;

    let snapshot = |sequence, count| {
        let keys = (1..=count)
            .map(|index| KeyOverlayButton {
                name: format!("K{index}").into(),
                is_pressed: index % 2 == 0,
                count: i64::from(index),
            })
            .collect();
        RawPreciseSnapshotBuilder::new(
            PreciseCaptureContext::new(
                ClientType::Stable,
                4,
                0,
                0,
                default_calculated_mods(),
                0,
                0,
            ),
            sequence,
            Ok(ReadStatus::Ready(GlobalPrecise {
                status: GameState::Play.value() as i64,
                time: 250.0,
            })),
        )
        .key_overlay(Ok(ReadStatus::Ready(keys)))
        .publish()
    };

    bundle.gameplay.keypress_mode = 3;
    bundle.apply_precise_inner(&snapshot(1, 3));
    assert!(bundle.gameplay.key_overlay.is_empty());
    assert_eq!(bundle.gameplay.keypresses.len(), 3);

    bundle.apply_precise_inner(&snapshot(2, 6));
    assert_eq!(bundle.gameplay.key_overlay.len(), 3);
    assert_eq!(bundle.gameplay.keypresses.len(), 6);
    assert_eq!(bundle.gameplay.key_overlay[2].name.as_ref(), "K3");
    assert_eq!(bundle.gameplay.keypresses[5].name.as_ref(), "K6");

    bundle.gameplay.keypress_mode = 0;
    bundle.apply_precise_inner(&snapshot(3, 6));
    assert_eq!(bundle.gameplay.key_overlay.len(), 4);
    assert_eq!(bundle.gameplay.keypresses.len(), 6);
    assert_eq!(bundle.gameplay.key_overlay[3].name.as_ref(), "K4");
}

#[test]
fn precise_keypress_timestamps_follow_observed_edges_and_reset_identity() {
    let mut bundle = StateBundle::new(ClientType::Stable, 5, Arc::new(EchoCalculation));
    bundle.gameplay.keypress_mode = 3;

    let snapshot = |sequence, time, keys: &[(&str, bool, i64)]| {
        RawPreciseSnapshotBuilder::new(
            PreciseCaptureContext::new(
                ClientType::Stable,
                5,
                0,
                0,
                default_calculated_mods(),
                0,
                0,
            ),
            sequence,
            Ok(ReadStatus::Ready(GlobalPrecise {
                status: GameState::Play.value() as i64,
                time,
            })),
        )
        .key_overlay(Ok(ReadStatus::Ready(
            keys.iter()
                .map(|(name, is_pressed, count)| KeyOverlayButton {
                    name: (*name).into(),
                    is_pressed: *is_pressed,
                    count: *count,
                })
                .collect(),
        )))
        .publish()
    };

    bundle.apply_precise_inner(&snapshot(1, 200.0, &[("K1", false, 0)]));
    assert_eq!(
        (
            bundle.gameplay.keypresses[0].pressed_at,
            bundle.gameplay.keypresses[0].released_at,
        ),
        (None, None)
    );

    bundle.apply_precise_inner(&snapshot(2, 208.0, &[("K1", true, 1)]));
    assert_eq!(
        (
            bundle.gameplay.keypresses[0].pressed_at,
            bundle.gameplay.keypresses[0].released_at,
        ),
        (Some(208.0), None)
    );

    bundle.apply_precise_inner(&snapshot(3, 216.0, &[("K1", true, 1)]));
    assert_eq!(bundle.gameplay.keypresses[0].pressed_at, Some(208.0));

    bundle.apply_precise_inner(&snapshot(4, 224.0, &[("K1", true, 2)]));
    assert_eq!(
        (
            bundle.gameplay.keypresses[0].count,
            bundle.gameplay.keypresses[0].pressed_at,
            bundle.gameplay.keypresses[0].released_at,
        ),
        (2, Some(208.0), None)
    );

    bundle.apply_precise_inner(&snapshot(5, 232.0, &[("K1", false, 2)]));
    assert_eq!(
        (
            bundle.gameplay.keypresses[0].pressed_at,
            bundle.gameplay.keypresses[0].released_at,
        ),
        (Some(208.0), Some(232.0))
    );

    bundle.gameplay.keypress_mode = 0;
    bundle.apply_precise_inner(&snapshot(6, 240.0, &[("K1", false, 2)]));
    assert_eq!(
        (
            bundle.gameplay.keypresses[0].pressed_at,
            bundle.gameplay.keypresses[0].released_at,
        ),
        (None, None)
    );

    bundle.apply_precise_inner(&snapshot(7, 248.0, &[("M1", true, 3)]));
    assert_eq!(
        (
            bundle.gameplay.keypresses[0].name.as_ref(),
            bundle.gameplay.keypresses[0].pressed_at,
            bundle.gameplay.keypresses[0].released_at,
        ),
        ("M1", Some(248.0), None)
    );

    bundle.reset_precise_epoch();
    assert_eq!(
        (
            bundle.gameplay.keypresses[0].pressed_at,
            bundle.gameplay.keypresses[0].released_at,
        ),
        (None, None)
    );
}

#[test]
fn stale_parsed_map_waits_for_current_mania_metadata() {
    let (checksum_a, parsed_a) = native_mania_map("A");
    let target_b = "stable-memory-checksum-b";
    let (source_b, parsed_b) = native_mania_map_with_identity("B", Some(target_b));
    assert_ne!(checksum_a, source_b);
    let mut bundle = StateBundle::new(ClientType::Stable, 44, Arc::new(EchoCalculation));
    bundle.global.status = GameState::Play.value() as i64;
    bundle.global.play_time = 250.0;
    bundle.menu.checksum = Some(target_b.into());
    bundle.beatmap.checksum = Some(checksum_a.clone());
    bundle.beatmap.source_key = Some(checksum_a);
    bundle.beatmap.parsed = Some(parsed_a);
    bundle.calculation_failures = 1;
    let frames = vec![
        StableReplayFrame {
            time: 200,
            keys: 2,
            y: 0.0,
        },
        StableReplayFrame {
            time: 390,
            keys: 0,
            y: 0.0,
        },
    ];
    let first = RawPreciseSnapshotBuilder::new(
        PreciseCaptureContext::new(
            ClientType::Stable,
            44,
            0,
            3,
            default_calculated_mods(),
            0,
            0,
        ),
        1,
        Ok(ReadStatus::Ready(GlobalPrecise {
            status: GameState::Play.value() as i64,
            time: 250.0,
        })),
    )
    .hit_errors(Ok(ReadStatus::Ready(HitErrors {
        index: 1,
        errors: HitErrorArray {
            all: vec![-10.0],
            press: vec![-10.0],
            release: Vec::new(),
        },
        release_corrections: Vec::new(),
        stable_replay_frames: Some(StableReplayFrameBatch {
            start_index: 0,
            index: 2,
            frames: std::sync::Arc::new(frames),
        }),
    })))
    .publish();

    bundle.apply_precise_inner(&first);
    assert_eq!(bundle.gameplay.hit_errors.all, [-10.0]);
    assert!(bundle.gameplay.hit_errors.press.is_empty());
    assert!(bundle.gameplay.hit_errors.release.is_empty());

    bundle.beatmap.checksum = Some(target_b.into());
    bundle.beatmap.source_key = Some(source_b);
    bundle.beatmap.parsed = Some(parsed_b);
    bundle.calculation_failures = 0;
    let second = RawPreciseSnapshotBuilder::new(
        PreciseCaptureContext::new(
            ClientType::Stable,
            44,
            0,
            3,
            default_calculated_mods(),
            1,
            1,
        ),
        2,
        Ok(ReadStatus::Ready(GlobalPrecise {
            status: GameState::Play.value() as i64,
            time: 258.0,
        })),
    )
    .hit_errors(Ok(ReadStatus::Ready(HitErrors {
        index: 1,
        errors: HitErrorArray::default(),
        release_corrections: Vec::new(),
        stable_replay_frames: Some(StableReplayFrameBatch {
            start_index: 2,
            index: 2,
            frames: std::sync::Arc::new(Vec::new()),
        }),
    })))
    .publish();

    bundle.apply_precise_inner(&second);
    assert!(bundle.gameplay.hit_errors.press.is_empty());
    assert_eq!(bundle.gameplay.hit_errors.release, [-10.0]);
}

#[test]
fn resolved_missing_mania_metadata_is_immediately_press_only() {
    let checksum = "missing-map";
    let mut bundle = StateBundle::new(ClientType::Stable, 45, Arc::new(EchoCalculation));
    bundle.global.status = GameState::Play.value() as i64;
    bundle.global.play_time = 250.0;
    bundle.menu.checksum = Some(checksum.into());
    bundle.beatmap.checksum = Some(checksum.into());
    bundle.previous_state_key = format!("{checksum}:3:");
    let snapshot = RawPreciseSnapshotBuilder::new(
        PreciseCaptureContext::new(
            ClientType::Stable,
            45,
            0,
            3,
            default_calculated_mods(),
            0,
            0,
        ),
        1,
        Ok(ReadStatus::Ready(GlobalPrecise {
            status: GameState::Play.value() as i64,
            time: 250.0,
        })),
    )
    .hit_errors(Ok(ReadStatus::Ready(HitErrors {
        index: 1,
        errors: HitErrorArray {
            all: vec![7.0],
            press: vec![7.0],
            release: Vec::new(),
        },
        release_corrections: Vec::new(),
        stable_replay_frames: Some(StableReplayFrameBatch {
            start_index: 0,
            index: 0,
            frames: std::sync::Arc::new(Vec::new()),
        }),
    })))
    .publish();

    bundle.apply_precise_inner(&snapshot);
    assert_eq!(bundle.gameplay.hit_errors.all, [7.0]);
    assert_eq!(bundle.gameplay.hit_errors.press, [7.0]);
    assert!(bundle.gameplay.hit_errors.release.is_empty());
}

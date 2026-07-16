use super::*;

#[test]
fn calculation_application_distinguishes_empty_and_applied_matches() {
    let mut bundle = StateBundle::new(ClientType::Stable, 31, Arc::new(EchoCalculation));
    let key = StateCalculationKey {
        instance_generation: 31,
        reset_epoch: 0,
        checksum: "map".into(),
        mode: 0,
        mods_checksum: String::new(),
    };
    let mods = CalculateMods {
        checksum: String::new(),
        number: 0,
        name: String::new(),
        array: Vec::new(),
        rate: 1.0,
    };

    let empty = bundle.apply_calculation(
        StateCalculationResult {
            key: key.clone(),
            beatmap: None,
            gameplay: None,
            result: None,
        },
        &key,
        &mods,
    );
    assert_eq!(empty, CalculationApplyOutcome::MatchedEmpty);

    let applied = bundle.apply_calculation(
        StateCalculationResult {
            key: key.clone(),
            beatmap: Some(BeatmapCalculationUpdate::default()),
            gameplay: None,
            result: None,
        },
        &key,
        &mods,
    );
    assert_eq!(applied, CalculationApplyOutcome::Applied);

    assert!(bundle.beatmap.pp_acc.values().all(|value| *value == 0.0));
    let calculated: BTreeMap<u8, f64> = (90..=100)
        .map(|accuracy| (accuracy, f64::from(accuracy)))
        .collect();
    bundle.set_calculate_pp(false);
    let disabled = bundle.apply_calculation(
        StateCalculationResult {
            key: key.clone(),
            beatmap: Some(BeatmapCalculationUpdate {
                pp_acc: Some(calculated.clone()),
                ..BeatmapCalculationUpdate::default()
            }),
            gameplay: None,
            result: None,
        },
        &key,
        &mods,
    );
    assert_eq!(disabled, CalculationApplyOutcome::Applied);
    assert!(bundle.beatmap.pp_acc.values().all(|value| *value == 0.0));

    bundle.set_calculate_pp(true);
    bundle.apply_calculation(
        StateCalculationResult {
            key: key.clone(),
            beatmap: Some(BeatmapCalculationUpdate {
                pp_acc: Some(calculated.clone()),
                ..BeatmapCalculationUpdate::default()
            }),
            gameplay: None,
            result: None,
        },
        &key,
        &mods,
    );
    assert_eq!(bundle.beatmap.pp_acc, calculated);

    bundle.set_calculate_pp(false);
    bundle.apply_calculation(
        StateCalculationResult {
            key,
            beatmap: Some(BeatmapCalculationUpdate {
                pp_acc: Some(default_pp_acc()),
                ..BeatmapCalculationUpdate::default()
            }),
            gameplay: None,
            result: None,
        },
        &StateCalculationKey {
            instance_generation: 31,
            reset_epoch: 0,
            checksum: "map".into(),
            mode: 0,
            mods_checksum: String::new(),
        },
        &mods,
    );
    assert_eq!(bundle.beatmap.pp_acc, calculated);
}

#[test]
fn calculation_uses_authored_beatmap_mode_for_converted_standard_maps() {
    let mut bundle = StateBundle::new(ClientType::Lazer, 35, Arc::new(EchoCalculation));
    let expected = StateCalculationKey {
        instance_generation: 35,
        reset_epoch: 0,
        checksum: "converted-standard".into(),
        mode: 3,
        mods_checksum: String::new(),
    };
    let mods = default_calculated_mods();
    let outcome = bundle.apply_calculation(
        StateCalculationResult {
            key: expected.clone(),
            beatmap: Some(BeatmapCalculationUpdate {
                original_mode: Some(0),
                ..BeatmapCalculationUpdate::default()
            }),
            gameplay: None,
            result: None,
        },
        &expected,
        &mods,
    );
    assert_eq!(outcome, CalculationApplyOutcome::Applied);
    assert_eq!(bundle.beatmap.mode, Some(0));
}

#[test]
fn event_only_calculation_retains_map_projection_and_keeps_state_key_retryable() {
    let mut bundle = StateBundle::new(ClientType::Stable, 36, Arc::new(EchoCalculation));
    bundle.beatmap.checksum = Some("retained".into());
    bundle.beatmap.mode = Some(2);
    bundle.beatmap.full_pp = 42.0;
    bundle.beatmap.graph = Arc::new(StrainGraph {
        primary: vec![1.0],
        series: Vec::new(),
        x_axis: vec![2.0],
    });
    let retained_graph = bundle.beatmap.graph.clone();
    let expected = StateCalculationKey {
        instance_generation: 36,
        reset_epoch: 0,
        checksum: "unready".into(),
        mode: 3,
        mods_checksum: String::new(),
    };
    let event_status = EventStatus {
        bpm: 180.0,
        kiai: true,
        break_time: true,
    };
    let outcome = bundle.apply_calculation(
        StateCalculationResult {
            key: expected.clone(),
            beatmap: Some(BeatmapCalculationUpdate {
                event_only: true,
                retry_state_key: true,
                event_status,
                ..BeatmapCalculationUpdate::default()
            }),
            gameplay: None,
            result: None,
        },
        &expected,
        &default_calculated_mods(),
    );
    assert_eq!(outcome, CalculationApplyOutcome::AppliedRetryable);
    assert_eq!(bundle.beatmap.checksum.as_deref(), Some("retained"));
    assert_eq!(bundle.beatmap.mode, Some(2));
    assert_eq!(bundle.beatmap.full_pp, 42.0);
    assert!(Arc::ptr_eq(&bundle.beatmap.graph, &retained_graph));
    assert_eq!(bundle.beatmap.event_status, event_status);
}

#[test]
fn graph_event_only_calculation_repads_graph_without_replacing_map_metadata() {
    let mut bundle = StateBundle::new(ClientType::Stable, 37, Arc::new(EchoCalculation));
    bundle.beatmap.checksum = Some("retained".into());
    bundle.beatmap.mode = Some(0);
    bundle.beatmap.full_pp = 42.0;
    bundle.beatmap.source_key = Some("retained-source".into());
    let graph = Arc::new(StrainGraph {
        primary: vec![3.0],
        series: Vec::new(),
        x_axis: vec![4.0],
    });
    let expected = StateCalculationKey {
        instance_generation: 37,
        reset_epoch: 0,
        checksum: "theme".into(),
        mode: 3,
        mods_checksum: String::new(),
    };
    let event_status = EventStatus {
        bpm: 120.0,
        kiai: false,
        break_time: true,
    };
    let outcome = bundle.apply_calculation(
        StateCalculationResult {
            key: expected.clone(),
            beatmap: Some(BeatmapCalculationUpdate {
                graph_event_only: true,
                graph: graph.clone(),
                graph_mp3_length: Some(90_000.5),
                event_status,
                ..BeatmapCalculationUpdate::default()
            }),
            gameplay: None,
            result: None,
        },
        &expected,
        &default_calculated_mods(),
    );
    assert_eq!(outcome, CalculationApplyOutcome::Applied);
    assert_eq!(bundle.beatmap.checksum.as_deref(), Some("retained"));
    assert_eq!(bundle.beatmap.mode, Some(0));
    assert_eq!(bundle.beatmap.full_pp, 42.0);
    assert_eq!(
        bundle.beatmap.source_key.as_deref(),
        Some("retained-source")
    );
    assert!(Arc::ptr_eq(&bundle.beatmap.graph, &graph));
    assert_eq!(bundle.beatmap.graph_mp3_length, Some(90_000.5));
    assert_eq!(bundle.beatmap.event_status, event_status);
}

#[test]
fn parsed_background_override_is_applied_only_for_stable() {
    for source in [ClientType::Stable, ClientType::Lazer] {
        let mut bundle = StateBundle::new(source, 38, Arc::new(EchoCalculation));
        bundle.menu.background_filename = Some("memory-bg.jpg".into());
        let expected = StateCalculationKey {
            instance_generation: 38,
            reset_epoch: 0,
            checksum: "map".into(),
            mode: 0,
            mods_checksum: String::new(),
        };
        let outcome = bundle.apply_calculation(
            StateCalculationResult {
                key: expected.clone(),
                beatmap: Some(BeatmapCalculationUpdate {
                    background_filename: Some("events/bg.jpg".into()),
                    ..BeatmapCalculationUpdate::default()
                }),
                gameplay: None,
                result: None,
            },
            &expected,
            &default_calculated_mods(),
        );
        assert_eq!(outcome, CalculationApplyOutcome::Applied);
        assert_eq!(
            bundle.menu.background_filename.as_deref(),
            Some(if source == ClientType::Stable {
                "events/bg.jpg"
            } else {
                "memory-bg.jpg"
            })
        );
    }
}

#[tokio::test]
async fn regular_global_refreshes_precise_projection_only_when_timing_changes() {
    let mut bundle = StateBundle::new(ClientType::Lazer, 33, Arc::new(EchoCalculation));
    let precise = RawPreciseSnapshotBuilder::new(
        PreciseCaptureContext::new(ClientType::Lazer, 33, 0, 0, default_calculated_mods(), 0, 0),
        1,
        Ok(ReadStatus::Ready(GlobalPrecise {
            status: GameState::Menu.value() as i64,
            time: 100.25,
        })),
    )
    .publish();
    let precise_publication = bundle
        .apply_precise(precise)
        .await
        .expect("precise transition")
        .publication;
    let original_precise = precise_publication.precise.clone();

    let mut changed_global = raw_global(GameState::Menu, 0);
    changed_global.play_time = 200.5;
    let changed = RawRegularSnapshotBuilder::new(
        RegularCaptureContext::new(ClientType::Lazer, 33, 0, 1)
            .with_base_precise_global_sequence(1),
        Ok(ReadStatus::Ready(changed_global)),
    )
    .publish();
    let changed_publication = bundle
        .apply_regular(changed)
        .await
        .expect("changed regular transition")
        .publication;
    assert_eq!(changed_publication.precise.play_time, 200.5);
    assert!(!Arc::ptr_eq(
        &original_precise,
        &changed_publication.precise
    ));

    let unchanged_precise = changed_publication.precise.clone();
    let mut same_global = raw_global(GameState::Menu, 0);
    same_global.play_time = 200.5;
    let same = RawRegularSnapshotBuilder::new(
        RegularCaptureContext::new(ClientType::Lazer, 33, 0, 2)
            .with_base_precise_global_sequence(1),
        Ok(ReadStatus::Ready(same_global)),
    )
    .publish();
    let same_publication = bundle
        .apply_regular(same)
        .await
        .expect("unchanged regular transition")
        .publication;
    assert!(Arc::ptr_eq(&unchanged_precise, &same_publication.precise));
}

#[tokio::test]
async fn regular_state_preserves_scores_above_javascript_safe_integer_range() {
    let mut bundle = StateBundle::new(ClientType::Lazer, 34, Arc::new(EchoCalculation));
    bundle.set_calculate_pp(false);
    let score_value = 9_007_199_254_740_994.0_f64;
    let mut gameplay = score(0, 10);
    gameplay.score = score_value;
    let mut leaderboard_player = raw_leaderboard_player();
    leaderboard_player.score = score_value;
    let play = RawRegularSnapshotBuilder::new(
        RegularCaptureContext::new(ClientType::Lazer, 34, 0, 1),
        Ok(ReadStatus::Ready(raw_global(GameState::Play, 500))),
    )
    .gameplay(Ok(ReadStatus::Ready(gameplay)))
    .leaderboard(Ok(ReadStatus::Ready((
        true,
        None,
        vec![leaderboard_player],
    ))))
    .publish();
    let play_publication = bundle
        .apply_regular(play)
        .await
        .expect("large gameplay score")
        .publication;
    assert_eq!(
        play_publication.regular.gameplay.score.to_bits(),
        score_value.to_bits()
    );
    assert_eq!(
        play_publication.regular.gameplay.leaderboard_scores[0]
            .score
            .to_bits(),
        score_value.to_bits()
    );

    let result = RawResultScreen {
        online_id: 1.0,
        player_name: "player".into(),
        mods: default_calculated_mods(),
        mode: 0,
        accuracy: 100.0,
        max_combo: 10,
        score: score_value,
        statistics: default_statistics(),
        maximum_statistics: default_statistics(),
        date: String::new(),
    };
    let result_snapshot = RawRegularSnapshotBuilder::new(
        RegularCaptureContext::new(ClientType::Lazer, 34, 0, 2),
        Ok(ReadStatus::Ready(raw_global(GameState::ResultScreen, 600))),
    )
    .result_screen(Ok(ReadStatus::Ready(result)))
    .publish();
    let result_publication = bundle
        .apply_regular(result_snapshot)
        .await
        .expect("large result score")
        .publication;
    assert_eq!(
        result_publication.regular.result_screen.raw.score.to_bits(),
        score_value.to_bits()
    );
}

#[tokio::test]
async fn regular_and_precise_state_preserve_fractional_and_nonfinite_times() {
    let mut bundle = StateBundle::new(ClientType::Lazer, 32, Arc::new(EchoCalculation));
    let mut raw = raw_global(GameState::Menu, 0);
    raw.play_time = f64::NAN;
    let regular = RawRegularSnapshotBuilder::new(
        RegularCaptureContext::new(ClientType::Lazer, 32, 0, 1),
        Ok(ReadStatus::Ready(raw)),
    )
    .publish();
    let committed = bundle
        .apply_regular(regular)
        .await
        .expect("non-finite regular time");
    assert!(committed.context.play_time.is_nan());
    assert!(committed.publication.regular.global.play_time.is_nan());

    let precise = RawPreciseSnapshotBuilder::new(
        PreciseCaptureContext::new(
            ClientType::Lazer,
            32,
            committed.context.reset_epoch,
            committed.context.mode,
            committed.context.mods.clone(),
            committed.context.committed_hit_error_index,
            committed.context.accepted_precise_sequence,
        ),
        1,
        Ok(ReadStatus::Ready(GlobalPrecise {
            status: GameState::Menu.value() as i64,
            time: f64::INFINITY,
        })),
    )
    .publish();
    let precise_context = bundle.apply_precise_inner(&precise);
    assert_eq!(precise_context.play_time, f64::INFINITY);
    assert_eq!(bundle.global.play_time, f64::INFINITY);

    bundle.global.play_time = 123.25;
    let snapshot = bundle.materialize_current();
    assert_eq!(snapshot.regular.global.play_time, 123.25);
}

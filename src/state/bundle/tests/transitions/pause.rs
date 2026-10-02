use super::*;

#[tokio::test]
async fn lazer_pause_preserves_score_and_applies_final_judgements() {
    let mut bundle = StateBundle::new(ClientType::Lazer, 11, Arc::new(EchoCalculation));
    let mut active_score = score(0, 50);
    active_score.statistics.great = 50;
    let active = RawRegularSnapshotBuilder::new(
        RegularCaptureContext::new(ClientType::Lazer, 11, 0, 1),
        Ok(ReadStatus::Ready(raw_global(GameState::Play, 500))),
    )
    .gameplay(Ok(ReadStatus::Ready(active_score.clone())))
    .publish();
    bundle.apply_regular(active).await.expect("active gameplay");

    let mut paused_score = active_score;
    paused_score.statistics.miss = 1;
    paused_score.combo = 0;
    paused_score.accuracy = 98.0;
    for (sequence, sample) in [
        (2, ReadStatus::Ready(paused_score)),
        (3, ReadStatus::NotReady),
    ] {
        let paused = RawRegularSnapshotBuilder::new(
            RegularCaptureContext::new(ClientType::Lazer, 11, 0, sequence),
            Ok(ReadStatus::Ready(raw_global_paused(
                GameState::Play,
                500,
                true,
            ))),
        )
        .gameplay(Ok(sample))
        .publish();
        let paused = bundle.apply_regular(paused).await.expect("paused gameplay");
        let gameplay = &paused.publication.regular.gameplay;
        assert!(paused.publication.regular.global.paused);
        assert_eq!(gameplay.player_name, "player");
        assert_eq!(gameplay.score, 123.0);
        assert_eq!(gameplay.accuracy, 98.0);
        assert_eq!(gameplay.statistics.great, 50);
        assert_eq!(gameplay.statistics.miss, 1);
        assert_eq!(gameplay.combo, 0);
        assert_eq!(gameplay.max_combo, 50);
        assert_eq!(gameplay.grade_current, "A");
    }

    let resumed = RawRegularSnapshotBuilder::new(
        RegularCaptureContext::new(ClientType::Lazer, 11, 0, 4),
        Ok(ReadStatus::Ready(raw_global_paused(
            GameState::Play,
            600,
            false,
        ))),
    )
    .gameplay(Ok(ReadStatus::Ready(score(0, 1))))
    .publish();
    let resumed = bundle
        .apply_regular(resumed)
        .await
        .expect("resumed gameplay");
    assert!(!resumed.publication.regular.global.paused);
    assert_eq!(resumed.publication.regular.gameplay.combo, 1);
}

#[tokio::test]
async fn play_start_clock_hold_does_not_report_paused() {
    let mut bundle = StateBundle::new(ClientType::Lazer, 101, Arc::new(EchoCalculation));
    let enter = RawRegularSnapshotBuilder::new(
        RegularCaptureContext::new(ClientType::Lazer, 101, 0, 1),
        Ok(ReadStatus::Ready(raw_global(GameState::Play, 5))),
    )
    .publish();
    let enter = bundle.apply_regular(enter).await.expect("enter play");
    assert!(!enter.publication.regular.global.paused);

    let held = RawRegularSnapshotBuilder::new(
        RegularCaptureContext::new(ClientType::Lazer, 101, 0, 2),
        Ok(ReadStatus::Ready(raw_global(GameState::Play, 5))),
    )
    .publish();
    let held = bundle.apply_regular(held).await.expect("held play clock");
    assert!(
        !held.publication.regular.global.paused,
        "pre-start/retry clock hold must not look like pause"
    );
}

#[tokio::test]
async fn play_retry_rewind_disarms_stall_paused_until_time_moves() {
    let mut bundle = StateBundle::new(ClientType::Stable, 102, Arc::new(EchoCalculation));
    for (seq, time) in [(1, 100), (2, 200), (3, 300)] {
        let snap = RawRegularSnapshotBuilder::new(
            RegularCaptureContext::new(ClientType::Stable, 102, 0, seq),
            Ok(ReadStatus::Ready(raw_global(GameState::Play, time))),
        )
        .publish();
        let applied = bundle
            .apply_regular(snap)
            .await
            .expect("advance play clock");
        assert!(!applied.publication.regular.global.paused);
    }

    let mid_pause = RawRegularSnapshotBuilder::new(
        RegularCaptureContext::new(ClientType::Stable, 102, 0, 4),
        Ok(ReadStatus::Ready(raw_global(GameState::Play, 300))),
    )
    .publish();
    let mid_pause = bundle
        .apply_regular(mid_pause)
        .await
        .expect("armed stall is pause");
    assert!(mid_pause.publication.regular.global.paused);

    let retry = RawRegularSnapshotBuilder::new(
        RegularCaptureContext::new(ClientType::Stable, 102, 0, 5),
        Ok(ReadStatus::Ready(raw_global(GameState::Play, 5))),
    )
    .publish();
    let retry = bundle.apply_regular(retry).await.expect("retry rewind");
    assert!(!retry.publication.regular.global.paused);

    let retry_hold = RawRegularSnapshotBuilder::new(
        RegularCaptureContext::new(ClientType::Stable, 102, 0, 6),
        Ok(ReadStatus::Ready(raw_global(GameState::Play, 5))),
    )
    .publish();
    let retry_hold = bundle.apply_regular(retry_hold).await.expect("retry hold");
    assert!(
        !retry_hold.publication.regular.global.paused,
        "retry clock hold must stay unpaused until time advances again"
    );
}

#[tokio::test]
async fn lazer_pause_then_select_play_resets_retained_gameplay_epoch() {
    let mut bundle = StateBundle::new(ClientType::Lazer, 12, Arc::new(EchoCalculation));
    let mut dt = default_calculated_mods();
    dt.number = 64;
    dt.name = "DT".into();
    dt.checksum = "dt-1.1".into();
    dt.rate = 1.1;
    dt.array = vec![crate::model::states::Mod {
        acronym: "DT".into(),
        settings: Some(
            [("speed_change".into(), serde_json::json!(1.1))]
                .into_iter()
                .collect(),
        ),
    }];
    let mut active_score = score(3, 50);
    active_score.mods = dt.clone();
    let active = RawRegularSnapshotBuilder::new(
        RegularCaptureContext::new(ClientType::Lazer, 12, 0, 1),
        Ok(ReadStatus::Ready(raw_global(GameState::Play, 500))),
    )
    .gameplay(Ok(ReadStatus::Ready(active_score)))
    .publish();
    bundle
        .apply_regular(active)
        .await
        .expect("active lazer gameplay");

    bundle.gameplay.hit_errors = HitErrorArray {
        all: vec![4.0, -6.0],
        press: vec![4.0],
        release: vec![-6.0],
    };
    bundle.gameplay.total_hit_errors = -2.0;
    bundle.gameplay.previous_hit_error_index = 2;
    bundle.gameplay.key_overlay = vec![KeyOverlayButton {
        name: "K1".into(),
        is_pressed: true,
        count: 7,
    }];
    bundle.gameplay.keypresses = vec![KeypressButton {
        name: "K1".into(),
        is_pressed: true,
        count: 7,
        pressed_at: Some(495.0),
        released_at: None,
    }];
    bundle.gameplay.keypress_identity_mode = Some(3);
    bundle.gameplay.is_key_overlay_default_state = false;
    bundle.precise_dirty = true;
    let mut paused_score = score(3, 50);
    paused_score.mods = dt.clone();

    let paused = RawRegularSnapshotBuilder::new(
        RegularCaptureContext::new(ClientType::Lazer, 12, 0, 2),
        Ok(ReadStatus::Ready(raw_global_paused(
            GameState::Play,
            500,
            true,
        ))),
    )
    .gameplay(Ok(ReadStatus::Ready(paused_score)))
    .publish();
    let paused = bundle
        .apply_regular(paused)
        .await
        .expect("paused lazer gameplay");

    assert!(paused.publication.regular.global.paused);
    assert_eq!(paused.publication.regular.gameplay.mode, 3);
    assert_eq!(paused.publication.regular.gameplay.mods, dt);
    assert_eq!(paused.publication.regular.gameplay.mods.rate, 1.1);
    assert_eq!(paused.publication.precise.mode, 3);
    assert_eq!(
        paused.publication.precise.hit_errors.all.as_slice(),
        &[4.0, -6.0]
    );
    assert_eq!(
        paused.publication.precise.hit_errors.press.as_slice(),
        &[4.0]
    );
    assert_eq!(
        paused.publication.precise.hit_errors.release.as_slice(),
        &[-6.0]
    );
    assert_eq!(paused.publication.precise.committed_hit_error_index, 2);
    assert_eq!(paused.publication.precise.key_overlay.len(), 1);
    assert!(paused.publication.precise.key_overlay[0].is_pressed);
    assert_eq!(paused.publication.precise.key_overlay[0].count, 7);
    assert_eq!(paused.publication.precise.keypresses.len(), 1);
    assert!(paused.publication.precise.keypresses[0].is_pressed);
    assert_eq!(paused.publication.precise.keypresses[0].count, 7);
    assert_eq!(
        paused.publication.precise.keypresses[0].pressed_at,
        Some(495.0)
    );
    let paused_epoch = paused.context.reset_epoch;

    let select_play = RawRegularSnapshotBuilder::new(
        RegularCaptureContext::new(ClientType::Lazer, 12, paused_epoch, 3),
        Ok(ReadStatus::Ready(raw_global(GameState::SelectPlay, 0))),
    )
    .publish();
    let select_play = bundle
        .apply_regular(select_play)
        .await
        .expect("SelectPlay after paused lazer gameplay");

    assert_eq!(select_play.publication.regular.gameplay.mode, 0);
    assert_eq!(
        select_play.publication.regular.gameplay.mods,
        default_calculated_mods()
    );
    assert_eq!(select_play.publication.precise.mode, 0);
    assert!(select_play.publication.precise.hit_errors.all.is_empty());
    assert!(select_play.publication.precise.hit_errors.press.is_empty());
    assert!(
        select_play
            .publication
            .precise
            .hit_errors
            .release
            .is_empty()
    );
    assert_eq!(select_play.publication.precise.committed_hit_error_index, 0);
    assert!(select_play.publication.precise.key_overlay.is_empty());
    assert!(select_play.publication.precise.keypresses.is_empty());
    assert!(select_play.context.reset_epoch > paused_epoch);
}

#[tokio::test]
async fn scoreless_lazer_pause_marks_epoch_for_select_play_reset() {
    let mut bundle = StateBundle::new(ClientType::Lazer, 24, Arc::new(EchoCalculation));
    let initial = RawRegularSnapshotBuilder::new(
        RegularCaptureContext::new(ClientType::Lazer, 24, 0, 1),
        Ok(ReadStatus::Ready(raw_global(GameState::Play, 500))),
    )
    .gameplay(Ok(ReadStatus::NotReady))
    .publish();
    bundle
        .apply_regular(initial)
        .await
        .expect("initial scoreless lazer gameplay");

    bundle.gameplay.mode = 3;
    bundle.gameplay.keypress_mode = 3;
    bundle.gameplay.mods.checksum = "seeded-mods".into();
    bundle.gameplay.hit_errors = HitErrorArray {
        all: vec![5.0, -7.0],
        press: vec![5.0],
        release: vec![-7.0],
    };
    bundle.gameplay.total_hit_errors = -2.0;
    bundle.gameplay.previous_hit_error_index = 2;
    bundle.gameplay.key_overlay = vec![KeyOverlayButton {
        name: "K1".into(),
        is_pressed: true,
        count: 4,
    }];
    bundle.gameplay.keypresses = vec![KeypressButton {
        name: "K1".into(),
        is_pressed: true,
        count: 4,
        pressed_at: Some(498.0),
        released_at: None,
    }];
    bundle.gameplay.keypress_identity_mode = Some(3);
    bundle.gameplay.is_key_overlay_default_state = false;
    bundle.precise_dirty = true;

    let paused = RawRegularSnapshotBuilder::new(
        RegularCaptureContext::new(ClientType::Lazer, 24, 0, 2),
        Ok(ReadStatus::Ready(raw_global_paused(
            GameState::Play,
            500,
            true,
        ))),
    )
    .gameplay(Ok(ReadStatus::NotReady))
    .publish();
    let paused = bundle
        .apply_regular(paused)
        .await
        .expect("paused scoreless lazer gameplay");

    assert!(paused.publication.regular.global.paused);
    assert!(!bundle.gameplay.is_default_state);
    assert_eq!(paused.publication.precise.mode, 3);
    assert_eq!(
        paused.publication.precise.hit_errors.all.as_slice(),
        &[5.0, -7.0]
    );
    assert_eq!(
        paused.publication.precise.hit_errors.press.as_slice(),
        &[5.0]
    );
    assert_eq!(
        paused.publication.precise.hit_errors.release.as_slice(),
        &[-7.0]
    );
    assert_eq!(paused.publication.precise.committed_hit_error_index, 2);
    assert_eq!(paused.publication.precise.key_overlay.len(), 1);
    assert!(paused.publication.precise.key_overlay[0].is_pressed);
    assert_eq!(paused.publication.precise.key_overlay[0].count, 4);
    assert_eq!(paused.publication.precise.keypresses.len(), 1);
    assert!(paused.publication.precise.keypresses[0].is_pressed);
    assert_eq!(paused.publication.precise.keypresses[0].count, 4);
    assert_eq!(
        paused.publication.precise.keypresses[0].pressed_at,
        Some(498.0)
    );
    let paused_epoch = paused.context.reset_epoch;

    let select_play = RawRegularSnapshotBuilder::new(
        RegularCaptureContext::new(ClientType::Lazer, 24, paused_epoch, 3),
        Ok(ReadStatus::Ready(raw_global(GameState::SelectPlay, 0))),
    )
    .publish();
    let select_play = bundle
        .apply_regular(select_play)
        .await
        .expect("SelectPlay after paused scoreless lazer gameplay");

    assert!(select_play.context.reset_epoch > paused_epoch);
    assert_eq!(select_play.publication.regular.gameplay.mode, 0);
    assert_eq!(
        select_play.publication.regular.gameplay.mods,
        default_calculated_mods()
    );
    assert_eq!(select_play.publication.precise.mode, 0);
    assert!(select_play.publication.precise.hit_errors.all.is_empty());
    assert!(select_play.publication.precise.hit_errors.press.is_empty());
    assert!(
        select_play
            .publication
            .precise
            .hit_errors
            .release
            .is_empty()
    );
    assert_eq!(select_play.publication.precise.committed_hit_error_index, 0);
    assert!(select_play.publication.precise.key_overlay.is_empty());
    assert!(select_play.publication.precise.keypresses.is_empty());
    assert_eq!(bundle.gameplay.keypress_identity_mode, None);
}

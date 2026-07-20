use super::*;

#[test]
fn state_defaults_and_retry_retention_match_source_contract() {
    let settings = SettingsState::default();
    assert_eq!(settings.values["mania"]["scrollDirection"], 1);
    assert_eq!(settings.values["keybinds"]["osu"]["k1"], "");
    let snapshot = StateBundle::new(ClientType::Stable, 9, Arc::new(EchoCalculation)).snapshot();
    assert!(snapshot.regular.menu.checksum.is_none());
    assert!(snapshot.regular.user.profile.is_none());
    assert_eq!(snapshot.precise.status, GameState::Menu.value() as i64);
    let mut configured = StateBundle::new(ClientType::Stable, 10, Arc::new(EchoCalculation));
    configured.set_capture_config(false, true);
    configured.gameplay.mode = 3;
    configured.gameplay.keypress_mode = 3;
    assert_eq!(configured.materialize_current().precise.mode, 3);
    let plan = configured.capture_plan();
    assert!(!plan.read_mania_scroll_speed);
    assert!(plan.show_mp_commands);

    let mut gameplay = GameplayState::default();
    assert_eq!(gameplay.grade_current, "");
    assert_eq!(gameplay.grade_expected, "");
    gameplay.is_default_state = false;
    gameplay.retries = 8;
    gameplay.mods.array = vec![crate::model::states::Mod {
        acronym: "HD".into(),
        settings: None,
    }];
    gameplay.player_name = "retained".into();
    gameplay.mode = 3;
    gameplay.keypress_mode = 3;
    gameplay.mods.checksum = "mods".into();
    gameplay.leaderboard_scores.push(raw_leaderboard_player());
    gameplay.hit_errors.all.push(10.0);
    gameplay.reset(true, ClientType::Stable);
    assert_eq!(
        (
            gameplay.retries,
            gameplay.player_name.as_str(),
            gameplay.mode,
            gameplay.keypress_mode,
        ),
        (8, "retained", 3, 3)
    );
    assert_eq!(gameplay.mods.checksum, "mods");
    assert_eq!(gameplay.leaderboard_scores.len(), 1);
    let expected_retry_grade = calculate_grade(
        false,
        &gameplay.mods.array,
        gameplay.mode as i32,
        gameplay.accuracy,
        &default_statistics(),
    );
    assert_eq!(gameplay.grade_current, expected_retry_grade);
    assert_eq!(gameplay.grade_expected, expected_retry_grade);
    let mut non_retry = gameplay.clone();
    let expected_non_retry_grade = calculate_grade(
        false,
        &non_retry.mods.array,
        non_retry.mode as i32,
        100.0,
        &default_statistics(),
    );
    non_retry.reset(false, ClientType::Stable);
    assert_eq!(non_retry.mode, 0);
    assert_eq!(non_retry.grade_current, expected_non_retry_grade);
    assert_eq!(non_retry.grade_expected, expected_non_retry_grade);

    assert_eq!(gameplay.hit_errors, HitErrorArray::default());
}

#[test]
fn unstable_rate_uses_all_errors_including_releases() {
    let gameplay = GameplayState {
        hit_errors: HitErrorArray {
            all: vec![0.0, 10.0],
            press: vec![0.0],
            release: vec![10.0],
        },
        total_hit_errors: 10.0,
        ..GameplayState::default()
    };

    assert_eq!(gameplay.calculate_ur(), 50.0);
}

#[tokio::test]
async fn user_profile_initializes_and_retains_last_connected_value() {
    let mut bundle = StateBundle::new(ClientType::Lazer, 10, Arc::new(EchoCalculation));
    let mut user = RawUser {
        name: "Guest".into(),
        accuracy: 100.0,
        ranked_score: 1.0,
        id: 1,
        level: 1.0,
        play_count: 1,
        play_mode: 0,
        rank: 1,
        country_code: 1,
        performance_points: 1.0,
        raw_bancho_status: 0,
        background_colour: -1,
        raw_login_status: 0,
        matchmaking: None,
    };
    let initial = RawRegularSnapshotBuilder::new(
        RegularCaptureContext::new(ClientType::Lazer, 10, 0, 1),
        Ok(ReadStatus::Ready(raw_global(GameState::Menu, 0))),
    )
    .user(Ok(ReadStatus::Ready(user.clone())))
    .publish();
    let initial = bundle
        .apply_regular(initial)
        .await
        .expect("initial guest user transition");
    assert_eq!(
        initial
            .publication
            .regular
            .user
            .profile
            .as_ref()
            .map(|u| u.name.as_str()),
        Some("Guest"),
    );

    user.name = "player".into();
    user.raw_login_status = 65_793;
    let connected = RawRegularSnapshotBuilder::new(
        RegularCaptureContext::new(ClientType::Lazer, 10, 0, 2),
        Ok(ReadStatus::Ready(raw_global(GameState::Menu, 0))),
    )
    .user(Ok(ReadStatus::Ready(user.clone())))
    .publish();
    let connected = bundle
        .apply_regular(connected)
        .await
        .expect("connected user transition");
    assert_eq!(
        connected
            .publication
            .regular
            .user
            .profile
            .as_ref()
            .map(|u| u.name.as_str()),
        Some("player"),
    );

    user.name = "Guest".into();
    user.raw_login_status = 0;
    let reconnecting = RawRegularSnapshotBuilder::new(
        RegularCaptureContext::new(ClientType::Lazer, 10, 0, 3),
        Ok(ReadStatus::Ready(raw_global(GameState::Menu, 0))),
    )
    .user(Ok(ReadStatus::Ready(user)))
    .publish();
    let reconnecting = bundle
        .apply_regular(reconnecting)
        .await
        .expect("reconnecting user transition");
    assert_eq!(
        reconnecting
            .publication
            .regular
            .user
            .profile
            .as_ref()
            .map(|u| u.name.as_str()),
        Some("player"),
    );
}

#[tokio::test]
async fn lazer_pause_clears_only_regular_gameplay_state() {
    let mut bundle = StateBundle::new(ClientType::Lazer, 11, Arc::new(EchoCalculation));
    let active = RawRegularSnapshotBuilder::new(
        RegularCaptureContext::new(ClientType::Lazer, 11, 0, 1),
        Ok(ReadStatus::Ready(raw_global(GameState::Play, 500))),
    )
    .gameplay(Ok(ReadStatus::Ready(score(3, 50))))
    .leaderboard(Ok(ReadStatus::Ready((
        true,
        None,
        vec![raw_leaderboard_player()],
    ))))
    .publish();
    let active = bundle
        .apply_regular(active)
        .await
        .expect("active lazer gameplay");
    assert_eq!(active.publication.precise.mode, 3);
    bundle.gameplay.hit_errors = press_hit_errors(vec![1.0, -1.0]);
    bundle.gameplay.previous_hit_error_index = 2;
    bundle.precise_dirty = true;

    let paused = RawRegularSnapshotBuilder::new(
        RegularCaptureContext::new(ClientType::Lazer, 11, 0, 2),
        Ok(ReadStatus::Ready(raw_global(GameState::Play, 500))),
    )
    .gameplay(Ok(ReadStatus::Ready(score(3, 50))))
    .leaderboard(Ok(ReadStatus::Ready((
        true,
        None,
        vec![raw_leaderboard_player()],
    ))))
    .publish();
    let paused = bundle
        .apply_regular(paused)
        .await
        .expect("paused lazer gameplay");

    assert!(paused.publication.regular.global.paused);
    assert_eq!(paused.publication.regular.gameplay.score, 0.0);
    assert_eq!(paused.publication.regular.gameplay.mode, 0);
    assert_eq!(paused.publication.regular.gameplay.combo, 0);
    assert!(paused.publication.regular.gameplay.player_name.is_empty());
    assert!(
        paused
            .publication
            .regular
            .gameplay
            .leaderboard_scores
            .is_empty()
    );
    assert_eq!(
        paused.publication.precise.hit_errors.all.as_slice(),
        &[1.0, -1.0]
    );
    assert_eq!(paused.publication.precise.committed_hit_error_index, 2);
    assert_eq!(paused.publication.precise.mode, 3);

    let resumed = RawRegularSnapshotBuilder::new(
        RegularCaptureContext::new(ClientType::Lazer, 11, 0, 3),
        Ok(ReadStatus::Ready(raw_global(GameState::Play, 600))),
    )
    .gameplay(Ok(ReadStatus::Ready(score(3, 51))))
    .publish();
    let resumed = bundle
        .apply_regular(resumed)
        .await
        .expect("resumed lazer gameplay");
    assert!(!resumed.publication.regular.global.paused);
    assert_eq!(resumed.publication.regular.gameplay.score, 123.0);
    assert_eq!(resumed.publication.regular.gameplay.mode, 3);
    assert_eq!(resumed.publication.precise.mode, 3);
    assert_eq!(resumed.publication.regular.gameplay.combo, 51);
}

#[test]
fn expected_grade_uses_live_accuracy() {
    let mut gameplay = GameplayState::default();
    let mut raw = score(3, 1);
    raw.accuracy = 98.5;
    gameplay.apply_score(&raw, ClientType::Stable, 100);
    let mut expected_statistics = gameplay.statistics.clone();
    expected_statistics.great = 100;
    let live = calculate_grade(
        false,
        &gameplay.mods.array,
        gameplay.mode as i32,
        gameplay.accuracy,
        &expected_statistics,
    );
    let perfect = calculate_grade(
        false,
        &gameplay.mods.array,
        gameplay.mode as i32,
        100.0,
        &expected_statistics,
    );
    assert_eq!(gameplay.grade_expected, live);
    assert_ne!(gameplay.grade_expected, perfect);
}

#[test]
fn tournament_menu_debounce_retains_old_map_for_five_hundred_ms() {
    let mut state = MenuState::default();
    let start = Instant::now();
    let first = RawMenu::Full(menu("first"));
    assert!(!state.apply(&first, true, start));
    assert!(state.checksum.is_none());
    assert!(!state.apply(&first, true, start + Duration::from_millis(499)));
    assert!(state.apply(&first, true, start + Duration::from_millis(500)));
    assert_eq!(state.checksum.as_deref(), Some("first"));

    let second = RawMenu::Full(menu("second"));
    assert!(!state.apply(&second, true, start + Duration::from_millis(600)));
    assert_eq!(state.checksum.as_deref(), Some("first"));
}

#[tokio::test]
async fn regular_transition_is_source_ordered_and_rewind_resets_epoch() {
    let mut bundle = StateBundle::new(ClientType::Stable, 7, Arc::new(EchoCalculation));
    bundle.set_calculate_pp(false);
    bundle.global.status = GameState::Play.value() as i64;
    bundle.global.play_time = 500.0;
    let first = RawRegularSnapshotBuilder::new(
        RegularCaptureContext::new(ClientType::Stable, 7, 0, 1),
        Ok(ReadStatus::Ready(raw_global(GameState::Play, 500))),
    )
    .menu(Ok(ReadStatus::Ready(RawMenu::Full(menu("map")))))
    .mp3_length(Ok(ReadStatus::Ready(60_000.0)))
    .gameplay(Ok(ReadStatus::Ready(score(3, 50))))
    .leaderboard(Ok(ReadStatus::Ready((
        true,
        None,
        vec![raw_leaderboard_player()],
    ))))
    .publish();
    let committed = bundle
        .apply_regular(first)
        .await
        .expect("regular transition");
    assert_eq!(committed.context.status, GameState::Play.value() as i64);
    assert_eq!(committed.context.mode, 3);
    assert_eq!(
        committed.publication.regular.menu.checksum.as_deref(),
        Some("map")
    );
    assert_eq!(committed.publication.regular.gameplay.player_name, "player");
    assert_eq!(
        committed
            .publication
            .regular
            .gameplay
            .leaderboard_scores
            .len(),
        1
    );
    let epoch = committed.context.reset_epoch;

    bundle.global.play_time = 100.0;
    let rewind = RawRegularSnapshotBuilder::new(
        RegularCaptureContext::new(ClientType::Stable, 7, epoch, 2),
        Ok(ReadStatus::Ready(raw_global(GameState::Play, 100))),
    )
    .menu(Ok(ReadStatus::Ready(RawMenu::Checksum(
        crate::model::memory::MenuChecksum {
            gamemode: 0,
            ranked_status: 4.0,
        },
    ))))
    .gameplay(Ok(ReadStatus::Ready(score(3, 10))))
    .publish();
    let rewound = bundle
        .apply_regular(rewind)
        .await
        .expect("rewind transition");
    assert!(rewound.context.reset_epoch > epoch);
    assert_eq!(rewound.context.committed_hit_error_index, 0);
    assert_eq!(rewound.context.accepted_precise_sequence, 0);
}

#[tokio::test]
async fn mismatched_calculation_key_cannot_mutate_published_state() {
    let mut bundle = StateBundle::new(ClientType::Stable, 11, Arc::new(MismatchedCalculation));
    let regular = RawRegularSnapshotBuilder::new(
        RegularCaptureContext::new(ClientType::Stable, 11, 0, 1),
        Ok(ReadStatus::Ready(raw_global(GameState::Menu, 0))),
    )
    .menu(Ok(ReadStatus::Ready(RawMenu::Full(menu("keyed-map")))))
    .audio_velocity_base(Ok(ReadStatus::Ready(vec![0.0; 40])))
    .publish();
    let committed = bundle
        .apply_regular(regular)
        .await
        .expect("calculation transition");
    assert_eq!(committed.publication.regular.beatmap.full_pp, 0.0);
    assert!(committed.publication.regular.beatmap.source_key.is_none());
    assert_eq!(committed.publication.regular.result_screen.pp, 0.0);
}

#[tokio::test]
async fn calculation_uses_post_read_paths_and_metadata_even_when_pp_disabled() {
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let backend = Arc::new(RecordingCalculation {
        requests: requests.clone(),
    });
    let mut bundle = StateBundle::new_with_root(ClientType::Stable, 12, "/game", backend);
    let (config_tx, config_rx) = watch::channel(Arc::new(crate::config::Config::default()));
    bundle.set_config_updates(config_rx);
    let config = crate::config::Config {
        calculate_pp: false,
        read_mania_scroll_speed: false,
        show_mp_commands: true,
        ..Default::default()
    };
    config_tx.send_replace(Arc::new(config));
    let regular = RawRegularSnapshotBuilder::new(
        RegularCaptureContext::new(ClientType::Stable, 12, 0, 1),
        Ok(ReadStatus::Ready(raw_global(GameState::Menu, 0))),
    )
    .menu(Ok(ReadStatus::Ready(RawMenu::Full(menu("fresh-map")))))
    .publish();

    let committed = bundle
        .apply_regular(regular)
        .await
        .expect("metadata calculation");
    assert!(!committed.next_capture.read_mania_scroll_speed);
    assert!(committed.next_capture.show_mp_commands);
    let requests = requests
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert!(!request.calculate_pp);
    assert!(request.entry.regular.menu.checksum.is_none());
    assert_eq!(
        request.current.regular.menu.checksum.as_deref(),
        Some("fresh-map")
    );
    assert_eq!(request.input.game_folder, PathBuf::from("/game"));
    assert_eq!(request.input.songs_folder, PathBuf::from("/game/songs"));
    assert_eq!(
        request.input.beatmap_path,
        PathBuf::from("/game/songs/set/map.osu"),
    );
    assert_eq!(request.input.checksum, "fresh-map");
    assert_eq!(request.input.mods, request.mods);
    assert_eq!(request.input.client_scoring, ClientType::Stable);
    assert_eq!(committed.publication.regular.beatmap.mods, request.mods);
    assert_eq!(
        committed.publication.regular.beatmap.source_key.as_deref(),
        Some("parsed"),
    );
}

#[tokio::test]
async fn first_play_calculation_uses_fresh_score_mode_and_mods() {
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut bundle = StateBundle::new(
        ClientType::Lazer,
        21,
        Arc::new(RecordingCalculation {
            requests: requests.clone(),
        }),
    );
    bundle.global.status = GameState::Play.value() as i64;
    bundle.global.play_time = 500.0;
    let mut dt = default_calculated_mods();
    dt.number = 64;
    dt.name = "DT".into();
    dt.checksum = "dt".into();
    let mut fresh_score = score(3, 10);
    fresh_score.mods = dt.clone();
    let regular = RawRegularSnapshotBuilder::new(
        RegularCaptureContext::new(ClientType::Lazer, 21, 0, 1),
        Ok(ReadStatus::Ready(raw_global(GameState::Play, 500))),
    )
    .menu(Ok(ReadStatus::Ready(RawMenu::Full(menu("fresh-play")))))
    .gameplay(Ok(ReadStatus::Ready(fresh_score)))
    .publish();

    let publication = bundle
        .apply_regular(regular)
        .await
        .expect("first play calculation")
        .publication;
    let requests = requests
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].key.mode, 3);
    assert_eq!(requests[0].input.mode, 3);
    assert_eq!(requests[0].input.mods, dt);
    assert_eq!(requests[0].mods, dt);
    assert_eq!(publication.regular.gameplay.mode, 3);
    assert_eq!(publication.regular.gameplay.mods, dt);
    assert_eq!(publication.regular.beatmap.mode, Some(3));
    assert_eq!(publication.regular.beatmap.mods, dt);
}

#[tokio::test]
async fn editor_graph_change_retries_same_time_after_transient_failure() {
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let failures = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut bundle = StateBundle::new(
        ClientType::Stable,
        22,
        Arc::new(ControlledCalculation {
            requests: requests.clone(),
            failures: failures.clone(),
        }),
    );
    bundle.global.status = GameState::Edit.value() as i64;
    bundle.global.play_time = 400.0;
    let snapshot = |sequence, raw_menu| {
        RawRegularSnapshotBuilder::new(
            RegularCaptureContext::new(ClientType::Stable, 22, 0, sequence),
            Ok(ReadStatus::Ready(raw_global(GameState::Edit, 400))),
        )
        .menu(Ok(ReadStatus::Ready(raw_menu)))
        .publish()
    };
    bundle
        .apply_regular(snapshot(1, RawMenu::Full(menu("old"))))
        .await
        .expect("initial editor calculation");
    failures.store(1, std::sync::atomic::Ordering::Release);
    let transient = bundle
        .apply_regular(snapshot(2, RawMenu::Full(menu("new"))))
        .await
        .expect("transient editor calculation is retained");
    assert_eq!(transient.next_capture.first_object_time, None);
    assert_eq!(bundle.previous_state_key, "old:0:");
    bundle
        .apply_regular(snapshot(
            3,
            RawMenu::Checksum(crate::model::memory::MenuChecksum {
                gamemode: 0,
                ranked_status: 4.0,
            }),
        ))
        .await
        .expect("same-time editor retry succeeds");
    let requests = requests
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[1].phase, CalculationPhase::Editor);
    assert_eq!(requests[1].play_time, requests[2].play_time);
    assert_eq!(requests[1].key.checksum, "new");
    assert_eq!(requests[2].key.checksum, "new");
    assert_eq!(bundle.previous_state_key, "new:0:");
    assert_eq!(bundle.beatmap.checksum.as_deref(), Some("new"));
    assert_eq!(bundle.calculation_failures, 0);
}

#[tokio::test]
async fn retry_not_ready_preserves_a_default_gameplay_marker() {
    let mut bundle = StateBundle::new(ClientType::Stable, 23, Arc::new(EchoCalculation));
    bundle.global.status = GameState::Play.value() as i64;
    bundle.global.play_time = 100.0;
    bundle.previous_time = 200.0;
    assert!(bundle.gameplay.is_default_state);
    let snapshot = RawRegularSnapshotBuilder::new(
        RegularCaptureContext::new(ClientType::Stable, 23, 0, 1),
        Ok(ReadStatus::Ready(raw_global(GameState::Play, 100))),
    )
    .gameplay(Ok(ReadStatus::NotReady))
    .publish();

    bundle
        .apply_regular(snapshot)
        .await
        .expect("retry with player loader not ready");
    assert!(bundle.gameplay.is_default_state);
    assert_eq!(bundle.calculation_reset_generation, 1);
}

#[tokio::test]
async fn backward_seek_after_first_object_retains_accumulated_hit_errors() {
    let mut bundle = StateBundle::new(ClientType::Lazer, 24, Arc::new(EchoCalculation));
    bundle.global.status = GameState::Play.value() as i64;
    bundle.global.play_time = 204_737.0;
    bundle.previous_time = 204_737.0;
    bundle.beatmap.first_object_time = Some(820);
    bundle.gameplay.is_default_state = false;
    bundle.gameplay.hit_errors = press_hit_errors(vec![0.0, 1.0, -1.0]);
    bundle.gameplay.previous_hit_error_index = 3;

    let snapshot = RawRegularSnapshotBuilder::new(
        RegularCaptureContext::new(ClientType::Lazer, 24, 0, 1),
        Ok(ReadStatus::Ready(raw_global(GameState::Play, 29_526))),
    )
    .gameplay(Ok(ReadStatus::NotReady))
    .publish();

    bundle
        .apply_regular(snapshot)
        .await
        .expect("backward seek remains in the active play epoch");

    assert_eq!(bundle.gameplay.hit_errors.all, [0.0, 1.0, -1.0]);
    assert_eq!(bundle.gameplay.previous_hit_error_index, 3);
    assert_eq!(bundle.calculation_reset_generation, 0);
}

#[tokio::test]
async fn stable_global_not_ready_and_tourney_debounce_do_not_abort_later_updates() {
    let mut bundle =
        StateBundle::new_with_root(ClientType::Stable, 13, "/game", Arc::new(EchoCalculation));
    bundle.global.memory_songs_folder = "/missing/Songs".into();
    bundle.set_roles(true, false);
    let mut settings = SettingsObject::new();
    settings.insert("groupType".into(), json!(7));
    let regular = RawRegularSnapshotBuilder::new(
        RegularCaptureContext::new(ClientType::Stable, 13, 0, 1),
        Ok(ReadStatus::NotReady),
    )
    .menu(Ok(ReadStatus::Ready(RawMenu::Full(menu("debounced")))))
    .settings(Ok(ReadStatus::Ready(settings)))
    .publish();

    let committed = bundle
        .apply_regular(regular)
        .await
        .expect("stable stale global transition");
    assert!(committed.publication.regular.menu.checksum.is_none());
    assert_eq!(committed.publication.regular.global.game_folder, "/game");
    assert_eq!(
        committed.publication.regular.global.songs_folder,
        format!(
            "{separator}game{separator}missing{separator}Songs",
            separator = std::path::MAIN_SEPARATOR
        ),
    );
    assert_eq!(
        committed.publication.regular.settings.values["groupType"],
        7
    );

    let mut lazer = StateBundle::new(ClientType::Lazer, 14, Arc::new(EchoCalculation));
    let mut settings = SettingsObject::new();
    settings.insert("groupType".into(), json!(9));
    let gated = RawRegularSnapshotBuilder::new(
        RegularCaptureContext::new(ClientType::Lazer, 14, 0, 1),
        Ok(ReadStatus::NotReady),
    )
    .settings(Ok(ReadStatus::Ready(settings)))
    .publish();
    let gated = lazer.apply_regular(gated).await.expect("lazer global gate");
    assert_eq!(gated.publication.regular.settings.values["groupType"], 0);
}

#[tokio::test]
async fn precise_publication_reuses_regular_arc_and_regular_reuses_clean_precise_arc() {
    let mut bundle = StateBundle::new(ClientType::Stable, 15, Arc::new(EchoCalculation));
    bundle.global.status = GameState::Play.value() as i64;
    bundle.global.play_time = 500.0;
    let leaderboard = vec![raw_leaderboard_player(); 1_000];
    let regular = RawRegularSnapshotBuilder::new(
        RegularCaptureContext::new(ClientType::Stable, 15, 0, 1),
        Ok(ReadStatus::Ready(raw_global(GameState::Play, 500))),
    )
    .menu(Ok(ReadStatus::Ready(RawMenu::Full(menu("arc-map")))))
    .gameplay(Ok(ReadStatus::Ready(score(0, 50))))
    .leaderboard(Ok(ReadStatus::Ready((true, None, leaderboard))))
    .publish();
    let first = bundle
        .apply_regular(regular)
        .await
        .expect("initial regular publication")
        .publication;
    let regular_ptr = first.regular.gameplay.leaderboard_scores.as_ptr();

    let precise = RawPreciseSnapshotBuilder::new(
        PreciseCaptureContext::new(
            ClientType::Stable,
            15,
            first.reset_epoch,
            0,
            default_calculated_mods(),
            0,
            0,
        ),
        1,
        Ok(ReadStatus::Ready(GlobalPrecise {
            status: GameState::Play.value() as i64,
            time: 510.0,
        })),
    )
    .hit_errors(Ok(ReadStatus::Ready(hit_error_suffix(1, vec![2.0]))))
    .publish();
    let precise_publication = bundle
        .apply_precise(precise)
        .await
        .expect("precise publication")
        .publication;
    assert!(Arc::ptr_eq(&first.regular, &precise_publication.regular));
    assert_eq!(
        precise_publication
            .regular
            .gameplay
            .leaderboard_scores
            .as_ptr(),
        regular_ptr,
    );
    assert!(!Arc::ptr_eq(&first.precise, &precise_publication.precise));
    assert_eq!(precise_publication.precise.committed_hit_error_index, 1);

    let next = RawRegularSnapshotBuilder::new(
        RegularCaptureContext::new(ClientType::Stable, 15, 0, 2),
        Ok(ReadStatus::Ready(raw_global(GameState::Play, 510))),
    )
    .menu(Ok(ReadStatus::Ready(RawMenu::Checksum(
        crate::model::memory::MenuChecksum {
            gamemode: 0,
            ranked_status: 4.0,
        },
    ))))
    .gameplay(Ok(ReadStatus::Ready(score(0, 50))))
    .publish();
    let next = bundle
        .apply_regular(next)
        .await
        .expect("next regular publication")
        .publication;
    assert!(!Arc::ptr_eq(&precise_publication.regular, &next.regular));
    assert!(Arc::ptr_eq(&precise_publication.precise, &next.precise));
}

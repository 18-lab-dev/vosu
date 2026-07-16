use super::*;

#[derive(Clone)]
struct LoggingMemoryReader {
    calls: Arc<Mutex<Vec<&'static str>>>,
    full_menu_checksum: Option<&'static str>,
    gameplay_first_object_time: Arc<Mutex<Option<Option<i64>>>>,
    synchronization: ReaderSynchronization,
    status: i64,
    global_not_ready: bool,
    menu_not_ready: bool,
    result_not_ready: bool,
    dependents_ready: bool,
    precise_global: Option<GlobalPrecise>,
}

impl LoggingMemoryReader {
    fn pending<T>(&self, name: &'static str) -> MemoryRead<T> {
        lock(&self.calls).push(name);
        Ok(ReadStatus::NotReady)
    }
}

impl SynchronizedMemoryReader for LoggingMemoryReader {
    fn reader_synchronization(&self) -> ReaderSynchronization {
        self.synchronization
    }
    fn synchronize_precise(&mut self, state: ReaderSynchronization, mode: i64) {
        self.synchronization = state;
        lock(&self.calls).push(if mode == 3 {
            "sync-mode-3"
        } else {
            "sync-other"
        });
    }
}

impl MemoryReader for LoggingMemoryReader {
    fn audio_velocity_base(&mut self) -> MemoryRead<AudioVelocityBase> {
        self.pending("audio")
    }
    fn user(&mut self) -> MemoryRead<User> {
        self.pending("user")
    }
    fn result_screen(&mut self) -> MemoryRead<ResultScreen> {
        lock(&self.calls).push("result");
        if self.result_not_ready {
            Ok(ReadStatus::NotReady)
        } else {
            Ok(ReadStatus::Unchanged)
        }
    }
    fn gameplay(&mut self, _: f64, first_object_time: Option<i64>) -> MemoryRead<Gameplay> {
        *lock(&self.gameplay_first_object_time) = Some(first_object_time);
        lock(&self.calls).push("gameplay");
        if self.dependents_ready {
            Ok(ReadStatus::Ready(raw_gameplay(3)))
        } else {
            Ok(ReadStatus::NotReady)
        }
    }
    fn key_overlay(&mut self, _: i64) -> MemoryRead<KeyOverlay> {
        self.pending("keys")
    }
    fn hit_errors(&mut self, _: i64) -> MemoryRead<HitErrors> {
        self.pending("hits")
    }
    fn global(&mut self) -> MemoryRead<Global> {
        lock(&self.calls).push("global");
        if self.global_not_ready {
            return Ok(ReadStatus::NotReady);
        }
        let mut value = raw_global();
        value.status = self.status;
        Ok(ReadStatus::Ready(value))
    }
    fn global_precise(&mut self) -> MemoryRead<GlobalPrecise> {
        lock(&self.calls).push("global-precise");
        match self.precise_global {
            Some(value) => Ok(ReadStatus::Ready(value)),
            None => Ok(ReadStatus::NotReady),
        }
    }
    fn menu(&mut self, _: &str) -> MemoryRead<Menu> {
        lock(&self.calls).push("menu");
        if self.menu_not_ready {
            return Ok(ReadStatus::NotReady);
        }
        if let Some(checksum) = self.full_menu_checksum {
            return Ok(ReadStatus::Ready(Menu::Full(
                crate::model::memory::MenuFull {
                    gamemode: 0,
                    checksum: checksum.into(),
                    filename: "map.osu".into(),
                    plays: 0,
                    artist: String::new(),
                    artist_original: String::new(),
                    title: String::new(),
                    title_original: String::new(),
                    ar: 0.0,
                    cs: 0.0,
                    hp: 0.0,
                    od: 0.0,
                    audio_filename: String::new(),
                    audio_file_mimetype: String::new(),
                    background_filename: String::new(),
                    background_file_mimetype: String::new(),
                    folder: String::new(),
                    creator: String::new(),
                    difficulty: String::new(),
                    map_id: 0,
                    set_id: 0,
                    ranked_status: 0.0,
                    object_count: 0,
                },
            )));
        }
        Ok(ReadStatus::Ready(Menu::Checksum(
            crate::model::memory::MenuChecksum {
                gamemode: 2,
                ranked_status: 0.0,
            },
        )))
    }
    fn mp3_length(&mut self) -> MemoryRead<Mp3Length> {
        self.pending("mp3")
    }
    fn tourney(&mut self) -> MemoryRead<Tourney> {
        lock(&self.calls).push("tourney");
        if self.dependents_ready {
            Ok(ReadStatus::Ready(raw_tourney()))
        } else {
            Ok(ReadStatus::NotReady)
        }
    }
    fn tourney_chat(&mut self, _: &[TourneyManagerChatItem]) -> MemoryRead<TourneyChat> {
        self.pending("chat")
    }
    fn tourney_user(&mut self) -> MemoryRead<TourneyUser> {
        self.pending("tourney-user")
    }
    fn leaderboard(&mut self, mode: i64) -> MemoryRead<Leaderboard> {
        lock(&self.calls).push(if mode == 3 {
            "leaderboard-mode-3"
        } else {
            "leaderboard-other"
        });
        Ok(ReadStatus::NotReady)
    }
    fn ranked_play(&mut self) -> MemoryRead<RankedPlay> {
        self.pending("ranked")
    }
    fn spectating_data(&mut self, _: bool) -> MemoryRead<LazerSpectator> {
        self.pending("spectating")
    }
    fn settings(&mut self) -> MemoryRead<Settings> {
        self.pending("settings")
    }
}

#[test]
fn source_capture_gates_and_dispatches_reducer_planned_branches_in_order() {
    let sync = Arc::new(Mutex::new(ReaderSynchronization::default()));
    let capture = |source, status, global_not_ready, menu_not_ready, plan: RegularCapturePlan| {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let request = RegularCaptureRequest {
            context: RegularCaptureContext::new(source, 1, 0, 1),
            plan,
        };
        SourceRegularReader::new(
            source,
            LoggingMemoryReader {
                calls: calls.clone(),
                gameplay_first_object_time: Arc::new(Mutex::new(None)),
                synchronization: ReaderSynchronization::default(),
                status,
                full_menu_checksum: None,
                global_not_ready,
                menu_not_ready,
                result_not_ready: false,
                dependents_ready: true,
                precise_global: None,
            },
            sync.clone(),
        )
        .capture_regular(&request)
        .expect("capture");
        lock(&calls).clone()
    };

    assert_eq!(
        capture(
            ClientType::Stable,
            GameState::Play.value() as i64,
            true,
            false,
            RegularCapturePlan::default()
        ),
        ["global", "menu", "settings", "audio", "user"],
        "stable retains stale global state and continues",
    );
    assert_eq!(
        capture(
            ClientType::Lazer,
            GameState::Play.value() as i64,
            true,
            false,
            RegularCapturePlan::default()
        ),
        ["global"],
        "lazer requires a ready global pointer graph",
    );
    assert_eq!(
        capture(
            ClientType::Stable,
            GameState::Play.value() as i64,
            false,
            true,
            RegularCapturePlan::default()
        ),
        ["global", "menu"],
    );

    let play = RegularCapturePlan {
        status: GameState::Play.value() as i64,
        read_mp3_length: true,
        read_tourney_user: true,
        ..RegularCapturePlan::default()
    };
    assert_eq!(
        capture(
            ClientType::Stable,
            GameState::Play.value() as i64,
            false,
            false,
            play
        ),
        [
            "global",
            "menu",
            "mp3",
            "settings",
            "gameplay",
            "leaderboard-mode-3",
            "tourney-user",
            "user"
        ],
        "reducer-owned play status drives the capture branch",
    );

    let tourney = RegularCapturePlan {
        status: GameState::Tourney.value() as i64,
        read_mp3_length: true,
        read_tourney_user: true,
        ..RegularCapturePlan::default()
    };
    assert_eq!(
        capture(
            ClientType::Stable,
            GameState::Tourney.value() as i64,
            false,
            false,
            tourney
        ),
        [
            "global",
            "menu",
            "mp3",
            "settings",
            "tourney",
            "chat",
            "tourney-user",
            "user"
        ],
    );

    assert_eq!(
        capture(
            ClientType::Lazer,
            GameState::ResultScreen.value() as i64,
            false,
            false,
            RegularCapturePlan {
                status: GameState::ResultScreen.value() as i64,
                ..RegularCapturePlan::default()
            }
        ),
        [
            "global", "menu", "mp3", "result", "ranked", "settings", "user"
        ],
        "fresh result status drives result read before ranked/settings",
    );
    assert_eq!(
        capture(
            ClientType::Lazer,
            GameState::Lobby.value() as i64,
            false,
            false,
            RegularCapturePlan {
                status: GameState::Lobby.value() as i64,
                ..RegularCapturePlan::default()
            }
        ),
        [
            "global",
            "menu",
            "mp3",
            "ranked",
            "settings",
            "spectating",
            "user"
        ],
    );

    let calls = Arc::new(Mutex::new(Vec::new()));
    let first_object_time = Arc::new(Mutex::new(None));
    SourceRegularReader::new(
        ClientType::Stable,
        LoggingMemoryReader {
            calls,
            gameplay_first_object_time: first_object_time.clone(),
            synchronization: ReaderSynchronization::default(),
            status: GameState::Play.value() as i64,
            global_not_ready: false,
            menu_not_ready: false,
            result_not_ready: false,
            dependents_ready: true,
            precise_global: None,
            full_menu_checksum: None,
        },
        sync.clone(),
    )
    .capture_regular(&RegularCaptureRequest {
        context: RegularCaptureContext::new(ClientType::Stable, 1, 0, 1),
        plan: RegularCapturePlan {
            status: GameState::Play.value() as i64,
            first_object_time: Some(12_345),
            ..RegularCapturePlan::default()
        },
    })
    .expect("stable play capture");
    assert_eq!(*lock(&first_object_time), Some(Some(12_345)));
    let changed_first_object_time = Arc::new(Mutex::new(None));
    SourceRegularReader::new(
        ClientType::Stable,
        LoggingMemoryReader {
            calls: Arc::new(Mutex::new(Vec::new())),
            full_menu_checksum: Some("new-map"),
            gameplay_first_object_time: changed_first_object_time.clone(),
            synchronization: ReaderSynchronization::default(),
            status: GameState::Play.value() as i64,
            global_not_ready: false,
            menu_not_ready: false,
            result_not_ready: false,
            dependents_ready: true,
            precise_global: None,
        },
        sync.clone(),
    )
    .capture_regular(&RegularCaptureRequest {
        context: RegularCaptureContext::new(ClientType::Stable, 1, 0, 2),
        plan: RegularCapturePlan {
            previous_checksum: "old-map".into(),
            status: GameState::Play.value() as i64,
            first_object_time: Some(12_345),
            ..RegularCapturePlan::default()
        },
    })
    .expect("changed-map stable play capture");
    assert_eq!(
        *lock(&changed_first_object_time),
        Some(None),
        "a new full menu invalidates the old map timing before gameplay"
    );

    let validated = ReaderSynchronization {
        game_base: Some(10),
        current_screen: Some(20),
    };
    *lock(&sync) = validated;
    let (key_tx, key_rx) = watch::channel(true);
    let _keep_sender = key_tx;
    let precise_calls = Arc::new(Mutex::new(Vec::new()));
    let mut precise = SourcePreciseReader::new(
        LoggingMemoryReader {
            calls: precise_calls.clone(),
            gameplay_first_object_time: Arc::new(Mutex::new(None)),
            synchronization: ReaderSynchronization::default(),
            status: GameState::Play.value() as i64,
            global_not_ready: false,
            menu_not_ready: false,
            result_not_ready: false,
            dependents_ready: false,
            precise_global: Some(GlobalPrecise {
                status: GameState::Menu.value() as i64,
                time: 200.0,
            }),
            full_menu_checksum: None,
        },
        key_rx,
    );
    let mut precise_context = context(ClientType::Lazer);
    precise_context.mode = 3;
    precise
        .capture_precise(&precise_context, 1, validated)
        .expect("precise capture");
    assert_eq!(
        lock(&precise_calls).as_slice(),
        &["sync-mode-3", "global-precise"],
        "fresh precise menu status suppresses keys and hits despite prior play context"
    );

    lock(&precise_calls).clear();
    precise.reader.precise_global = Some(GlobalPrecise {
        status: GameState::Play.value() as i64,
        time: 150.0,
    });
    precise_context.status = GameState::Menu.value() as i64;
    precise_context.play_time = 0.0;
    precise
        .capture_precise(&precise_context, 2, validated)
        .expect("fresh play capture");
    assert_eq!(
        lock(&precise_calls).as_slice(),
        &["sync-mode-3", "global-precise", "keys", "hits"],
        "fresh precise play status drives keys and hits despite prior menu context",
    );
}

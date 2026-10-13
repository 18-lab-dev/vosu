use super::*;

impl StateBundle {
    pub fn new(
        source: ClientType,
        generation: u64,
        calculation: Arc<dyn StateCalculationBackend>,
    ) -> Self {
        Self::new_with_root(source, generation, PathBuf::new(), calculation)
    }

    pub fn new_with_root(
        source: ClientType,
        generation: u64,
        instance_root: impl Into<PathBuf>,
        calculation: Arc<dyn StateCalculationBackend>,
    ) -> Self {
        Self {
            source,
            generation,
            instance_root: instance_root.into(),
            reset_epoch: 0,
            calculation_reset_generation: 0,
            global: GlobalState::default(),
            settings: SettingsState::default(),
            beatmap: BeatmapPPState::default(),
            menu: MenuState::default(),
            bass_density: BassDensityState::default(),
            gameplay: GameplayState::default(),
            stable_mania_hit_errors: StableManiaHitErrorState::default(),
            result_screen: ResultScreenState::default(),
            tourney: TourneyManagerState::default(),
            user: UserState::default(),
            lazer_multi_spectating: LazerMultiSpectatingState::default(),
            accepted_precise_sequence: 0,
            latest_precise_global_sequence: 0,
            ranked_play: RankedPlayState::default(),
            room: RoomState::default(),
            previous_state_key: String::new(),
            previous_mp3_length: None,
            previous_time: 0.0,
            play_clock_armed: false,
            stall_reference_play_time: 0.0,
            is_tourney_manager: false,
            is_tourney_spectator: false,
            calculate_pp: true,
            read_mania_scroll_speed: true,
            show_mp_commands: false,
            calculation_failures: 0,
            calculation,
            published_regular: None,
            published_precise: None,
            precise_dirty: true,
            config_updates: None,
        }
    }

    pub fn set_roles(&mut self, tourney_manager: bool, tourney_spectator: bool) {
        self.is_tourney_manager = tourney_manager;
        self.is_tourney_spectator = tourney_spectator;
    }

    pub fn set_calculate_pp(&mut self, calculate: bool) {
        self.calculate_pp = calculate;
    }

    pub fn set_capture_config(&mut self, read_mania_scroll_speed: bool, show_mp_commands: bool) {
        self.read_mania_scroll_speed = read_mania_scroll_speed;
        self.show_mp_commands = show_mp_commands;
    }

    pub fn apply_config(&mut self, config: &crate::config::Config) {
        self.calculate_pp = config.calculate_pp;
        self.read_mania_scroll_speed = config.read_mania_scroll_speed;
        self.show_mp_commands = config.show_mp_commands;
    }

    pub fn set_config_updates(&mut self, updates: watch::Receiver<Arc<crate::config::Config>>) {
        self.config_updates = Some(updates);
        self.refresh_config();
    }

    pub(super) fn refresh_config(&mut self) {
        let config = self
            .config_updates
            .as_ref()
            .map(|updates| updates.borrow().clone());
        if let Some(config) = config {
            self.apply_config(&config);
        }
    }

    /// Stall heuristic for clients without an authoritative pause flag.
    ///
    /// Only reports paused after play_time has advanced forward while staying in
    /// Play and then held across a whole API cadence interval. Entering play,
    /// retries (time rewind), and pre-start clock holds stay unpaused so
    /// overlays keep rendering. Subscriber-driven samples between API cadence
    /// captures may resume but never assert a pause: samples milliseconds apart
    /// routinely observe an unchanged clock while the game is running.
    fn derive_paused_from_play_time_stall(
        &mut self,
        previous_status: i64,
        status: i64,
        previous_play_time: f64,
        play_time: f64,
        api_cadence: bool,
    ) -> bool {
        let reference = self.stall_reference_play_time;
        if api_cadence {
            self.stall_reference_play_time = play_time;
        }
        let play = GameState::Play.value() as i64;
        if status != play {
            self.play_clock_armed = false;
            return false;
        }
        if previous_status != play {
            self.play_clock_armed = false;
            return false;
        }
        if play_time != previous_play_time {
            if play_time > previous_play_time {
                self.play_clock_armed = true;
            } else {
                // Retry / seek backwards resets the armed window.
                self.play_clock_armed = false;
            }
            return false;
        }
        if !api_cadence {
            return self.global.paused;
        }
        self.play_clock_armed && play_time == reference
    }

    pub(super) fn increment_epoch(&mut self) {
        self.reset_epoch = self.reset_epoch.saturating_add(1);
        self.accepted_precise_sequence = 0;
    }

    pub(super) fn reset_precise_epoch(&mut self) {
        self.increment_epoch();
        self.gameplay.reset_keys();
        self.gameplay.reset_hit_errors();
        self.stable_mania_hit_errors.reset();
        self.gameplay.unstable_rate = 0.0;
        self.precise_dirty = true;
    }

    pub(super) fn mark_calculation_reset(&mut self) {
        self.calculation_reset_generation = self.calculation_reset_generation.saturating_add(1);
    }

    pub(super) fn current_mode_mods(&self, status: i64) -> (i64, CalculateMods) {
        if status == GameState::Play.value() as i64 {
            (self.gameplay.mode, self.gameplay.mods.clone())
        } else if status == GameState::ResultScreen.value() as i64 {
            (
                self.result_screen.raw.mode,
                self.result_screen.raw.mods.clone(),
            )
        } else {
            (
                self.menu.gamemode.unwrap_or(0),
                self.global.menu_mods.clone(),
            )
        }
    }

    pub(super) fn context(&self, mode: i64, mods: CalculateMods) -> ReadContext {
        ReadContext {
            source: self.source,
            instance_generation: self.generation,
            reset_epoch: self.reset_epoch,
            status: self.global.status,
            play_time: self.global.play_time,
            mode,
            mods,
            committed_hit_error_index: self.gameplay.previous_hit_error_index,
            accepted_precise_sequence: self.accepted_precise_sequence,
            latest_precise_global_sequence: self.latest_precise_global_sequence,
        }
    }

    pub(super) fn capture_plan(&self) -> RegularCapturePlan {
        RegularCapturePlan {
            previous_checksum: self.menu.previous_md5.clone(),
            read_mp3_length: self.source == ClientType::Lazer
                || self
                    .menu
                    .folder
                    .as_deref()
                    .is_some_and(|folder| !folder.is_empty()),
            status: self.global.status,
            gameplay_time: self.global.play_time,
            first_object_time: if self.beatmap.checksum == self.menu.checksum {
                self.beatmap.first_object_time
            } else {
                None
            },
            tourney_messages: self.tourney.messages.clone().into(),
            read_tourney_user: self.is_tourney_spectator,
            read_mania_scroll_speed: self.read_mania_scroll_speed,
            show_mp_commands: self.show_mp_commands,
        }
    }

    pub(super) fn calculation_key(
        &self,
        mode: i64,
        mods: &CalculateMods,
    ) -> Option<StateCalculationKey> {
        Some(StateCalculationKey {
            instance_generation: self.generation,
            reset_epoch: self.reset_epoch,
            checksum: self.menu.checksum.clone()?,
            mode,
            mods_checksum: mods.checksum.clone(),
        })
    }

    pub(super) fn apply_calculation(
        &mut self,
        result: StateCalculationResult,
        expected: &StateCalculationKey,
        mods: &CalculateMods,
    ) -> CalculationApplyOutcome {
        if &result.key != expected {
            return CalculationApplyOutcome::Mismatched;
        }
        let applied =
            result.beatmap.is_some() || result.gameplay.is_some() || result.result.is_some();
        let retry_state_key = result
            .beatmap
            .as_ref()
            .is_some_and(|update| update.retry_state_key);
        if let Some(update) = result.beatmap {
            if !update.event_only && !update.graph_event_only {
                self.beatmap.checksum = Some(expected.checksum.clone());
                self.beatmap.mode = update.original_mode.or(Some(expected.mode));
                self.beatmap.mods = mods.clone();
                self.beatmap.first_object_time = update.first_object_time;
                self.beatmap.full_pp = update.full_pp;
                if self.calculate_pp
                    && let Some(pp_acc) = update.pp_acc
                {
                    self.beatmap.pp_acc = pp_acc;
                }
                if self.source == ClientType::Stable
                    && let Some(background_filename) = update.background_filename.as_ref()
                {
                    self.menu.background_filename = Some(background_filename.clone());
                }
                self.beatmap.source_key = update.source_key;
                self.beatmap.parsed = update.parsed;
                self.beatmap.graph = update.graph;
                self.beatmap.graph_mp3_length = update.graph_mp3_length;
            } else if update.graph_event_only {
                self.beatmap.graph = update.graph;
                self.beatmap.graph_mp3_length = update.graph_mp3_length;
            }
            self.beatmap.event_status = update.event_status;
        }
        if let Some(gameplay) = result.gameplay {
            self.gameplay.calculated = gameplay;
            self.beatmap.attributes = Some(gameplay);
        }
        if let Some(result) = result.result {
            self.result_screen.pp = result.pp;
            self.result_screen.fc_pp = result.fc_pp;
        }
        if applied && retry_state_key {
            CalculationApplyOutcome::AppliedRetryable
        } else if applied {
            CalculationApplyOutcome::Applied
        } else {
            CalculationApplyOutcome::MatchedEmpty
        }
    }

    pub(super) async fn apply_regular_inner(
        &mut self,
        snapshot: &RawRegularSnapshot,
    ) -> Result<Option<Arc<StateBundleSnapshot>>, StateWorkerError> {
        let regular_global_is_stale =
            snapshot.context().base_precise_global_sequence < self.latest_precise_global_sequence;
        self.refresh_config();
        let entry = self.materialize_current();
        if self.global.game_folder.is_empty() && !self.instance_root.as_os_str().is_empty() {
            self.global.game_folder = self.instance_root.to_string_lossy().into_owned();
        }
        match snapshot.global() {
            Ok(ReadStatus::NotReady) if self.source == ClientType::Lazer => return Ok(None),
            Ok(ReadStatus::NotReady) => {}
            Ok(ReadStatus::Ready(raw_global)) => {
                let play_time_changed = self.global.play_time != raw_global.play_time;
                let previous_status = self.global.status;
                let previous_play_time = self.global.previous_play_time;
                self.global.paused = match raw_global.paused {
                    Some(paused) => paused,
                    None => self.derive_paused_from_play_time_stall(
                        previous_status,
                        raw_global.status,
                        previous_play_time,
                        raw_global.play_time,
                        snapshot.context().api_cadence,
                    ),
                };
                self.global.previous_play_time = raw_global.play_time;
                self.global.game_time = raw_global.game_time;
                self.global.is_watching_replay = raw_global.is_watching_replay;
                self.global.is_replay_ui_hidden = raw_global.is_replay_ui_hidden;
                self.global.is_multi_spectating = raw_global.is_multi_spectating;
                self.global.show_interface = raw_global.show_interface;
                self.global.chat_status = raw_global.chat_status;
                self.global.menu_mods = raw_global.menu_mods.clone();
                self.global
                    .memory_songs_folder
                    .clone_from(&raw_global.memory_songs_folder);
                self.global.skin_folder = safe_join([raw_global.skin_folder.as_str()]);
                if !regular_global_is_stale {
                    self.global.status = raw_global.status;
                    self.global.play_time = raw_global.play_time;
                }
                self.precise_dirty |= !regular_global_is_stale && play_time_changed;
            }
            _ => {}
        }
        if self.global.songs_folder.is_empty() && !self.global.memory_songs_folder.is_empty() {
            let memory = Path::new(&self.global.memory_songs_folder);
            let songs = if self.source == ClientType::Lazer || memory.exists() {
                memory.to_path_buf()
            } else {
                PathBuf::from(safe_join([
                    self.instance_root.to_string_lossy().as_ref(),
                    self.global.memory_songs_folder.as_str(),
                ]))
            };
            self.global.songs_folder = songs.to_string_lossy().into_owned();
        }

        if let Some(menu_read) = snapshot.menu() {
            match menu_read {
                Ok(ReadStatus::NotReady) => return Ok(None),
                Ok(ReadStatus::Ready(raw_menu)) => {
                    self.menu.apply(
                        raw_menu,
                        self.is_tourney_manager || self.is_tourney_spectator,
                        Instant::now(),
                    );
                }
                _ => {}
            }
        }
        if let Some(read) = snapshot.mp3_length().and_then(ready) {
            self.menu.mp3_length = Some(*read);
        }

        let status = self.global.status;
        if status == GameState::ResultScreen.value() as i64
            && let Some(result_read) = snapshot.result_screen()
        {
            match result_read {
                Ok(ReadStatus::NotReady) => return Ok(None),
                Ok(ReadStatus::Ready(result)) => self.result_screen.apply(result, self.source),
                _ => {}
            }
        }
        if self.source == ClientType::Lazer {
            self.ranked_play.info = snapshot.ranked_play().and_then(ready).cloned();
            self.room.info = snapshot.room().and_then(ready).cloned();
        }
        if let Some(settings) = snapshot.settings().and_then(ready) {
            self.settings.apply(settings);
        }

        match GameState::from_value(status as i32) {
            Some(GameState::Menu) => {
                if let Some(values) = snapshot.audio_velocity_base().and_then(ready) {
                    self.bass_density.apply(values);
                }
            }
            Some(GameState::Edit) => {}
            Some(GameState::SelectEdit | GameState::SelectPlay) => {
                let reset = !self.gameplay.is_default_state
                    || self.result_screen != ResultScreenState::default()
                    || self.beatmap.attributes.is_some();
                if reset {
                    self.mark_calculation_reset();
                    if !self.gameplay.is_default_state {
                        self.reset_precise_epoch();
                        self.gameplay.reset(false, self.source);
                    }
                    self.result_screen = ResultScreenState::default();
                    self.beatmap.reset_attributes();
                }
            }
            Some(GameState::Play) => {
                let at_play_start = self.beatmap.first_object_time.map_or(
                    self.global.play_time <= 1_000.0,
                    |first| {
                        if self.source == ClientType::Lazer {
                            self.global.play_time <= first as f64
                        } else {
                            self.global.play_time < first as f64
                        }
                    },
                );
                if self.previous_time > self.global.play_time && at_play_start {
                    self.mark_calculation_reset();
                    self.reset_precise_epoch();
                    self.gameplay.reset(true, self.source);
                    self.beatmap.reset_attributes();
                }
                if self.beatmap.checksum.is_some()
                    && self.beatmap.checksum == self.menu.checksum
                    && at_play_start
                {
                    self.gameplay.reset_quick();
                }
                self.previous_time = self.global.play_time;
                if let Some(score) = snapshot.gameplay().and_then(ready) {
                    if !self.gameplay.is_default_state && score.mode != self.gameplay.mode {
                        self.mark_calculation_reset();
                        self.reset_precise_epoch();
                        self.gameplay.reset(false, self.source);
                    }
                    let previous_mode = self.gameplay.mode;
                    let previous_combo = self.gameplay.combo;
                    let previous_unstable_rate = self.gameplay.unstable_rate;
                    self.gameplay.apply_score(
                        score,
                        self.source,
                        self.menu.object_count.unwrap_or(0),
                    );
                    self.precise_dirty |= previous_mode != self.gameplay.mode
                        || previous_unstable_rate.to_bits()
                            != self.gameplay.unstable_rate.to_bits();
                    if self.source == ClientType::Lazer && previous_combo > self.gameplay.combo {
                        self.gameplay.reset_quick();
                    }
                }
                if let Some((visible, player, scores)) = snapshot.leaderboard().and_then(ready) {
                    self.gameplay.leaderboard_visible = *visible;
                    self.gameplay.leaderboard_player = player
                        .as_ref()
                        .map(LeaderboardPlayerState::from)
                        .unwrap_or_default();
                    self.gameplay.leaderboard_scores = scores.clone();
                }
                if self.source == ClientType::Lazer && self.global.paused {
                    // Precise samples may precede the first readable score; keep menu reset armed.
                    self.gameplay.is_default_state = false;
                }
            }
            Some(GameState::Tourney) if self.source == ClientType::Stable => {
                self.is_tourney_manager = true;
                if let Some(tourney) = snapshot.tourney().and_then(ready) {
                    self.tourney.scoreboard = tourney.clone();
                }
                if let Some(messages) = snapshot.tourney_chat().and_then(ready) {
                    self.tourney.messages = messages.clone();
                }
            }
            Some(GameState::Lobby) if self.source == ClientType::Lazer => {
                if let Some(data) = snapshot.lazer_spectating().and_then(ready) {
                    self.lazer_multi_spectating.data = Some(data.clone());
                }
            }
            Some(GameState::Lobby | GameState::MatchSetup | GameState::OnlineSelection) => {}
            Some(GameState::ResultScreen) => {}
            _ => {
                if !self.gameplay.is_default_state
                    || self.result_screen != ResultScreenState::default()
                {
                    self.mark_calculation_reset();
                    self.reset_precise_epoch();
                    self.gameplay.reset(false, self.source);
                    self.result_screen = ResultScreenState::default();
                }
            }
        }

        if self.is_tourney_spectator {
            match snapshot.tourney_user() {
                Some(Ok(ReadStatus::Ready(user))) => {
                    self.tourney.user = Some(TourneyUserState {
                        accuracy: user.accuracy,
                        ranked_score: user.ranked_score,
                        play_count: user.playcount,
                        global_rank: user.global_rank,
                        pp: user.pp,
                        name: user.name.clone(),
                        country: user.country.clone(),
                        id: user.id,
                    });
                    self.tourney.is_default_state = false;
                }
                Some(Ok(
                    ReadStatus::NotReady | ReadStatus::Unchanged | ReadStatus::Unsupported(_),
                )) => {
                    self.tourney.user = None;
                    self.tourney.is_default_state = true;
                    if !self.gameplay.is_default_state {
                        self.mark_calculation_reset();
                        self.reset_precise_epoch();
                        self.gameplay.reset(false, self.source);
                    }
                }
                _ => {}
            }
        }
        if let Some(Ok(ReadStatus::Ready(user))) = snapshot.user()
            && (user.raw_login_status != 0 || self.user.profile.is_none())
        {
            self.user.profile = Some(user.clone());
        }

        let (calculation_mode, calculation_mods) = self.current_mode_mods(status);
        let state_key = format!(
            "{}:{calculation_mode}:{}",
            self.menu.checksum.as_deref().unwrap_or_default(),
            calculation_mods.checksum
        );
        let graph_changed = self.previous_state_key != state_key
            || self.previous_mp3_length != self.menu.mp3_length;
        let editor_changed = status != GameState::Edit.value() as i64
            || self.previous_time != self.global.play_time
            || graph_changed;
        if editor_changed
            && let Some(key) = self.calculation_key(calculation_mode, &calculation_mods)
        {
            let phase = if status == GameState::Play.value() as i64 {
                CalculationPhase::Gameplay
            } else if status == GameState::ResultScreen.value() as i64 {
                CalculationPhase::Result
            } else if status == GameState::Edit.value() as i64 {
                CalculationPhase::Editor
            } else {
                CalculationPhase::Menu
            };
            let current = self.materialize_current_reusing(Some(&entry));
            let game_folder = PathBuf::from(&self.global.game_folder);
            let songs_folder = PathBuf::from(&self.global.songs_folder);
            let menu_folder = self.menu.folder.clone();
            let filename = self.menu.filename.clone();
            let beatmap_path = menu_folder
                .as_deref()
                .zip(filename.as_deref())
                .map_or_else(PathBuf::new, |(folder, filename)| {
                    songs_folder.join(folder).join(filename)
                });
            let input = CalculationInput {
                game_folder,
                songs_folder,
                menu_folder,
                filename,
                beatmap_path,
                checksum: key.checksum.clone(),
                mode: calculation_mode,
                mods: calculation_mods.clone(),
                client_scoring: self.source,
            };
            let request = StateCalculationRequest {
                key: key.clone(),
                input,
                phase,
                entry,
                current: Arc::clone(&current),
                mods: calculation_mods.clone(),
                calculate_pp: self.calculate_pp,
                play_time: self.global.play_time,
                calculation_reset_generation: self.calculation_reset_generation,
                gameplay: snapshot.gameplay().and_then(ready).cloned(),
                result: snapshot.result_screen().and_then(ready).cloned(),
            };
            let mut current_is_reusable = true;
            match self.calculation.calculate(request).await {
                Ok(result) => {
                    let outcome = self.apply_calculation(result, &key, &calculation_mods);
                    if outcome != CalculationApplyOutcome::Mismatched {
                        current_is_reusable = !matches!(
                            outcome,
                            CalculationApplyOutcome::Applied
                                | CalculationApplyOutcome::AppliedRetryable
                        );
                        if matches!(
                            outcome,
                            CalculationApplyOutcome::Applied
                                | CalculationApplyOutcome::AppliedRetryable
                        ) {
                            self.calculation_failures = 0;
                        }
                        if outcome != CalculationApplyOutcome::AppliedRetryable {
                            self.previous_state_key = state_key;
                            self.previous_mp3_length = self.menu.mp3_length;
                            if status == GameState::Edit.value() as i64 {
                                self.previous_time = self.global.play_time;
                            }
                        }
                    }
                }
                Err(StateWorkerError::Transition(message)) => {
                    self.calculation_failures = self.calculation_failures.saturating_add(1);

                    if self.calculation_failures <= 10 {
                        tracing::debug!(
                            source = ?self.source,
                            failures = self.calculation_failures,
                            error = %message,
                            "state calculation failed; retaining stale state"
                        );
                    } else {
                        tracing::error!(
                            source = ?self.source,
                            failures = self.calculation_failures,
                            error = %message,
                            "state calculation repeatedly failed; retaining stale state"
                        );
                    }
                }
                Err(error) => return Err(error),
            }
            if current_is_reusable {
                return Ok(Some(current));
            }
        }
        Ok(None)
    }

    fn stable_mania_metadata(&self, snapshot: &RawPreciseSnapshot) -> StableManiaMetadata {
        let Some(checksum) = self.menu.checksum.as_deref() else {
            return StableManiaMetadata::Pending;
        };
        if self.beatmap.checksum.as_deref() == Some(checksum)
            && let (Some(source_key), Some(parsed)) = (
                self.beatmap.source_key.as_deref(),
                self.beatmap.parsed.as_ref(),
            )
            && parsed.cache_key().source_checksum == source_key
            && parsed.cache_key().memory_identity.as_deref() == Some(checksum)
        {
            let mirror =
                snapshot.capture().mods().number & crate::calculation::osu_mods::MIRROR != 0;
            return parsed
                .mania_judgement_events(mirror)
                .map_or(StableManiaMetadata::Unavailable, StableManiaMetadata::Ready);
        }

        let desired_state_key = format!(
            "{checksum}:{}:{}",
            snapshot.capture().mode(),
            snapshot.capture().mods().checksum
        );
        if self.previous_state_key == desired_state_key && self.calculation_failures == 0 {
            StableManiaMetadata::Unavailable
        } else {
            StableManiaMetadata::Pending
        }
    }

    pub(super) fn apply_precise_inner(&mut self, snapshot: &RawPreciseSnapshot) -> ReadContext {
        if let Ok(ReadStatus::Ready(global)) = snapshot.global() {
            self.global.status = global.status;
            self.global.play_time = global.time;
            self.latest_precise_global_sequence = snapshot.produced_sequence();
        }
        if self.global.status == GameState::Play.value() as i64 {
            if self.global.play_time >= 150.0
                && let Some(Ok(ReadStatus::Ready(keys))) = snapshot.key_overlay()
            {
                let identities_match = self.gameplay.keypress_identity_mode
                    == Some(self.gameplay.keypress_mode)
                    && keys.len() == self.gameplay.keypresses.len()
                    && keys
                        .iter()
                        .zip(&self.gameplay.keypresses)
                        .all(|(current, previous)| current.name == previous.name);
                let values_match = identities_match
                    && keys
                        .iter()
                        .zip(&self.gameplay.keypresses)
                        .all(|(current, previous)| {
                            let valid = (0..=1_000_000).contains(&current.count);
                            previous.is_pressed == (valid && current.is_pressed)
                                && previous.count == if valid { current.count } else { 0 }
                        });
                if !values_match {
                    let sanitized: Vec<_> = keys
                        .iter()
                        .cloned()
                        .map(|mut key| {
                            if !(0..=1_000_000).contains(&key.count) {
                                key.is_pressed = false;
                                key.count = 0;
                            }
                            key
                        })
                        .collect();
                    self.gameplay.keypresses = sanitized
                        .iter()
                        .enumerate()
                        .map(|(index, key)| {
                            let previous =
                                identities_match.then(|| &self.gameplay.keypresses[index]);
                            let (pressed_at, released_at) = match previous {
                                Some(previous) if previous.is_pressed == key.is_pressed => {
                                    (previous.pressed_at, previous.released_at)
                                }
                                Some(previous) if key.is_pressed => {
                                    (Some(self.global.play_time), previous.released_at)
                                }
                                Some(previous) => {
                                    (previous.pressed_at, Some(self.global.play_time))
                                }
                                None if key.is_pressed => (Some(self.global.play_time), None),
                                None => (None, None),
                            };
                            KeypressButton {
                                name: Arc::clone(&key.name),
                                is_pressed: key.is_pressed,
                                count: key.count,
                                pressed_at,
                                released_at,
                            }
                        })
                        .collect();
                    self.gameplay.keypress_identity_mode = Some(self.gameplay.keypress_mode);
                    self.gameplay.key_overlay = if self.source == ClientType::Lazer {
                        sanitized
                    } else if sanitized.len() < 4 {
                        Vec::new()
                    } else {
                        let legacy_len = if self.gameplay.keypress_mode == 0 {
                            4
                        } else {
                            3
                        };
                        sanitized[..legacy_len].to_vec()
                    };
                }
                self.gameplay.is_key_overlay_default_state = false;
            }
            if let Some(Ok(ReadStatus::Ready(errors))) = snapshot.hit_errors()
                && matches!(
                    snapshot.global(),
                    Ok(ReadStatus::Ready(global))
                        if global.status == GameState::Play.value() as i64
                )
            {
                let mut release_corrections: Vec<_> = errors.release_corrections.iter().collect();
                release_corrections
                    .sort_by_key(|correction| std::cmp::Reverse(correction.press_offset));
                for correction in release_corrections {
                    let Some(position) =
                        correction.press_offset.checked_add(1).and_then(|distance| {
                            self.gameplay.hit_errors.press.len().checked_sub(distance)
                        })
                    else {
                        continue;
                    };
                    if self.gameplay.hit_errors.press[position].to_bits()
                        == correction.value.to_bits()
                    {
                        self.gameplay.hit_errors.press.remove(position);
                        self.gameplay.hit_errors.release.push(correction.value);
                    }
                }
                self.gameplay.total_hit_errors += errors.errors.all.iter().sum::<f64>();
                self.gameplay.total_hit_error_squares +=
                    errors.errors.all.iter().map(|hit| hit * hit).sum::<f64>();
                self.gameplay
                    .hit_errors
                    .all
                    .extend_from_slice(&errors.errors.all);
                let stable_partition =
                    if self.source == ClientType::Stable && snapshot.capture().mode() == 3 {
                        errors.stable_replay_frames.as_ref().map(|batch| {
                            let target_checksum = self.menu.checksum.as_deref();
                            let metadata = self
                                .stable_mania_hit_errors
                                .needs_metadata(target_checksum)
                                .then(|| self.stable_mania_metadata(snapshot));
                            self.stable_mania_hit_errors.partition(
                                &errors.errors.all,
                                batch,
                                target_checksum,
                                self.calculation_failures,
                                metadata,
                                Instant::now(),
                            )
                        })
                    } else {
                        None
                    };
                if let Some((press, release)) = stable_partition {
                    self.gameplay.hit_errors.press.extend(press);
                    self.gameplay.hit_errors.release.extend(release);
                } else {
                    self.gameplay
                        .hit_errors
                        .press
                        .extend_from_slice(&errors.errors.press);
                    self.gameplay
                        .hit_errors
                        .release
                        .extend_from_slice(&errors.errors.release);
                }
                self.gameplay.previous_hit_error_index = errors.index;
            }
        } else {
            self.gameplay.reset_keys();
        }
        let (mode, mods) = self.current_mode_mods(self.global.status);
        self.accepted_precise_sequence = snapshot.produced_sequence();
        let mut context = ReadContext {
            source: self.source,
            instance_generation: self.generation,
            reset_epoch: self.reset_epoch,
            status: self.global.status,
            play_time: self.global.play_time,
            mode,
            mods,
            committed_hit_error_index: self.gameplay.previous_hit_error_index,
            accepted_precise_sequence: self.accepted_precise_sequence,
            latest_precise_global_sequence: self.latest_precise_global_sequence,
        };
        if snapshot
            .hit_errors()
            .is_some_and(|read| !matches!(read, Ok(ReadStatus::Ready(_))))
        {
            context.committed_hit_error_index = snapshot.capture().start_index();
            context.accepted_precise_sequence = snapshot.capture().base_accepted_precise_sequence();
            self.accepted_precise_sequence = context.accepted_precise_sequence;
        }
        context
    }
}

impl SnapshotTransition for StateBundle {
    type Publication = Arc<StateBundleSnapshot>;

    fn apply_regular<'a>(
        &'a mut self,
        snapshot: Arc<RawRegularSnapshot>,
    ) -> TransitionFuture<'a, RegularTransition<Self::Publication>> {
        Box::pin(async move {
            let reusable_publication = self.apply_regular_inner(&snapshot).await?;
            let (mode, mods) = self.current_mode_mods(self.global.status);
            let context = self.context(mode, mods);
            let next_capture = self.capture_plan();
            let publication = if let Some(publication) = reusable_publication {
                self.published_regular = Some(Arc::clone(&publication.regular));
                self.published_precise = Some(Arc::clone(&publication.precise));
                self.precise_dirty = false;
                publication
            } else {
                self.publish_regular_state()
            };
            Ok(RegularTransition {
                publication,
                context,
                next_capture,
            })
        })
    }

    fn apply_precise<'a>(
        &'a mut self,
        snapshot: Arc<RawPreciseSnapshot>,
    ) -> TransitionFuture<'a, PreciseTransition<Self::Publication>> {
        Box::pin(async move {
            let context = self.apply_precise_inner(&snapshot);
            let publication = self.publish_precise_state();
            Ok(PreciseTransition {
                publication,
                context: Some(context),
            })
        })
    }
}

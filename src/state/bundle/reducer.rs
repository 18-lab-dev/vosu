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
            result_screen: ResultScreenState::default(),
            tourney: TourneyManagerState::default(),
            user: UserState::default(),
            lazer_multi_spectating: LazerMultiSpectatingState::default(),
            accepted_precise_sequence: 0,
            latest_precise_global_sequence: 0,
            ranked_play: RankedPlayState::default(),
            previous_state_key: String::new(),
            previous_mp3_length: None,
            previous_time: 0.0,
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

    pub fn snapshot(&self) -> Arc<StateBundleSnapshot> {
        Arc::new(StateBundleSnapshot {
            source: self.source,
            generation: self.generation,
            reset_epoch: self.reset_epoch,
            regular: self
                .published_regular
                .clone()
                .unwrap_or_else(|| Arc::new(self.build_regular_state())),
            precise: self
                .published_precise
                .clone()
                .unwrap_or_else(|| Arc::new(self.build_precise_state())),
        })
    }

    pub(super) fn materialize_current(&self) -> Arc<StateBundleSnapshot> {
        Arc::new(StateBundleSnapshot {
            source: self.source,
            generation: self.generation,
            reset_epoch: self.reset_epoch,
            regular: Arc::new(self.build_regular_state()),
            precise: Arc::new(self.build_precise_state()),
        })
    }

    pub(super) fn build_regular_state(&self) -> RegularState {
        RegularState {
            global: self.global.clone(),
            settings: self.settings.clone(),
            beatmap: self.beatmap.clone(),
            menu: self.menu.clone(),
            bass_density: self.bass_density.clone(),
            gameplay: self.gameplay.clone(),
            result_screen: self.result_screen.clone(),
            tourney: self.tourney.clone(),
            user: self.user.clone(),
            lazer_multi_spectating: self.lazer_multi_spectating.clone(),
            ranked_play: self.ranked_play.clone(),
        }
    }

    pub(super) fn build_precise_state(&self) -> PreciseState {
        PreciseState {
            status: self.global.status,
            play_time: self.global.play_time,
            mode: self.gameplay.keypress_mode,
            key_overlay: Arc::new(self.gameplay.key_overlay.clone()),
            keypresses: Arc::new(self.gameplay.keypresses.clone()),
            hit_errors: Arc::new(self.gameplay.hit_errors.clone()),
            total_hit_errors: self.gameplay.total_hit_errors,
            committed_hit_error_index: self.gameplay.previous_hit_error_index,
            unstable_rate: self.gameplay.unstable_rate,
        }
    }

    pub(super) fn publish_regular_state(&mut self) -> Arc<StateBundleSnapshot> {
        let next = self.build_regular_state();
        if self.published_regular.as_deref() != Some(&next) {
            self.published_regular = Some(Arc::new(next));
        }
        if self.precise_dirty || self.published_precise.is_none() {
            self.published_precise = Some(Arc::new(self.build_precise_state()));
        }
        self.precise_dirty = false;
        self.snapshot()
    }

    pub(super) fn publish_precise_state(&mut self) -> Arc<StateBundleSnapshot> {
        if self.published_regular.is_none() {
            self.published_regular = Some(Arc::new(self.build_regular_state()));
        }
        self.published_precise = Some(Arc::new(self.build_precise_state()));
        self.precise_dirty = false;
        self.snapshot()
    }

    pub(super) fn increment_epoch(&mut self) {
        self.reset_epoch = self.reset_epoch.saturating_add(1);
        self.accepted_precise_sequence = 0;
    }

    pub(super) fn reset_precise_epoch(&mut self) {
        self.increment_epoch();
        self.gameplay.reset_keys();
        self.gameplay.reset_hit_errors();
        self.gameplay.unstable_rate = 0.0;
        self.precise_dirty = true;
    }

    pub(super) fn reset_hit_epoch(&mut self) {
        self.increment_epoch();
        self.gameplay.reset_hit_errors();
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
    ) -> Result<(), StateWorkerError> {
        let regular_global_is_stale =
            snapshot.context().base_precise_global_sequence < self.latest_precise_global_sequence;
        self.refresh_config();
        let entry = self.materialize_current();
        if self.global.game_folder.is_empty() && !self.instance_root.as_os_str().is_empty() {
            self.global.game_folder = self.instance_root.to_string_lossy().into_owned();
        }
        match snapshot.global() {
            Ok(ReadStatus::NotReady) if self.source == ClientType::Lazer => return Ok(()),
            Ok(ReadStatus::NotReady) => {}
            Ok(ReadStatus::Ready(raw_global)) => {
                let play_time_changed = self.global.play_time != raw_global.play_time;
                self.global.paused = raw_global.play_time == self.global.previous_play_time;
                self.global.previous_play_time = raw_global.play_time;
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
                Ok(ReadStatus::NotReady) => return Ok(()),
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
                Ok(ReadStatus::NotReady) => return Ok(()),
                Ok(ReadStatus::Ready(result)) => self.result_screen.apply(result, self.source),
                _ => {}
            }
        }
        if self.source == ClientType::Lazer {
            self.ranked_play.info = snapshot.ranked_play().and_then(ready).cloned();
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
                    self.reset_hit_epoch();
                    self.gameplay.reset_quick();
                }
                self.previous_time = self.global.play_time;
                if self.source == ClientType::Lazer && self.global.paused {
                    self.gameplay.reset_paused_regular();
                } else {
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
                        if self.source == ClientType::Lazer && previous_combo > self.gameplay.combo
                        {
                            self.gameplay.reset_quick();
                        }
                    }
                    if let Some((visible, player, scores)) = snapshot.leaderboard().and_then(ready)
                    {
                        self.gameplay.leaderboard_visible = *visible;
                        self.gameplay.leaderboard_player = player
                            .as_ref()
                            .map(LeaderboardPlayerState::from)
                            .unwrap_or_default();
                        self.gameplay.leaderboard_scores = scores.clone();
                    }
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
                        global_rank: 0,
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
            let current = self.materialize_current();
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
                current,
                mods: calculation_mods.clone(),
                calculate_pp: self.calculate_pp,
                play_time: self.global.play_time,
                calculation_reset_generation: self.calculation_reset_generation,
                gameplay: snapshot.gameplay().and_then(ready).cloned(),
                result: snapshot.result_screen().and_then(ready).cloned(),
            };
            match self.calculation.calculate(request).await {
                Ok(result) => {
                    let outcome = self.apply_calculation(result, &key, &calculation_mods);
                    if outcome != CalculationApplyOutcome::Mismatched {
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
        }
        Ok(())
    }

    pub(super) fn apply_precise_inner(&mut self, snapshot: &RawPreciseSnapshot) -> ReadContext {
        if let Ok(ReadStatus::Ready(global)) = snapshot.global() {
            self.global.status = global.status;
            self.global.play_time = global.time;
            self.latest_precise_global_sequence = snapshot.produced_sequence();
        }
        if self.global.status == GameState::Play.value() as i64 && self.global.play_time >= 150.0 {
            if let Some(Ok(ReadStatus::Ready(keys))) = snapshot.key_overlay() {
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
                let identities_match = self.gameplay.keypress_identity_mode
                    == Some(self.gameplay.keypress_mode)
                    && sanitized.len() == self.gameplay.keypresses.len()
                    && sanitized
                        .iter()
                        .zip(&self.gameplay.keypresses)
                        .all(|(current, previous)| current.name == previous.name);
                self.gameplay.keypresses = sanitized
                    .iter()
                    .enumerate()
                    .map(|(index, key)| {
                        let previous = identities_match.then(|| &self.gameplay.keypresses[index]);
                        let (pressed_at, released_at) = match previous {
                            Some(previous) if previous.is_pressed == key.is_pressed => {
                                (previous.pressed_at, previous.released_at)
                            }
                            Some(previous) if key.is_pressed => {
                                (Some(self.global.play_time), previous.released_at)
                            }
                            Some(previous) => (previous.pressed_at, Some(self.global.play_time)),
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
                self.gameplay.is_key_overlay_default_state = false;
            }
            if let Some(Ok(ReadStatus::Ready(errors))) = snapshot.hit_errors() {
                self.gameplay.total_hit_errors += errors.array.iter().sum::<f64>();
                self.gameplay.hit_errors.extend_from_slice(&errors.array);
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
            self.apply_regular_inner(&snapshot).await?;
            let (mode, mods) = self.current_mode_mods(self.global.status);
            let context = self.context(mode, mods);
            let next_capture = self.capture_plan();
            let publication = self.publish_regular_state();
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

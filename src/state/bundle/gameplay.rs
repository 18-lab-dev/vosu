use super::*;

#[derive(Debug, Clone, PartialEq)]
pub struct GameplayState {
    pub is_default_state: bool,
    pub is_key_overlay_default_state: bool,
    pub failed: bool,
    pub retries: i64,
    pub player_name: String,
    pub mode: i64,
    pub keypress_mode: i64,
    pub mods: CalculateMods,
    pub score: f64,
    pub player_hp_smooth: f64,
    pub player_hp: f64,
    pub accuracy: f64,
    pub statistics: Statistics,
    pub maximum_statistics: Statistics,
    pub combo: i64,
    pub max_combo: i64,
    pub combo_previous: i64,
    pub hit_miss_previous: i64,
    pub slider_breaks: i64,
    pub unstable_rate: f64,
    pub grade_current: String,
    pub grade_expected: String,
    pub key_overlay: Vec<KeyOverlayButton>,
    pub keypresses: Vec<KeypressButton>,
    pub keypress_identity_mode: Option<i64>,
    pub hit_errors: HitErrorArray,
    pub total_hit_errors: f64,
    pub previous_hit_error_index: i64,
    pub previous_play_time: f64,
    pub leaderboard_visible: bool,
    pub leaderboard_player: LeaderboardPlayerState,
    pub leaderboard_scores: Vec<LeaderboardPlayer>,
    pub calculated: ProgressiveAttributes,
}

impl Default for GameplayState {
    fn default() -> Self {
        let statistics = default_statistics();
        Self {
            is_default_state: true,
            is_key_overlay_default_state: true,
            failed: false,
            retries: 0,
            player_name: String::new(),
            mode: 0,
            keypress_mode: 0,
            mods: default_calculated_mods(),
            score: 0.0,
            player_hp_smooth: 0.0,
            player_hp: 0.0,
            accuracy: 100.0,
            statistics: statistics.clone(),
            maximum_statistics: statistics,
            combo: 0,
            max_combo: 0,
            combo_previous: 0,
            hit_miss_previous: 0,
            slider_breaks: 0,
            unstable_rate: 0.0,
            grade_current: String::new(),
            grade_expected: String::new(),
            key_overlay: Vec::new(),
            keypresses: Vec::new(),
            keypress_identity_mode: None,
            hit_errors: HitErrorArray::default(),
            total_hit_errors: 0.0,
            previous_hit_error_index: 0,
            previous_play_time: 0.0,
            leaderboard_visible: false,
            leaderboard_player: LeaderboardPlayerState::default(),
            leaderboard_scores: Vec::new(),
            calculated: ProgressiveAttributes::default(),
        }
    }
}

impl GameplayState {
    pub(super) fn reset(&mut self, retry: bool, source: ClientType) {
        let grade_mode = self.mode;
        let grade_mods = self.mods.array.clone();
        let retained = retry.then(|| {
            (
                self.is_default_state,
                self.retries,
                self.player_name.clone(),
                self.mode,
                self.keypress_mode,
                self.mods.clone(),
                self.leaderboard_visible,
                self.leaderboard_player.clone(),
                self.leaderboard_scores.clone(),
            )
        });
        *self = Self::default();
        self.grade_current = calculate_grade(
            source == ClientType::Lazer,
            &grade_mods,
            grade_mode as i32,
            self.accuracy,
            &self.statistics,
        );
        self.grade_expected.clone_from(&self.grade_current);
        if let Some((
            is_default_state,
            retries,
            player_name,
            mode,
            keypress_mode,
            mods,
            visible,
            player,
            scores,
        )) = retained
        {
            self.is_default_state = is_default_state;
            self.retries = retries;
            self.player_name = player_name;
            self.mode = mode;
            self.keypress_mode = keypress_mode;
            self.mods = mods;
            self.leaderboard_visible = visible;
            self.leaderboard_player = player;
            self.leaderboard_scores = scores;
        }
    }

    pub(super) fn reset_quick(&mut self) {
        self.previous_play_time = 0.0;
        self.calculated = ProgressiveAttributes::default();
    }

    pub(super) fn reset_paused_regular(&mut self, score: Option<&Score>) {
        self.is_default_state = false;
        if let Some(score) = score {
            self.mode = score.mode;
            if !score.mods.array.is_empty() {
                self.mods.clone_from(&score.mods);
            }
        }
        self.failed = false;
        self.retries = 0;
        self.player_name.clear();
        self.score = 0.0;
        self.player_hp_smooth = 0.0;
        self.player_hp = 0.0;
        self.accuracy = 100.0;
        self.statistics = default_statistics();
        self.maximum_statistics = default_statistics();
        self.combo = 0;
        self.max_combo = 0;
        self.combo_previous = 0;
        self.hit_miss_previous = 0;
        self.slider_breaks = 0;
        self.grade_current.clear();
        self.grade_expected.clear();
        self.leaderboard_visible = false;
        self.leaderboard_player = LeaderboardPlayerState::default();
        self.leaderboard_scores.clear();
        self.calculated = ProgressiveAttributes::default();
    }

    pub(super) fn reset_hit_errors(&mut self) {
        self.hit_errors.all.clear();
        self.hit_errors.press.clear();
        self.hit_errors.release.clear();
        self.total_hit_errors = 0.0;
        self.previous_hit_error_index = 0;
    }

    pub(super) fn reset_keys(&mut self) {
        self.keypress_identity_mode = None;
        if self.is_key_overlay_default_state {
            return;
        }
        for key in &mut self.key_overlay {
            key.is_pressed = false;
            key.count = 0;
        }
        for key in &mut self.keypresses {
            key.is_pressed = false;
            key.count = 0;
            key.pressed_at = None;
            key.released_at = None;
        }
        self.is_key_overlay_default_state = true;
    }

    pub(super) fn apply_score(&mut self, result: &Score, source: ClientType, object_count: i64) {
        self.is_default_state = false;
        self.failed = result.failed;
        self.retries = result.retries;
        self.player_name.clone_from(&result.player_name);
        self.mods = result.mods.clone();
        self.mode = result.mode;
        self.keypress_mode = result.mode;
        self.score = result.score;
        self.player_hp_smooth = result.player_hp_smooth;
        self.player_hp = result.player_hp;
        self.accuracy = result.accuracy;
        self.statistics = result.statistics.clone();
        self.maximum_statistics = result.maximum_statistics.clone();
        self.combo = result.combo;
        self.max_combo = result.max_combo;
        if self.max_combo > 0 {
            self.update_unstable_rate();
        }
        if self.combo_previous > self.max_combo {
            self.combo_previous = 0;
        }
        if self.combo < self.combo_previous && self.statistics.miss == self.hit_miss_previous {
            self.slider_breaks += 1;
        }
        self.hit_miss_previous = self.statistics.miss;
        self.combo_previous = self.combo;
        self.grade_current = calculate_grade(
            source == ClientType::Lazer,
            &self.mods.array,
            self.mode as i32,
            self.accuracy,
            &self.statistics,
        );
        let mut expected = self.statistics.clone();
        expected.great += (object_count
            - expected.great
            - expected.good
            - expected.ok
            - expected.meh
            - expected.miss)
            .max(0);
        self.grade_expected = calculate_grade(
            source == ClientType::Lazer,
            &self.mods.array,
            self.mode as i32,
            self.accuracy,
            &expected,
        );
    }

    fn update_unstable_rate(&mut self) {
        let base = self.calculate_ur();
        self.unstable_rate = if self.mods.number & 64 == 64 {
            base / 1.5
        } else if self.mods.number & 256 == 256 {
            base * 1.33
        } else {
            base
        };
    }

    pub(super) fn calculate_ur(&self) -> f64 {
        if self.hit_errors.all.is_empty() {
            return 0.0;
        }
        let mean = self.total_hit_errors / self.hit_errors.all.len() as f64;
        let variance = self
            .hit_errors
            .all
            .iter()
            .map(|hit| (hit - mean).powi(2))
            .sum::<f64>()
            / self.hit_errors.all.len() as f64;
        variance.sqrt() * 10.0
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResultScreenState {
    pub raw: RawResultScreen,
    pub grade: String,
    pub pp: f64,
    pub fc_pp: f64,
    pub previous_beatmap: String,
}

impl Default for ResultScreenState {
    fn default() -> Self {
        Self {
            raw: RawResultScreen {
                online_id: 0.0,
                player_name: String::new(),
                mods: default_calculated_mods(),
                mode: 0,
                accuracy: 0.0,
                max_combo: 0,
                score: 0.0,
                statistics: default_statistics(),
                maximum_statistics: default_statistics(),
                date: String::new(),
            },
            grade: String::new(),
            pp: 0.0,
            fc_pp: 0.0,
            previous_beatmap: String::new(),
        }
    }
}

impl ResultScreenState {
    pub(super) fn apply(&mut self, raw: &RawResultScreen, source: ClientType) {
        self.raw = raw.clone();
        self.grade = calculate_grade(
            source == ClientType::Lazer,
            &raw.mods.array,
            raw.mode as i32,
            raw.accuracy,
            &raw.statistics,
        );
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TourneyUserState {
    pub accuracy: f64,
    pub ranked_score: f64,
    pub play_count: i64,
    pub global_rank: i64,
    pub pp: f64,
    pub name: String,
    pub country: String,
    pub id: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TourneyManagerState {
    pub is_default_state: bool,
    pub scoreboard: RawTourney,
    pub messages: Vec<TourneyManagerChatItem>,
    pub user: Option<TourneyUserState>,
}

impl Default for TourneyManagerState {
    fn default() -> Self {
        Self {
            is_default_state: true,
            scoreboard: RawTourney {
                ipc_state: 0,
                left_stars: 0,
                right_stars: 0,
                best_of: 0,
                stars_visible: false,
                score_visible: false,
                first_team_name: String::new(),
                second_team_name: String::new(),
                first_team_score: 0,
                second_team_score: 0,
            },
            messages: Vec::new(),
            user: None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct UserState {
    pub profile: Option<RawUser>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct LazerMultiSpectatingState {
    pub data: Option<LazerSpectator>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct RankedPlayState {
    pub info: Option<RawRankedPlay>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RegularState {
    pub global: GlobalState,
    pub settings: SettingsState,
    pub beatmap: BeatmapPPState,
    pub menu: MenuState,
    pub bass_density: BassDensityState,
    pub gameplay: GameplayState,
    pub result_screen: ResultScreenState,
    pub tourney: TourneyManagerState,
    pub user: UserState,
    pub lazer_multi_spectating: LazerMultiSpectatingState,
    pub ranked_play: RankedPlayState,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PreciseState {
    pub status: i64,
    pub play_time: f64,
    pub mode: i64,
    pub key_overlay: Arc<Vec<KeyOverlayButton>>,
    pub keypresses: Arc<Vec<KeypressButton>>,
    pub hit_errors: Arc<HitErrorArray>,
    pub total_hit_errors: f64,
    pub committed_hit_error_index: i64,
    pub unstable_rate: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StateBundleSnapshot {
    pub source: ClientType,
    pub generation: u64,
    pub reset_epoch: u64,
    pub regular: Arc<RegularState>,
    pub precise: Arc<PreciseState>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CalculationPhase {
    Menu,
    Editor,
    Gameplay,
    Result,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateCalculationKey {
    pub instance_generation: u64,
    pub reset_epoch: u64,
    pub checksum: String,
    pub mode: i64,
    pub mods_checksum: String,
}

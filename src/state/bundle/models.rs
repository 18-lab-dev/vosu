use super::*;

#[derive(Debug, Clone, PartialEq)]
pub struct LeaderboardPlayerState {
    pub user_id: Option<i64>,
    pub name: String,
    pub score: f64,
    pub combo: i64,
    pub accuracy: f64,
    pub max_combo: i64,
    pub mods: CalculateMods,
    pub statistics: Statistics,
    pub team: i64,
    pub position: i64,
    pub is_passing: bool,
}

impl Default for LeaderboardPlayerState {
    fn default() -> Self {
        Self {
            user_id: None,
            name: String::new(),
            score: 0.0,
            combo: 0,
            accuracy: 100.0,
            max_combo: 0,
            mods: default_calculated_mods(),
            statistics: default_statistics(),
            team: 0,
            position: 0,
            is_passing: false,
        }
    }
}

impl From<&LeaderboardPlayer> for LeaderboardPlayerState {
    fn from(player: &LeaderboardPlayer) -> Self {
        Self {
            user_id: Some(player.user_id),
            name: player.name.clone(),
            score: player.score,
            combo: player.combo,
            accuracy: player.accuracy,
            max_combo: player.max_combo,
            mods: player.mods.clone(),
            statistics: player.statistics.clone(),
            team: player.team,
            position: player.position,
            is_passing: player.is_passing,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SettingsState {
    pub values: SettingsObject,
}

impl Default for SettingsState {
    fn default() -> Self {
        let mut values = IndexMap::new();
        values.insert(
            "audio".into(),
            json!({
                "ignoreBeatmapSounds": false, "useSkinSamples": false,
                "volume": {"masterInactive": 0, "master": 0, "music": 0, "effect": 0},
                "offset": {"universal": 0}
            }),
        );
        values.insert(
            "background".into(),
            json!({"storyboard": false, "video": false, "blur": 0, "dim": 0}),
        );
        values.insert(
            "client".into(),
            json!({"updateAvailable": false, "branch": 0, "version": ""}),
        );
        values.insert(
            "resolution".into(),
            json!({
                "fullscreen": false, "width": 0, "height": 0,
                "widthFullscreen": 0, "heightFullscreen": 0
            }),
        );
        values.insert("scoreMeter".into(), json!({"type": 0, "size": 0}));
        values.insert(
            "cursor".into(),
            json!({"useSkinCursor": false, "autoSize": false, "size": 0, "menuSize": 0}),
        );
        values.insert("mouse".into(), json!({"rawInput": false, "disableButtons": false, "disableWheel": false, "sensitivity": 0}));
        values.insert(
            "tablet".into(),
            json!({
                "enabled": false, "x": 0, "y": 0, "width": 0, "height": 0,
                "rotation": 0, "pressureThreshold": 0
            }),
        );
        values.insert(
            "mania".into(),
            json!({
                "speedBPMScale": false, "usePerBeatmapSpeedScale": false,
                "scrollSpeed": 0, "scrollDirection": 1,
                "timingBasedNoteColouring": null, "perBeatmapScrollSpeed": null
            }),
        );
        values.insert("visual".into(), json!({
            "snakingInSliders": null, "snakingOutSliders": null,
            "hitLighting": null, "beatmapColours": null,
            "comboColourNormalisation": null,
            "hitAnimations": null, "rateAdjustedHitAnimation": null,
            "scalingMode": null, "positionX": null, "positionY": null, "sizeX": null, "sizeY": null
        }));
        values.insert(
            "skin".into(),
            json!({
                "useDefaultSkinInEditor": false, "ignoreBeatmapSkins": false,
                "tintSliderBall": false, "useTaikoSkin": false, "name": ""
            }),
        );
        values.insert(
            "keybinds".into(),
            json!({
                "osu": {"k1": "", "k2": "", "smokeKey": ""},
                "fruits": {"k1": "", "k2": "", "Dash": ""},
                "taiko": {"innerLeft": "", "innerRight": "", "outerLeft": "", "outerRight": ""},
                "quickRetry": ""
            }),
        );
        values.insert("groupType".into(), json!(0));
        values.insert("sortType".into(), json!(0));
        values.insert("leaderboardType".into(), json!(0));
        values.insert("progressBarType".into(), json!(0));
        Self { values }
    }
}

impl SettingsState {
    pub(super) fn apply(&mut self, update: &SettingsObject) {
        for (path, value) in update {
            set_nested(&mut self.values, path, value.clone());
        }
    }
}

fn set_nested(root: &mut SettingsObject, path: &str, value: Value) {
    let mut parts = path.split('.');
    let Some(first) = parts.next() else { return };
    let remaining = parts.collect::<Vec<_>>();
    if remaining.is_empty() {
        if root.contains_key(first) {
            root.insert(first.to_owned(), value);
        }
        return;
    }
    let Some(mut current) = root.get_mut(first) else {
        return;
    };
    for component in &remaining[..remaining.len() - 1] {
        let Some(next) = current
            .as_object_mut()
            .and_then(|object| object.get_mut(*component))
        else {
            return;
        };
        current = next;
    }
    if let Some(object) = current.as_object_mut() {
        let leaf = remaining[remaining.len() - 1];
        if object.contains_key(leaf) {
            object.insert(leaf.to_owned(), value);
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct GlobalState {
    pub is_watching_replay: bool,
    pub is_replay_ui_hidden: bool,
    pub is_multi_spectating: bool,
    pub show_interface: bool,
    pub chat_status: i64,
    pub status: i64,
    pub paused: bool,
    pub game_time: i64,
    pub play_time: f64,
    pub previous_play_time: f64,
    pub menu_mods: CalculateMods,
    pub game_folder: String,
    pub skin_folder: String,
    pub current_skin: Option<std::sync::Arc<crate::model::memory::CurrentSkin>>,
    pub render: Option<crate::model::memory::ClientRenderState>,
    pub songs_folder: String,
    pub memory_songs_folder: String,
}

impl Default for GlobalState {
    fn default() -> Self {
        Self {
            is_watching_replay: false,
            is_replay_ui_hidden: false,
            is_multi_spectating: false,
            show_interface: false,
            chat_status: 0,
            status: 0,
            paused: false,
            game_time: 0,
            play_time: 0.0,
            previous_play_time: 0.0,
            menu_mods: default_calculated_mods(),
            game_folder: String::new(),
            skin_folder: String::new(),
            current_skin: None,
            render: None,
            songs_folder: String::new(),
            memory_songs_folder: String::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct BeatmapPPState {
    pub checksum: Option<String>,
    pub mode: Option<i64>,
    pub mods: CalculateMods,
    pub first_object_time: Option<i64>,
    pub full_pp: f64,
    pub attributes: Option<ProgressiveAttributes>,
    pub pp_acc: BTreeMap<u8, f64>,
    pub source_key: Option<String>,
    /// Lossless parsed/calculated map projection shared without copying.
    pub parsed: Option<Arc<ParsedBeatmap>>,
    pub graph: Arc<StrainGraph>,
    pub graph_mp3_length: Option<f64>,
    pub event_status: EventStatus,
}

impl Default for BeatmapPPState {
    fn default() -> Self {
        Self {
            checksum: None,
            mode: None,
            mods: default_calculated_mods(),
            first_object_time: None,
            full_pp: 0.0,
            attributes: None,
            pp_acc: default_pp_acc(),
            source_key: None,
            parsed: None,
            graph: Arc::new(StrainGraph::default()),
            graph_mp3_length: None,
            event_status: EventStatus::default(),
        }
    }
}

impl PartialEq for BeatmapPPState {
    fn eq(&self, other: &Self) -> bool {
        self.checksum == other.checksum
            && self.mode == other.mode
            && self.mods == other.mods
            && self.first_object_time == other.first_object_time
            && self.full_pp == other.full_pp
            && self.attributes == other.attributes
            && self.pp_acc == other.pp_acc
            && self.source_key == other.source_key
            && Arc::ptr_eq(&self.graph, &other.graph)
            && self.graph_mp3_length == other.graph_mp3_length
            && self.event_status == other.event_status
            && match (&self.parsed, &other.parsed) {
                (Some(left), Some(right)) => Arc::ptr_eq(left, right),
                (None, None) => true,
                _ => false,
            }
    }
}

impl BeatmapPPState {
    pub(super) fn reset_attributes(&mut self) {
        self.attributes = None;
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct MenuState {
    pub gamemode: Option<i64>,
    pub plays: Option<i64>,
    pub artist: Option<String>,
    pub artist_original: Option<String>,
    pub title: Option<String>,
    pub title_original: Option<String>,
    pub ar: Option<f64>,
    pub cs: Option<f64>,
    pub hp: Option<f64>,
    pub od: Option<f64>,
    pub audio_filename: Option<String>,
    pub audio_file_mimetype: Option<String>,
    pub background_filename: Option<String>,
    pub background_file_mimetype: Option<String>,
    pub folder: Option<String>,
    pub creator: Option<String>,
    pub filename: Option<String>,
    pub difficulty: Option<String>,
    pub map_id: Option<i64>,
    pub set_id: Option<i64>,
    pub ranked_status: Option<f64>,
    pub checksum: Option<String>,
    pub object_count: Option<i64>,
    pub mp3_length: Option<f64>,
    pub(super) previous_md5: String,
    pub(super) pending_checksum: String,
    pub(super) map_change_time: Option<Instant>,
}

impl MenuState {
    pub(super) fn apply(&mut self, update: &RawMenu, tournament: bool, now: Instant) -> bool {
        match update {
            RawMenu::Checksum(short) => {
                self.gamemode = Some(short.gamemode);
                self.ranked_status = Some(short.ranked_status);
                true
            }
            RawMenu::Full(full) => self.apply_full(full, tournament, now),
        }
    }

    fn apply_full(&mut self, full: &MenuFull, tournament: bool, now: Instant) -> bool {
        if tournament && self.pending_checksum != full.checksum {
            self.pending_checksum.clone_from(&full.checksum);
            self.map_change_time = Some(now);
            return false;
        }
        if tournament
            && self.map_change_time.is_some_and(|started| {
                now.saturating_duration_since(started) < Duration::from_millis(500)
            })
        {
            return false;
        }
        self.gamemode = Some(full.gamemode);
        self.checksum = Some(full.checksum.clone());
        self.filename = Some(safe_join([full.filename.as_str()]));
        self.plays = Some(full.plays);
        self.artist = Some(full.artist.clone());
        self.artist_original = Some(full.artist_original.clone());
        self.title = Some(full.title.clone());
        self.title_original = Some(full.title_original.clone());
        self.ar = Some(full.ar);
        self.cs = Some(full.cs);
        self.hp = Some(full.hp);
        self.od = Some(full.od);
        self.audio_filename = Some(safe_join([full.audio_filename.as_str()]));
        self.audio_file_mimetype = Some(full.audio_file_mimetype.clone());
        self.background_filename = Some(safe_join([full.background_filename.as_str()]));
        self.background_file_mimetype = Some(full.background_file_mimetype.clone());
        self.folder = Some(safe_join([full.folder.as_str()]));
        self.creator = Some(full.creator.clone());
        self.difficulty = Some(full.difficulty.clone());
        self.map_id = Some(full.map_id);
        self.set_id = Some(full.set_id);
        self.ranked_status = Some(full.ranked_status);
        self.object_count = Some(full.object_count);
        self.previous_md5.clone_from(&full.checksum);
        true
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct BassDensityState {
    pub current_audio_velocity: f64,
    pub density: f64,
}

impl BassDensityState {
    pub(super) fn apply(&mut self, values: &[f64]) {
        let mut bass = 0.0;
        for index in 0..40 {
            let value = values.get(index).copied().unwrap_or(f64::NAN);
            if value < 0.0 {
                self.density = 0.5;
                return;
            }
            bass += (2.0 * value * (40 - index) as f64) / 40.0;
        }
        if !self.current_audio_velocity.is_finite() || !bass.is_finite() {
            self.current_audio_velocity = 0.0;
            self.density = 0.5;
            return;
        }
        self.current_audio_velocity = self.current_audio_velocity.max((bass * 1.5).min(6.0)) * 0.95;
        self.density = (1.0 + self.current_audio_velocity) * 0.5;
    }
}

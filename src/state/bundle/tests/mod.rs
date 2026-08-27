use super::*;
use crate::memory::snapshot::{
    PreciseCaptureContext, RawPreciseSnapshotBuilder, RawRegularSnapshotBuilder,
    RegularCaptureContext,
};
use crate::model::memory::{Global as RawGlobal, GlobalPrecise, HitErrors, ReleaseCorrection};
use crate::process::ReadStatus;

fn press_hit_errors(values: Vec<f64>) -> HitErrorArray {
    HitErrorArray {
        all: values.clone(),
        press: values,
        release: Vec::new(),
    }
}

fn hit_error_suffix(index: i64, values: Vec<f64>) -> HitErrors {
    HitErrors {
        index,
        errors: press_hit_errors(values),
        release_corrections: Vec::new(),
        stable_replay_frames: None,
    }
}
struct RecordingCalculation {
    requests: Arc<std::sync::Mutex<Vec<StateCalculationRequest>>>,
}

impl StateCalculationBackend for RecordingCalculation {
    fn calculate<'a>(&'a self, request: StateCalculationRequest) -> StateCalculationFuture<'a> {
        self.requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(request.clone());
        Box::pin(async move {
            Ok(StateCalculationResult {
                key: request.key,
                beatmap: Some(BeatmapCalculationUpdate {
                    first_object_time: Some(100),
                    full_pp: 1.0,
                    source_key: Some("parsed".into()),
                    ..BeatmapCalculationUpdate::default()
                }),
                gameplay: None,
                result: None,
            })
        })
    }
}

struct RecordingEmptyCalculation {
    requests: Arc<std::sync::Mutex<Vec<StateCalculationRequest>>>,
}

impl StateCalculationBackend for RecordingEmptyCalculation {
    fn calculate<'a>(&'a self, request: StateCalculationRequest) -> StateCalculationFuture<'a> {
        self.requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(request.clone());
        Box::pin(async move {
            Ok(StateCalculationResult {
                key: request.key,
                beatmap: None,
                gameplay: None,
                result: None,
            })
        })
    }
}

struct ControlledCalculation {
    requests: Arc<std::sync::Mutex<Vec<StateCalculationRequest>>>,
    failures: Arc<std::sync::atomic::AtomicUsize>,
}

impl StateCalculationBackend for ControlledCalculation {
    fn calculate<'a>(&'a self, request: StateCalculationRequest) -> StateCalculationFuture<'a> {
        self.requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(request.clone());
        let fail = self
            .failures
            .fetch_update(
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
                |remaining| remaining.checked_sub(1),
            )
            .is_ok();
        Box::pin(async move {
            if fail {
                return Err(StateWorkerError::Transition("transient calculation".into()));
            }
            Ok(StateCalculationResult {
                key: request.key,
                beatmap: Some(BeatmapCalculationUpdate {
                    first_object_time: Some(100),
                    full_pp: 1.0,
                    source_key: Some("parsed".into()),
                    ..BeatmapCalculationUpdate::default()
                }),
                gameplay: None,
                result: None,
            })
        })
    }
}

struct ProjectionCalculation {
    calls: std::sync::atomic::AtomicUsize,
}

impl StateCalculationBackend for ProjectionCalculation {
    fn calculate<'a>(&'a self, request: StateCalculationRequest) -> StateCalculationFuture<'a> {
        let value = self.calls.fetch_add(1, std::sync::atomic::Ordering::AcqRel) as f64 + 1.0;
        Box::pin(async move {
            Ok(StateCalculationResult {
                key: request.key,
                beatmap: Some(BeatmapCalculationUpdate {
                    original_mode: None,
                    event_only: false,
                    graph_event_only: false,
                    retry_state_key: false,
                    background_filename: None,
                    first_object_time: Some(100),
                    full_pp: value,
                    source_key: Some(format!("projection-{value}")),
                    pp_acc: None,
                    graph: Arc::new(StrainGraph {
                        primary: vec![value],
                        series: Vec::new(),
                        x_axis: vec![value],
                    }),
                    event_status: EventStatus {
                        bpm: value,
                        kiai: value > 1.0,
                        break_time: false,
                    },
                    parsed: None,
                    graph_mp3_length: None,
                }),
                gameplay: None,
                result: None,
            })
        })
    }
}

struct EchoCalculation;

impl StateCalculationBackend for EchoCalculation {
    fn calculate<'a>(&'a self, request: StateCalculationRequest) -> StateCalculationFuture<'a> {
        Box::pin(async move {
            Ok(StateCalculationResult {
                key: request.key,
                beatmap: None,
                gameplay: None,
                result: None,
            })
        })
    }
}

struct MismatchedCalculation;

impl StateCalculationBackend for MismatchedCalculation {
    fn calculate<'a>(&'a self, request: StateCalculationRequest) -> StateCalculationFuture<'a> {
        Box::pin(async move {
            let mut key = request.key;
            key.mods_checksum.push_str("-stale");
            Ok(StateCalculationResult {
                key,
                beatmap: Some(BeatmapCalculationUpdate {
                    first_object_time: Some(100),
                    full_pp: 999.0,
                    source_key: Some("stale".into()),
                    ..BeatmapCalculationUpdate::default()
                }),
                gameplay: None,
                result: Some(ResultPerformance {
                    pp: 999.0,
                    fc_pp: 999.0,
                }),
            })
        })
    }
}

fn raw_global(status: GameState, time: i64) -> RawGlobal {
    RawGlobal {
        is_watching_replay: false,
        is_replay_ui_hidden: false,
        is_multi_spectating: false,
        show_interface: true,
        chat_status: 0,
        status: status.value() as i64,
        paused: None,
        play_time: time as f64,
        game_time: time,
        menu_mods: default_calculated_mods(),
        skin_folder: "skin".into(),
        memory_songs_folder: "songs".into(),
    }
}

fn raw_global_paused(status: GameState, time: i64, paused: bool) -> RawGlobal {
    let mut global = raw_global(status, time);
    global.paused = Some(paused);
    global
}

fn menu(checksum: &str) -> MenuFull {
    MenuFull {
        gamemode: 0,
        checksum: checksum.into(),
        filename: "map.osu".into(),
        plays: 2,
        artist: "artist".into(),
        artist_original: "artist-original".into(),
        title: "title".into(),
        title_original: "title-original".into(),
        ar: 9.0,
        cs: 4.0,
        hp: 6.0,
        od: 8.0,
        audio_filename: "audio.mp3".into(),
        audio_file_mimetype: "audio/mpeg".into(),
        background_filename: "bg.jpg".into(),
        background_file_mimetype: "image/jpeg".into(),
        folder: "set".into(),
        creator: "mapper".into(),
        difficulty: "Insane".into(),
        map_id: 1,
        set_id: 2,
        ranked_status: 4.0,
        object_count: 100,
    }
}

fn score(mode: i64, combo: i64) -> Score {
    Score {
        failed: false,
        retries: 4,
        player_name: "player".into(),
        mods: default_calculated_mods(),
        mode,
        score: 123.0,
        player_hp_smooth: 0.75,
        player_hp: 0.8,
        accuracy: 98.5,
        statistics: default_statistics(),
        maximum_statistics: default_statistics(),
        combo,
        max_combo: combo,
        pp: None,
    }
}

fn raw_leaderboard_player() -> LeaderboardPlayer {
    LeaderboardPlayer {
        user_id: 1,
        name: "leader".into(),
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

mod calculation;
mod epochs;
mod transitions;

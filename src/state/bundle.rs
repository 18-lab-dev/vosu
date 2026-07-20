use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::watch;

use indexmap::IndexMap;
use serde_json::{Value, json};

use super::{
    PreciseTransition, ReadContext, RegularCapturePlan, RegularTransition, SnapshotTransition,
    StateWorkerError, TransitionFuture,
};
use crate::calculation::beatmap::{EventStatus, ParsedBeatmap, StrainGraph};
use crate::calculation::progressive::{ProgressiveAttributes, ResultPerformance};
use crate::calculation::{calculate_grade, default_calculated_mods, safe_join};
use crate::memory::snapshot::{RawPreciseSnapshot, RawRegularSnapshot};
use crate::model::enums::{ClientType, GameState};
use crate::model::memory::{
    LazerSpectator, Menu as RawMenu, MenuFull, RankedPlay as RawRankedPlay,
    ResultScreen as RawResultScreen, Score, Tourney as RawTourney, User as RawUser,
};
use crate::model::states::{
    CalculateMods, HitErrorArray, KeyOverlayButton, KeypressButton, LeaderboardPlayer,
    SettingsObject, Statistics, TourneyManagerChatItem,
};
use crate::process::{MemoryRead, ReadStatus};

fn ready<T>(read: &MemoryRead<T>) -> Option<&T> {
    match read {
        Ok(ReadStatus::Ready(value)) => Some(value),
        _ => None,
    }
}

fn default_statistics() -> Statistics {
    Statistics {
        perfect: 0,
        great: 0,
        good: 0,
        ok: 0,
        meh: 0,
        miss: 0,
        small_tick_miss: 0,
        small_tick_hit: 0,
        large_tick_miss: 0,
        large_tick_hit: 0,
        small_bonus: 0,
        large_bonus: 0,
        ignore_miss: 0,
        ignore_hit: 0,
        combo_break: 0,
        slider_tail_hit: 0,
        legacy_combo_increase: Some(0),
    }
}

fn default_pp_acc() -> BTreeMap<u8, f64> {
    (90..=100).map(|accuracy| (accuracy, 0.0)).collect()
}

#[path = "bundle/calculation.rs"]
mod calculation;
#[path = "bundle/gameplay.rs"]
mod gameplay;
#[path = "bundle/models.rs"]
mod models;
#[path = "bundle/reducer.rs"]
mod reducer;

pub use calculation::*;
pub use gameplay::*;
pub use models::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CalculationApplyOutcome {
    Mismatched,
    MatchedEmpty,
    Applied,
    AppliedRetryable,
}

pub struct StateBundle {
    source: ClientType,
    generation: u64,
    reset_epoch: u64,
    calculation_reset_generation: u64,
    instance_root: PathBuf,
    global: GlobalState,
    settings: SettingsState,
    beatmap: BeatmapPPState,
    menu: MenuState,
    bass_density: BassDensityState,
    gameplay: GameplayState,
    result_screen: ResultScreenState,
    tourney: TourneyManagerState,
    user: UserState,
    lazer_multi_spectating: LazerMultiSpectatingState,
    accepted_precise_sequence: u64,
    latest_precise_global_sequence: u64,
    ranked_play: RankedPlayState,
    previous_state_key: String,
    previous_mp3_length: Option<f64>,
    previous_time: f64,
    is_tourney_manager: bool,
    is_tourney_spectator: bool,
    calculate_pp: bool,
    read_mania_scroll_speed: bool,
    show_mp_commands: bool,
    calculation: Arc<dyn StateCalculationBackend>,
    calculation_failures: u64,
    published_regular: Option<Arc<RegularState>>,
    published_precise: Option<Arc<PreciseState>>,
    precise_dirty: bool,
    config_updates: Option<watch::Receiver<Arc<crate::config::Config>>>,
}

#[cfg(test)]
#[path = "bundle/tests/mod.rs"]
mod tests;

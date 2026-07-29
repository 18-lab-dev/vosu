use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use tokio::sync::{Notify, watch};
use tokio::time::{sleep, timeout};

use super::*;
use crate::memory::snapshot::{
    PreciseCaptureContext, RawPreciseSnapshot, RawPreciseSnapshotBuilder, RawRegularSnapshot,
    RawRegularSnapshotBuilder, RegularCaptureContext,
};
use crate::memory::{MemoryReader, ReaderSynchronization, SynchronizedMemoryReader};
use crate::model::enums::{ClientType, GameState};
use crate::model::memory::{
    AudioVelocityBase, Gameplay, Global, GlobalPrecise, HitErrors, KeyOverlay, LazerSpectator,
    Leaderboard, Menu, Mp3Length, RankedPlay, ResultScreen, Settings, Tourney, TourneyChat,
    TourneyUser, User,
};
use crate::model::states::{CalculateMods, HitErrorArray, Statistics, TourneyManagerChatItem};
use crate::process::{MemoryRead, ReadStatus};

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn mods(checksum: &str) -> CalculateMods {
    CalculateMods {
        checksum: checksum.into(),
        number: 0,
        name: String::new(),
        array: Vec::new(),
        rate: 1.0,
    }
}

fn context(source: ClientType) -> ReadContext {
    ReadContext {
        source,
        instance_generation: 7,
        reset_epoch: 3,
        status: GameState::Play.value() as i64,
        play_time: 200.0,
        mode: 0,
        mods: mods("nomod"),
        committed_hit_error_index: 0,
        accepted_precise_sequence: 0,
        latest_precise_global_sequence: 0,
    }
}

fn raw_global() -> Global {
    Global {
        is_watching_replay: false,
        is_replay_ui_hidden: false,
        is_multi_spectating: false,
        show_interface: true,
        chat_status: 0,
        status: GameState::Play.value() as i64,
        play_time: 200.0,
        game_time: 0,
        menu_mods: mods("nomod"),
        skin_folder: String::new(),
        memory_songs_folder: String::new(),
    }
}

fn zero_statistics() -> Statistics {
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
        legacy_combo_increase: None,
    }
}

fn raw_gameplay(mode: i64) -> Gameplay {
    Gameplay {
        failed: false,
        retries: 0,
        player_name: String::new(),
        mods: mods("nomod"),
        mode,
        score: 0.0,
        player_hp_smooth: 0.0,
        player_hp: 0.0,
        accuracy: 0.0,
        statistics: zero_statistics(),
        maximum_statistics: zero_statistics(),
        combo: 0,
        max_combo: 0,
        pp: None,
    }
}

fn raw_tourney() -> Tourney {
    Tourney {
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
    }
}

#[derive(Default)]
struct ManualClock {
    cancelled: AtomicBool,
    state: Mutex<ManualClockState>,
    changed: Condvar,
}

#[derive(Default)]
struct ManualClockState {
    observed: Vec<Duration>,
    releases: VecDeque<Duration>,
}

impl ManualClock {
    fn observed_count(&self, duration: Duration) -> usize {
        lock(&self.state)
            .observed
            .iter()
            .filter(|value| **value == duration)
            .count()
    }

    fn advance(&self, duration: Duration) {
        lock(&self.state).releases.push_back(duration);
        self.changed.notify_all();
    }
}

impl WorkerClock for ManualClock {
    fn sleep(&self, duration: Duration) -> bool {
        let mut state = lock(&self.state);
        state.observed.push(duration);
        self.changed.notify_all();
        loop {
            if self.cancelled.load(Ordering::Acquire) {
                return false;
            }
            if let Some(position) = state.releases.iter().position(|value| *value == duration) {
                state.releases.remove(position);
                return true;
            }
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    fn cancel(&self) {
        let _state = lock(&self.state);
        self.cancelled.store(true, Ordering::Release);
        self.changed.notify_all();
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

struct FakeRegular {
    captures: Arc<Mutex<Vec<u64>>>,
}

impl RegularSnapshotReader for FakeRegular {
    fn capture_regular(
        &mut self,
        request: &RegularCaptureRequest,
    ) -> Result<Arc<RawRegularSnapshot>, StateWorkerError> {
        lock(&self.captures).push(request.context.produced_sequence);
        Ok(
            RawRegularSnapshotBuilder::new(request.context, Ok(ReadStatus::Ready(raw_global())))
                .publish(),
        )
    }
}

struct BlockedRegular {
    gate: Arc<(Mutex<bool>, Condvar)>,
}

impl RegularSnapshotReader for BlockedRegular {
    fn capture_regular(
        &mut self,
        request: &RegularCaptureRequest,
    ) -> Result<Arc<RawRegularSnapshot>, StateWorkerError> {
        let (open, changed) = &*self.gate;
        let open = lock(open);
        drop(
            changed
                .wait_while(open, |value| !*value)
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        Ok(
            RawRegularSnapshotBuilder::new(request.context, Ok(ReadStatus::Ready(raw_global())))
                .publish(),
        )
    }
}

struct GatedRecordingRegular {
    gate: Arc<(Mutex<bool>, Condvar)>,
    captures: Arc<Mutex<Vec<RegularCaptureContext>>>,
}

impl RegularSnapshotReader for GatedRecordingRegular {
    fn capture_regular(
        &mut self,
        request: &RegularCaptureRequest,
    ) -> Result<Arc<RawRegularSnapshot>, StateWorkerError> {
        lock(&self.captures).push(request.context);
        if request.context.epoch.reset_epoch == 3 {
            let (open, changed) = &*self.gate;
            let open = lock(open);
            drop(
                changed
                    .wait_while(open, |value| !*value)
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
            );
        }
        Ok(
            RawRegularSnapshotBuilder::new(request.context, Ok(ReadStatus::Ready(raw_global())))
                .publish(),
        )
    }
}

struct FakePrecise {
    captures: Arc<Mutex<Vec<PreciseCaptureContext>>>,
    corrupt_first_mode: bool,
}

impl PreciseSnapshotReader for FakePrecise {
    fn capture_precise(
        &mut self,
        context: &ReadContext,
        produced_sequence: u64,
        _synchronization: ReaderSynchronization,
    ) -> Result<Arc<RawPreciseSnapshot>, StateWorkerError> {
        let mut capture = context.precise_capture();
        if self.corrupt_first_mode && produced_sequence == 1 {
            capture = PreciseCaptureContext::new(
                context.source,
                context.instance_generation,
                context.reset_epoch,
                context.mode + 1,
                context.mods.clone(),
                context.committed_hit_error_index,
                context.accepted_precise_sequence,
            );
        }
        lock(&self.captures).push(capture.clone());
        Ok(RawPreciseSnapshotBuilder::new(
            capture,
            produced_sequence,
            Ok(ReadStatus::Ready(GlobalPrecise {
                status: context.status,
                time: context.play_time,
            })),
        )
        .hit_errors(Ok(ReadStatus::Ready(HitErrors {
            index: context.committed_hit_error_index + 1,
            errors: HitErrorArray {
                all: vec![1.0],
                press: vec![1.0],
                release: Vec::new(),
            },
            release_corrections: Vec::new(),
        })))
        .publish())
    }
}

struct PanickingRegular {
    entered: Arc<AtomicBool>,
}

impl RegularSnapshotReader for PanickingRegular {
    fn capture_regular(
        &mut self,
        _request: &RegularCaptureRequest,
    ) -> Result<Arc<RawRegularSnapshot>, StateWorkerError> {
        self.entered.store(true, Ordering::Release);
        panic!("regular reader panic fixture");
    }
}

struct BlockingPrecise {
    entered: Arc<AtomicBool>,
    gate: Arc<(Mutex<bool>, Condvar)>,
    dropped: Arc<AtomicBool>,
}

impl PreciseSnapshotReader for BlockingPrecise {
    fn capture_precise(
        &mut self,
        context: &ReadContext,
        produced_sequence: u64,
        _synchronization: ReaderSynchronization,
    ) -> Result<Arc<RawPreciseSnapshot>, StateWorkerError> {
        self.entered.store(true, Ordering::Release);
        let (open, changed) = &*self.gate;
        let open = lock(open);
        drop(
            changed
                .wait_while(open, |value| !*value)
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        Ok(RawPreciseSnapshotBuilder::new(
            context.precise_capture(),
            produced_sequence,
            Ok(ReadStatus::Ready(GlobalPrecise {
                status: context.status,
                time: context.play_time,
            })),
        )
        .publish())
    }
}

impl Drop for BlockingPrecise {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::Release);
    }
}

struct RecordingTransition {
    context: ReadContext,
    regular: Arc<AtomicUsize>,
    precise_sequences: Arc<Mutex<Vec<u64>>>,
}

impl SnapshotTransition for RecordingTransition {
    type Publication = String;

    fn apply_regular<'a>(
        &'a mut self,
        _snapshot: Arc<RawRegularSnapshot>,
    ) -> TransitionFuture<'a, RegularTransition<String>> {
        self.regular.fetch_add(1, Ordering::AcqRel);
        let context = self.context.clone();
        Box::pin(async move {
            Ok(RegularTransition {
                publication: "regular".into(),
                context,
                next_capture: RegularCapturePlan::default(),
            })
        })
    }

    fn apply_precise<'a>(
        &'a mut self,
        snapshot: Arc<RawPreciseSnapshot>,
    ) -> TransitionFuture<'a, PreciseTransition<String>> {
        lock(&self.precise_sequences).push(snapshot.produced_sequence());
        Box::pin(async {
            Ok(PreciseTransition {
                publication: "precise".into(),
                context: None,
            })
        })
    }
}

struct EpochAdvancingTransition {
    context: ReadContext,
    regular: Arc<AtomicUsize>,
}

impl SnapshotTransition for EpochAdvancingTransition {
    type Publication = String;

    fn apply_regular<'a>(
        &'a mut self,
        _snapshot: Arc<RawRegularSnapshot>,
    ) -> TransitionFuture<'a, RegularTransition<String>> {
        self.regular.fetch_add(1, Ordering::AcqRel);
        let context = self.context.clone();
        Box::pin(async move {
            Ok(RegularTransition {
                publication: "regular".into(),
                context,
                next_capture: RegularCapturePlan::default(),
            })
        })
    }

    fn apply_precise<'a>(
        &'a mut self,
        snapshot: Arc<RawPreciseSnapshot>,
    ) -> TransitionFuture<'a, PreciseTransition<String>> {
        self.context.reset_epoch = 4;
        self.context.accepted_precise_sequence = snapshot.produced_sequence();
        let context = self.context.clone();
        Box::pin(async move {
            Ok(PreciseTransition {
                publication: "precise".into(),
                context: Some(context),
            })
        })
    }
}

struct GatedPublisher {
    publications: Arc<Mutex<Vec<String>>>,
    gate_first_regular: AtomicBool,
    gate: Arc<(Mutex<bool>, Condvar)>,
}

impl SnapshotPublisher<String> for GatedPublisher {
    fn publish(&self, publication: String) -> Result<(), StateWorkerError> {
        lock(&self.publications).push(publication.clone());
        if publication == "regular" && self.gate_first_regular.swap(false, Ordering::AcqRel) {
            let (open, changed) = &*self.gate;
            let open = lock(open);
            drop(
                changed
                    .wait_while(open, |value| !*value)
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
            );
        }
        Ok(())
    }
}

struct OpenGateOnDrop(Arc<(Mutex<bool>, Condvar)>);

impl Drop for OpenGateOnDrop {
    fn drop(&mut self) {
        *lock(&self.0.0) = true;
        self.0.1.notify_all();
    }
}

async fn wait_until(mut predicate: impl FnMut() -> bool) {
    timeout(Duration::from_secs(2), async {
        while !predicate() {
            sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("condition timed out");
}

mod readers;
mod worker;

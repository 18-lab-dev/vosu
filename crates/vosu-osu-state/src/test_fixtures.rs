//! `test-harness`-only fixtures shared with the root crate's
//! `subscription_cadence_cpu_and_wakeups` benchmark
//! (`src/server/v2_subscription/cadence_benchmark.rs`).
//!
//! That benchmark drives a real worker through [`crate::spawn_state_worker_with_clock`]
//! against `vosu::server::v2_subscription::V2SubscriptionHub`, a root-crate type;
//! it cannot live in this crate without creating a `vosu-osu-state -> vosu` cycle
//! (`vosu` already depends on `vosu-osu-state`). Rather than duplicate the
//! reader/transition fixtures the in-crate cadence tests
//! (`src/tests/worker.rs`) already define privately, this module re-implements
//! the handful they share (`FakePrecise`, `RecordingTransition`, `TimedRegular`,
//! `DeliveryRecorder`, `context`, `lock`) as a small `pub` surface gated the
//! same way `vosu-net`'s `test-harness` feature is: forbidden outside
//! `debug_assertions` (see `lib.rs`), so it never reaches a release build.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use vosu_osu_model::enums::{ClientType, GameState};
use vosu_osu_model::memory::{Global, GlobalPrecise, HitErrors};
use vosu_osu_model::states::{CalculateMods, HitErrorArray};
use vosu_osu_reader::memory::ReaderSynchronization;
use vosu_osu_reader::memory::snapshot::{
    PreciseCaptureContext, RawPreciseSnapshot, RawPreciseSnapshotBuilder, RawRegularSnapshot,
    RawRegularSnapshotBuilder,
};
use vosu_sys::process::ReadStatus;

use crate::{
    Delivery, PreciseSnapshotReader, PreciseTransition, ReadContext, RegularCaptureRequest,
    RegularSnapshotReader, RegularTransition, SnapshotPublisher, SnapshotTransition,
    StateWorkerError, TransitionFuture,
};

/// Same poisoning-tolerant lock the in-crate cadence tests use, so a panicking
/// worker thread never masks the assertion that caught it.
pub fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
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

/// Same fixed, mid-play [`ReadContext`] the in-crate cadence tests use.
pub fn context(source: ClientType) -> ReadContext {
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
        render: None,
        is_watching_replay: false,
        is_replay_ui_hidden: false,
        is_multi_spectating: false,
        show_interface: true,
        chat_status: 0,
        status: GameState::Play.value() as i64,
        paused: None,
        play_time: 200.0,
        game_time: 0,
        menu_mods: mods("nomod"),
        skin_folder: String::new(),
        current_skin: None,
        memory_songs_folder: String::new(),
    }
}

/// Precise reader that records every captured context and always succeeds.
pub struct FakePrecise {
    pub captures: Arc<Mutex<Vec<PreciseCaptureContext>>>,
    pub corrupt_first_mode: bool,
}

impl PreciseSnapshotReader for FakePrecise {
    fn capture_precise(
        &mut self,
        context: &ReadContext,
        produced_sequence: u64,
        _synchronization: ReaderSynchronization,
    ) -> Result<Arc<RawPreciseSnapshot>, StateWorkerError> {
        let mut capture = PreciseCaptureContext::new(
            context.source,
            context.instance_generation,
            context.reset_epoch,
            context.mode,
            context.mods.clone(),
            context.committed_hit_error_index,
            context.accepted_precise_sequence,
        );
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
            list_len: context.committed_hit_error_index + 1,
            errors: HitErrorArray {
                all: vec![1.0],
                press: vec![1.0],
                release: Vec::new(),
                key: None,
                offsets: Vec::new(),
            },
            release_corrections: Vec::new(),
            stable_replay_frames: None,
        })))
        .publish())
    }
}

/// Regular reader that timestamps every capture request.
pub struct TimedRegular {
    pub captures: Arc<Mutex<Vec<(std::time::Instant, bool)>>>,
}

impl RegularSnapshotReader for TimedRegular {
    fn capture_regular(
        &mut self,
        request: &RegularCaptureRequest,
    ) -> Result<Arc<RawRegularSnapshot>, StateWorkerError> {
        lock(&self.captures).push((std::time::Instant::now(), request.context.api_cadence));
        Ok(
            RawRegularSnapshotBuilder::new(request.context, Ok(ReadStatus::Ready(raw_global())))
                .publish(),
        )
    }
}

/// Transition that only counts regular applications and records precise sequences.
pub struct RecordingTransition {
    pub context: ReadContext,
    pub regular: Arc<std::sync::atomic::AtomicUsize>,
    pub precise_sequences: Arc<Mutex<Vec<u64>>>,
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
                next_capture: crate::RegularCapturePlan::default(),
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

/// Publisher that records every delivered publication.
pub struct DeliveryRecorder(pub Arc<Mutex<Vec<(String, Delivery)>>>);

impl SnapshotPublisher<String> for DeliveryRecorder {
    fn publish(&self, publication: String, delivery: Delivery) -> Result<(), StateWorkerError> {
        lock(&self.0).push((publication, delivery));
        Ok(())
    }
}

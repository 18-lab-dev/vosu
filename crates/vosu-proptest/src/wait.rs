//! Wall-clock bounds for test threads and rendezvous. These bounds also
//! work outside a Tokio runtime and when its virtual clock is paused.
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

const LIMIT: Duration = Duration::from_secs(10);

/// A reusable test barrier which fails instead of hanging on a missing peer.
pub struct Barrier {
    parties: usize,
    state: Mutex<(usize, usize, bool)>,
    changed: Condvar,
}

impl Barrier {
    pub fn new(parties: usize) -> Self {
        assert!(parties > 0);
        Self {
            parties,
            state: Mutex::new((0, 0, false)),
            changed: Condvar::new(),
        }
    }

    #[track_caller]
    pub fn wait(&self) {
        self.wait_within(LIMIT);
    }

    #[track_caller]
    fn wait_within(&self, limit: Duration) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        assert!(!state.2, "test barrier already timed out");
        let generation = state.0;
        state.1 += 1;
        if state.1 == self.parties {
            state.0 += 1;
            state.1 = 0;
            self.changed.notify_all();
            return;
        }
        let (mut state, _) = self
            .changed
            .wait_timeout_while(state, limit, |state| state.0 == generation && !state.2)
            .unwrap_or_else(|p| p.into_inner());
        if state.0 == generation {
            state.2 = true;
            self.changed.notify_all();
            panic!(
                "test barrier did not receive all {} participants within {limit:?}",
                self.parties
            );
        }
    }
}

/// Retains the normal join result, but diagnoses a stalled test worker first.
pub trait JoinBounded {
    type Output;
    #[track_caller]
    fn join_bounded(self) -> std::thread::Result<Self::Output>;
}

fn wait_finished(is_finished: impl Fn() -> bool, limit: Duration) {
    let deadline = Instant::now() + limit;
    while !is_finished() {
        assert!(
            Instant::now() < deadline,
            "test worker did not finish within {limit:?}"
        );
        std::thread::park_timeout(Duration::from_millis(1));
    }
}

impl<T> JoinBounded for std::thread::JoinHandle<T> {
    type Output = T;
    fn join_bounded(self) -> std::thread::Result<T> {
        wait_finished(|| self.is_finished(), LIMIT);
        self.join()
    }
}

impl<T> JoinBounded for std::thread::ScopedJoinHandle<'_, T> {
    type Output = T;
    fn join_bounded(self) -> std::thread::Result<T> {
        wait_finished(|| self.is_finished(), LIMIT);
        self.join()
    }
}

/// Reaps a test child without letting a missing exit stall the suite.
pub fn child(child: &mut std::process::Child) -> std::io::Result<std::process::ExitStatus> {
    let deadline = Instant::now() + LIMIT;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("test child {} did not exit within {LIMIT:?}", child.id()),
            ));
        }
        std::thread::park_timeout(Duration::from_millis(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn barrier_can_be_reused() {
        let barrier = Barrier::new(2);
        std::thread::scope(|scope| {
            let peer = scope.spawn(|| {
                for _ in 0..3 {
                    barrier.wait();
                }
            });
            for _ in 0..3 {
                barrier.wait();
            }
            peer.join_bounded().unwrap();
        });
    }

    #[test]
    #[should_panic(expected = "test barrier did not receive all 2 participants")]
    fn missing_barrier_peer_fails() {
        Barrier::new(2).wait_within(Duration::from_millis(1));
    }

    #[test]
    fn worker_panic_is_preserved() {
        assert!(
            std::thread::spawn(|| panic!("worker assertion"))
                .join_bounded()
                .is_err()
        );
    }

    #[test]
    #[should_panic(expected = "test worker did not finish")]
    fn unfinished_worker_fails() {
        wait_finished(|| false, Duration::from_millis(1));
    }

    #[tokio::test(start_paused = true)]
    #[should_panic(expected = "test future timed out: std::future::pending::<()>()")]
    async fn pending_future_fails_with_operation_name() {
        crate::bounded!(std::future::pending::<()>());
    }

    #[tokio::test]
    async fn ready_future_preserves_its_value() {
        assert_eq!(crate::bounded!(async { 42 }), 42);
    }
}

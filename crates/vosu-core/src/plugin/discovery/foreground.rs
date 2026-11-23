//! Windows foreground-window poller (`docs/v2/implementation-plan.md` M1
//! §4, phase d2b, unit c): the OS exposes no foreground-change
//! *notification* vosu can subscribe to (unlike process appear/exit,
//! which eBPF/CN_PROC push on Linux), only a point-in-time query
//! (`vosu_sys::process::foreground_process_id`,
//! `GetForegroundWindow` + `GetWindowThreadProcessId`), so this polls it
//! on its own dedicated thread and calls
//! [`super::host::PluginHost::foreground_changed`] only when the owning
//! pid actually changes — that call takes no process id itself (it
//! re-reads the live foreground owner through
//! [`super::super::focus::OsForegroundSource`] when it recomputes focus,
//! `host/lifecycle.rs`'s own doc comment); this poller's only job is
//! noticing *when* to trigger that recompute.
//!
//! # Cadence
//! [`FOREGROUND_POLL_INTERVAL`] is 100 ms, reusing 1.x's own
//! `WINDOWS_FOCUS_INTERVAL` (`src/instance.rs`) exactly rather than
//! picking a new number: that constant was already tuned against a real
//! osu! session for "indistinguishable from instant" alt-tab
//! responsiveness without measurably taxing a foreground CPU core, and
//! nothing about the generic core host changes that trade-off. It sits
//! at the fast end of the 100-250 ms range this unit's assignment allows
//! for a cadence with no 1.x precedent, which this one has.
//!
//! # Threading
//! One dedicated `std::thread`, never a Tokio worker (`AGENTS.md`: "No
//! blocking sleep or wait on a Tokio worker"), the same split
//! [`super::DiscoveryService`] already uses: the loop body's one
//! syscall (`vosu_sys::process::foreground_process_id`) is synchronous,
//! and the one asynchronous wait — this service's own wake-on-stop
//! [`tokio::sync::Notify`] raced against the
//! [`FOREGROUND_POLL_INTERVAL`] timeout — runs through
//! `Handle::block_on`, never a worker. `tokio::time::sleep` under
//! `block_on` (not `std::thread::sleep`) keeps this off the
//! disallowed-methods lint's `scripts/lint-expectations.txt` list it
//! would otherwise need an entry for. [`ForegroundPoller`] holds only a
//! [`std::sync::Weak`] reference to the host (`AGENTS.md`: background
//! tasks hold `Weak`) and never joins its own thread: `stop`/`Drop` set
//! a cancel flag, wake the `Notify`, and join from the *caller's*
//! thread, bounded by at most one in-flight wait.

use std::time::Duration;

/// Polling cadence (see module doc "Cadence").
pub const FOREGROUND_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Pure foreground-owner change detector: whether `current` differs from
/// `previous`, including a transition to or from "no foreground window"
/// (`None`) — either direction is a real change the Focus rule must
/// re-run (`docs/v2/api-v1.md` "Focus rule": losing a tracked foreground
/// owner falls through to the most-recently-focused session, exactly
/// like gaining one promotes it, so both edges matter). Used by both
/// [`ForegroundPoller`]'s `#[cfg(windows)]` polling loop and this
/// module's own `tests` on every target (`#[cfg(any(windows, test))]`):
/// otherwise a non-Windows, non-test build has no caller at all, which
/// `-D warnings`' `dead_code` lint correctly rejects.
#[cfg(any(windows, test))]
#[must_use]
pub(crate) fn foreground_owner_changed(previous: Option<u32>, current: Option<u32>) -> bool {
    previous != current
}

#[cfg(windows)]
mod backend {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Weak};
    use std::thread::{self, JoinHandle};

    use tokio::runtime::Handle;
    use tokio::sync::Notify;

    use super::super::super::host::PluginHost;
    use super::{FOREGROUND_POLL_INTERVAL, foreground_owner_changed};

    /// Owns the foreground-poller background thread. [`Self::start`]
    /// unconditionally starts it; like [`super::super::DiscoveryService`],
    /// whether to call it at all is the caller's own decision (production
    /// runtime wiring, a later unit).
    ///
    /// # Natively unverified
    /// `#[cfg(windows)]`, compile-checked only through `cargo xwin`
    /// (`AGENTS.md`: the Windows VM is down, no native run is possible
    /// here) — unverified natively: every
    /// `vosu_sys::process::foreground_process_id` call below (and
    /// transitively `GetForegroundWindow`/`GetWindowThreadProcessId`),
    /// and the real alt-tab latency this cadence produces on a live
    /// desktop.
    pub struct ForegroundPoller {
        cancel: Arc<AtomicBool>,
        wake: Arc<Notify>,
        thread: Option<JoinHandle<()>>,
    }

    impl ForegroundPoller {
        /// Starts the background poller, calling
        /// [`PluginHost::foreground_changed`] on `host` whenever the
        /// foreground-owning pid changes, until [`Self::stop`] is called
        /// (or this value drops, which stops it the same way). `runtime`
        /// drives the one `block_on` wait between polls, the same role
        /// [`super::super::DiscoveryService::start`]'s own `runtime`
        /// parameter plays.
        #[must_use]
        pub fn start(host: &Arc<PluginHost>, runtime: Handle) -> Self {
            let cancel = Arc::new(AtomicBool::new(false));
            let wake = Arc::new(Notify::new());
            let loop_host: Weak<PluginHost> = Arc::downgrade(host);
            let loop_cancel = Arc::clone(&cancel);
            let loop_wake = Arc::clone(&wake);
            let thread = thread::Builder::new()
                .name("vosu-foreground".to_owned())
                .spawn(move || {
                    let mut last: Option<u32> = None;
                    while !loop_cancel.load(Ordering::Acquire) {
                        let Some(host) = loop_host.upgrade() else {
                            break;
                        };
                        let current = vosu_sys::process::foreground_process_id();
                        if foreground_owner_changed(last, current) {
                            host.foreground_changed();
                            last = current;
                        }
                        drop(host);
                        if loop_cancel.load(Ordering::Acquire) {
                            break;
                        }
                        runtime.block_on(async {
                            tokio::select! {
                                () = loop_wake.notified() => {}
                                () = tokio::time::sleep(FOREGROUND_POLL_INTERVAL) => {}
                            }
                        });
                    }
                })
                .expect("spawning the foreground poller thread must not fail");
            Self {
                cancel,
                wake,
                thread: Some(thread),
            }
        }

        /// Stops the background thread and joins it (bounded: the loop
        /// body never blocks longer than one poll's worth of work plus
        /// the wake-or-timeout wait above). Equivalent to dropping this
        /// value; both exist so a caller that wants to observe the join
        /// (orderly runtime shutdown, alongside
        /// [`super::super::DiscoveryService::stop`]) can call it
        /// explicitly.
        pub fn stop(mut self) {
            self.stop_inner();
        }

        fn stop_inner(&mut self) {
            self.cancel.store(true, Ordering::Release);
            // `notify_one` (not `notify_waiters`): see
            // `DiscoveryService::stop_inner`'s own comment -- a wake
            // between this store and the loop's next wait stores a
            // permit that wait consumes immediately, instead of being
            // lost (P3: `stop` must not stall until
            // `FOREGROUND_POLL_INTERVAL`'s own timeout).
            self.wake.notify_one();
            if let Some(thread) = self.thread.take()
                && thread.join().is_err()
            {
                tracing::warn!("foreground poller thread panicked during shutdown");
            }
        }
    }

    impl Drop for ForegroundPoller {
        fn drop(&mut self) {
            self.stop_inner();
        }
    }
}

#[cfg(windows)]
pub use backend::ForegroundPoller;

#[cfg(test)]
mod tests {
    use super::foreground_owner_changed;

    #[test]
    fn same_owner_is_not_a_change() {
        assert!(!foreground_owner_changed(Some(42), Some(42)));
        assert!(!foreground_owner_changed(None, None));
    }

    #[test]
    fn a_different_owner_is_a_change() {
        assert!(foreground_owner_changed(Some(42), Some(43)));
    }

    #[test]
    fn gaining_a_foreground_owner_is_a_change() {
        assert!(foreground_owner_changed(None, Some(42)));
    }

    #[test]
    fn losing_the_foreground_owner_is_a_change() {
        assert!(foreground_owner_changed(Some(42), None));
    }
}

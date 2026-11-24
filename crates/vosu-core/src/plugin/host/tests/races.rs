//! The two P2 shutdown/timer races: a `TIMER_SET` made during `create`
//! queued and replayed once `create` returns, and `destroy_all` not
//! leaking an instance still racing `create_instance`.
//! Split out of `tests.rs` so that module stays under the maintained-file
//! line limit (`docs/architecture/source-map.md`).

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::plugin::error::HostError;
use crate::plugin::host::*;
use crate::plugin::registry;

use super::helpers::test_host;

/// Blocks the gated plugin's own `create` until released, so a test
/// can force a `TIMER_SET` it made from inside `create` to fire —
/// and be queued, not delivered or lost — strictly before `create`
/// itself returns: an ordering this gate decides, never how fast
/// `create` happens to run on whatever machine runs this test.
struct CreateGate {
    release: Mutex<bool>,
    release_cv: Condvar,
    delivered: Mutex<bool>,
    delivered_cv: Condvar,
}

impl CreateGate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            release: Mutex::new(false),
            release_cv: Condvar::new(),
            delivered: Mutex::new(false),
            delivered_cv: Condvar::new(),
        })
    }

    fn wait_for_release(&self) {
        let mut release = self.release.lock().unwrap_or_else(|p| p.into_inner());
        while !*release {
            release = self
                .release_cv
                .wait(release)
                .unwrap_or_else(|p| p.into_inner());
        }
    }

    fn release(&self) {
        *self.release.lock().unwrap_or_else(|p| p.into_inner()) = true;
        self.release_cv.notify_all();
    }

    fn mark_delivered(&self) {
        *self.delivered.lock().unwrap_or_else(|p| p.into_inner()) = true;
        self.delivered_cv.notify_all();
    }

    fn wait_for_delivery(&self, timeout: Duration) -> bool {
        let (guard, result) = self
            .delivered_cv
            .wait_timeout_while(
                self.delivered.lock().unwrap_or_else(|p| p.into_inner()),
                timeout,
                |delivered| !*delivered,
            )
            .unwrap_or_else(|p| p.into_inner());
        drop(guard);
        !result.timed_out()
    }
}

fn create_gate_registry() -> &'static Mutex<HashMap<String, Arc<CreateGate>>> {
    static REGISTRY: std::sync::LazyLock<Mutex<HashMap<String, Arc<CreateGate>>>> =
        std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
    &REGISTRY
}

/// A plugin whose own `create` sets a one-shot timer (§6.11) and then
/// blocks on this test's [`CreateGate`] before returning, so the test
/// can hold `create` open across the real timer firing. `on_message`
/// marks the gate once it actually receives the replayed
/// `TIMER_FIRED` — the end-to-end proof this finding's fix requires,
/// not merely that something got queued.
mod gated_timer_plugin {
    use std::sync::Arc;

    use vosu_plugin_api::{
        HostServices, Init, Message, Plugin, PluginError, TimerFired, TimerSet, VtableHostServices,
        vosu_status,
    };

    use super::{CreateGate, create_gate_registry};

    pub struct GatedTimerPlugin {
        gate: Arc<CreateGate>,
    }

    impl Plugin for GatedTimerPlugin {
        fn create(host: VtableHostServices, init: Init) -> Result<Self, PluginError> {
            let status = host.send(
                &Message::TimerSet(TimerSet {
                    timer: 1,
                    interval_ms: None,
                }),
                0,
            );
            if status != vosu_status::VOSU_OK {
                return Err(status.into());
            }
            let gate = create_gate_registry()
                .lock()
                .unwrap()
                .get(&init.plugin_id)
                .cloned()
                .unwrap_or_else(|| panic!("no create gate registered for {:?}", init.plugin_id));
            gate.wait_for_release();
            Ok(Self { gate })
        }

        fn on_message(
            &mut self,
            message: Message,
            _correlation_id: u64,
        ) -> Result<(), PluginError> {
            if let Message::TimerFired(TimerFired { timer: 1 }) = message {
                self.gate.mark_delivered();
            }
            Ok(())
        }
    }

    vosu_plugin_api::export_plugin!(bundled GatedTimerPlugin);
}

/// The new-finding P2 race, closed: a `TIMER_SET` the plugin makes
/// from inside its own `create` fires, and is handed to
/// `PluginHost::timer_fired`, while `create_instance` has not yet
/// replaced the `Creating` placeholder with the real instance — this
/// test gates `create` open until it observes exactly that (the
/// fire queued in `InstanceSlot::Creating`'s own `pending` list, a
/// real internal state transition this test waits on, never a fixed
/// sleep guessing the two have raced) — and then proves the queued
/// fire still reaches the plugin once `create` finally returns,
/// rather than being discarded for good.
#[test]
fn a_timer_set_during_create_that_fires_before_registration_is_queued_and_replayed() {
    let plugin_id = "acme.gated-timer";
    let gate = CreateGate::new();
    create_gate_registry()
        .lock()
        .unwrap()
        .insert(plugin_id.to_owned(), Arc::clone(&gate));

    let manifest_text = format!(
        "kind='plugin'\nmanifest_version=1\nid='{plugin_id}'\nname='Gated Timer'\nversion='0.1.0'\nhost='>=1.0.0, <2.0.0'\nabi_version=1\nruntime='in-process'\n"
    );
    let loaded = registry::load_bundled(&[registry::BundledPlugin {
        manifest: &manifest_text,
        vtable: gated_timer_plugin::vosu_plugin_vtable,
    }])
    .expect("loading the gated-timer test plugin");

    let runtime = tokio::runtime::Runtime::new().expect("building a Tokio runtime");
    let _enter = runtime.enter();
    let (_log_dir, host) = test_host(runtime.handle().clone());
    let data_dir = tempfile::tempdir().unwrap();
    let id = PluginInstanceId::new(1);

    std::thread::scope(|scope| {
        let host_ref = &host;
        let loaded_ref = &loaded[0];
        let data_dir_path = data_dir.path().to_path_buf();
        let create_thread =
            scope.spawn(move || host_ref.create_instance(id, loaded_ref, data_dir_path, None));

        // Wait for the real one-shot timer (fires after
        // `timers::MIN_TIMER_GRANULARITY`) to have already reached
        // `timer_fired` and queued itself against the still-`Creating`
        // slot — proof the race actually happened before
        // `create_instance` got anywhere near installing the real
        // instance, not an assumption resting on scheduling luck.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let queued = {
                let guard = host.instances.lock().unwrap_or_else(|p| p.into_inner());
                matches!(
                    guard.get(&id),
                    Some(InstanceSlot::Creating { pending, .. }) if !pending.is_empty()
                )
            };
            if queued {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "expected the timer to fire and queue against the Creating placeholder within 5s"
            );
            std::thread::sleep(Duration::from_millis(1));
        }

        gate.release();
        create_thread
            .join()
            .unwrap()
            .expect("creating the gated-timer instance");
    });

    assert!(
        gate.wait_for_delivery(Duration::from_secs(5)),
        "the timer fire queued during create must still reach the plugin once replayed"
    );

    host.destroy_all(Duration::from_secs(5));
}

/// A plugin whose own `create` blocks on this test's [`CreateGate`]
/// (reused from the timer-race fixture above) until released, and
/// whose `Drop` records that it was actually destroyed — the proof
/// `destroy_all_does_not_leak_an_instance_still_creating_when_it_races`
/// needs that losing the shutdown race tears the instance down
/// instead of merely refusing to publish it.
mod race_gated_plugin {
    use std::sync::atomic::{AtomicBool, Ordering};

    use vosu_plugin_api::{Init, Message, Plugin, PluginError, VtableHostServices};

    use super::create_gate_registry;

    pub static DESTROYED: AtomicBool = AtomicBool::new(false);

    pub struct RaceGatedPlugin;

    impl Plugin for RaceGatedPlugin {
        fn create(_host: VtableHostServices, init: Init) -> Result<Self, PluginError> {
            let gate = create_gate_registry()
                .lock()
                .unwrap()
                .get(&init.plugin_id)
                .cloned()
                .unwrap_or_else(|| panic!("no create gate registered for {:?}", init.plugin_id));
            gate.wait_for_release();
            Ok(Self)
        }

        fn on_message(
            &mut self,
            _message: Message,
            _correlation_id: u64,
        ) -> Result<(), PluginError> {
            Ok(())
        }
    }

    impl Drop for RaceGatedPlugin {
        fn drop(&mut self) {
            DESTROYED.store(true, Ordering::SeqCst);
        }
    }

    vosu_plugin_api::export_plugin!(bundled RaceGatedPlugin);
}

/// The race this finding's fix closes: `destroy_all` must not drop a
/// still-`Creating` placeholder and declare itself done while
/// `create_instance`'s own thread is still inside the plugin's
/// `create` — otherwise that call's later swap installs a `Live`
/// entry `destroy_all` has already finished draining, leaking a
/// running instance forever. Proves both ends: `create_instance`
/// loses the race with `HostError::ShuttingDown`, and the instance it
/// built is actually destroyed (not merely unpublished), with no
/// `Live` entry left behind afterward.
#[test]
fn destroy_all_does_not_leak_an_instance_still_creating_when_it_races() {
    let plugin_id = "acme.race-gated";
    let gate = CreateGate::new();
    create_gate_registry()
        .lock()
        .unwrap()
        .insert(plugin_id.to_owned(), Arc::clone(&gate));
    race_gated_plugin::DESTROYED.store(false, Ordering::SeqCst);

    let manifest_text = format!(
        "kind='plugin'\nmanifest_version=1\nid='{plugin_id}'\nname='Race Gated'\nversion='0.1.0'\nhost='>=1.0.0, <2.0.0'\nabi_version=1\nruntime='in-process'\n"
    );
    let loaded = registry::load_bundled(&[registry::BundledPlugin {
        manifest: &manifest_text,
        vtable: race_gated_plugin::vosu_plugin_vtable,
    }])
    .expect("loading the race-gated test plugin");

    let runtime = tokio::runtime::Runtime::new().expect("building a Tokio runtime");
    let _enter = runtime.enter();
    let (_log_dir, host) = test_host(runtime.handle().clone());
    let data_dir = tempfile::tempdir().unwrap();
    let id = PluginInstanceId::new(1);

    std::thread::scope(|scope| {
        let host_ref = &host;
        let loaded_ref = &loaded[0];
        let data_dir_path = data_dir.path().to_path_buf();
        let create_thread =
            scope.spawn(move || host_ref.create_instance(id, loaded_ref, data_dir_path, None));

        // Wait for `create_instance` to register the `Creating`
        // placeholder: the plugin's own `create` is now blocked on
        // the gate.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let creating = matches!(
                host.instances
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .get(&id),
                Some(InstanceSlot::Creating { .. })
            );
            if creating {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "expected the Creating placeholder to appear within 5s"
            );
            std::thread::sleep(Duration::from_millis(1));
        }

        let destroy_thread = scope.spawn(|| host_ref.destroy_all(Duration::from_secs(5)));

        // Wait for `destroy_all` to have actually set `shutting_down`
        // before releasing `create`, so this test exercises the exact
        // race window the fix closes rather than letting `create`
        // finish first by scheduling luck.
        let deadline = Instant::now() + Duration::from_secs(5);
        while !host.shutting_down.load(Ordering::Acquire) {
            assert!(
                Instant::now() < deadline,
                "expected destroy_all to observe the race within 5s"
            );
            std::thread::sleep(Duration::from_millis(1));
        }

        gate.release();

        let create_result = create_thread.join().unwrap();
        destroy_thread.join().unwrap();

        assert!(
            matches!(create_result, Err(HostError::ShuttingDown)),
            "expected create_instance to lose the shutdown race, got {create_result:?}"
        );
    });

    assert!(
        race_gated_plugin::DESTROYED.load(Ordering::SeqCst),
        "the instance created during the shutdown race must still be destroyed, not leaked"
    );
    assert!(
        host.instances
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_empty(),
        "destroy_all must not leave a Live entry behind after racing create_instance"
    );
}

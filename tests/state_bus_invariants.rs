use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

struct TrackingAllocator;

thread_local! {
    static TRACK_ALLOCATIONS: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

// SAFETY: every method below forwards unchanged to `std::alloc::System`, a
// valid `GlobalAlloc` implementation; this wrapper only adds allocation
// counting around the delegated calls.
unsafe impl GlobalAlloc for TrackingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        TRACK_ALLOCATIONS.with(|enabled| {
            if enabled.get() {
                ALLOCATIONS.with(|count| count.set(count.get() + 1));
            }
        });
        // SAFETY: forwards `layout` unchanged to `System::alloc`.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        TRACK_ALLOCATIONS.with(|enabled| {
            if enabled.get() {
                ALLOCATIONS.with(|count| count.set(count.get() + 1));
            }
        });
        // SAFETY: forwards `layout` unchanged to `System::alloc_zeroed`.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        TRACK_ALLOCATIONS.with(|enabled| {
            if enabled.get() {
                ALLOCATIONS.with(|count| count.set(count.get() + 1));
            }
        });
        // SAFETY: forwards `ptr`, `layout`, and `new_size` unchanged to
        // `System::realloc`, which has the same safety contract as this
        // method.
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: forwards `ptr` and `layout` unchanged to `System::dealloc`.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL_ALLOCATOR: TrackingAllocator = TrackingAllocator;

fn allocations_during<T>(operation: impl FnOnce() -> T) -> (T, usize) {
    ALLOCATIONS.with(|count| count.set(0));
    TRACK_ALLOCATIONS.with(|enabled| enabled.set(true));
    let result = operation();
    TRACK_ALLOCATIONS.with(|enabled| enabled.set(false));
    let allocations = ALLOCATIONS.with(Cell::get);
    (result, allocations)
}

#[test]
fn unchanged_snapshot_publishes_and_allocates_nothing() {
    use std::sync::atomic::AtomicBool;
    use vosu_osu::{project::SessionFacts, publish::BusPublisher};
    use vosu_osu_model::{enums::ClientType, launch::InstanceSettings};
    use vosu_osu_state::{Delivery, SnapshotPublisher, StateBundle};
    use vosu_plugin_api::{HostServices, Message, vosu_opcode, vosu_status};
    struct Counter(Arc<AtomicUsize>);
    impl HostServices for Counter {
        fn send(&self, _: &Message, _: u64) -> vosu_status {
            vosu_status::VOSU_OK
        }
        fn send_flat(&self, _: vosu_opcode, _: u32, _: u64, _: &[u8]) -> vosu_status {
            self.0.fetch_add(1, Ordering::Relaxed);
            vosu_status::VOSU_OK
        }
    }
    let sends = Arc::new(AtomicUsize::new(0));
    let publisher = BusPublisher::new(
        Counter(sends.clone()),
        8203,
        Arc::new(AtomicBool::new(true)),
        SessionFacts {
            client: ClientType::Stable,
            settings: InstanceSettings {
                tournament_spectator: false,
                tournament_ipc_id: None,
                devserver_endpoint: None,
            },
            foreground: None,
        },
    );
    let snapshot = StateBundle::new(
        ClientType::Stable,
        1,
        Arc::new(vosu_osu_state::state_backend::LiveStateCalculationBackend::new()),
    )
    .snapshot();
    publisher.publish(snapshot.clone(), Delivery::All).unwrap();
    assert_eq!(
        sends.load(Ordering::Relaxed),
        1,
        "the publisher must be active, not unidentified"
    );
    let (_, allocations) = allocations_during(|| {
        for _ in 0..100 {
            publisher.publish(snapshot.clone(), Delivery::All).unwrap();
        }
    });
    assert_eq!(allocations, 0);
    assert_eq!(sends.load(Ordering::Relaxed), 1);
    let mut changed = (*snapshot).clone();
    let mut precise = (*changed.precise).clone();
    precise.play_time = 1234.0;
    changed.precise = Arc::new(precise);
    let changed = Arc::new(changed);
    publisher
        .publish(changed.clone(), Delivery::Precise)
        .unwrap();
    assert_eq!(
        sends.load(Ordering::Relaxed),
        2,
        "a changed precise input must publish"
    );
    // Both lanes and the output buffers have been warmed. Changing the precise
    // lane must not allocate to remember regular groups or build dirty flags.
    publisher
        .publish(snapshot.clone(), Delivery::Precise)
        .unwrap();
    publisher
        .publish(changed.clone(), Delivery::Precise)
        .unwrap();
    let before = sends.load(Ordering::Relaxed);
    let (_, allocations) = allocations_during(|| {
        for _ in 0..100 {
            publisher
                .publish(snapshot.clone(), Delivery::Precise)
                .unwrap();
            publisher
                .publish(changed.clone(), Delivery::Precise)
                .unwrap();
        }
    });
    assert_eq!(allocations, 0);
    assert_eq!(sends.load(Ordering::Relaxed), before + 200);
}

#[test]
fn steady_state_scalar_commit_allocations_bounded() {
    use std::sync::RwLock;
    use vosu_core::state::{
        ClientFacts, Pointer, PublisherGrant, SchemaRegistry, SessionKey, StateStore, StateValue,
    };
    use vosu_plugin_api::{PublishFieldRef, PublishValueRef};
    let grant = PublisherGrant::from_manifest(
        &vosu_plugin_api::manifest::PackageManifest::parse(include_str!(
            "../crates/vosu-osu/manifest.toml"
        ))
        .and_then(vosu_plugin_api::manifest::PackageManifest::into_plugin)
        .unwrap(),
    )
    .unwrap();
    let store = StateStore::new(Arc::new(RwLock::new(SchemaRegistry::with_core().unwrap())));
    let key = SessionKey(8204);
    store.open_session(
        key,
        &grant,
        ClientFacts {
            game: "osu".into(),
            variant: "stable".into(),
            name: "bench".into(),
            version: "1".into(),
            pid: 8204,
            session: "invariant".into(),
            plugin_id: "vosu.osu".into(),
            plugin_version: "2.0.0-alpha.0".into(),
        },
        true,
    );
    let commit = |n| {
        store
            .commit(
                &grant,
                key,
                [PublishFieldRef {
                    handle: key.0,
                    ns: "vosu.osu",
                    pointer: "/bench/scalar",
                    value: PublishValueRef::I64(n),
                }]
                .into_iter(),
            )
            .unwrap()
    };
    drop(commit(0));
    let mut maximum = 0;
    for n in 1..=100 {
        let (outcome, allocations) = allocations_during(|| commit(n));
        maximum = maximum.max(allocations);
        assert_eq!(outcome.current.revision, n as u64 + 1);
        assert_eq!(
            outcome
                .current
                .get("vosu.osu", &Pointer::parse("/bench/scalar").unwrap()),
            Some(&StateValue::I64(n))
        );
    }
    // A fixed-size scalar update must not grow with session revision/history.
    // This ceiling covers validation, persistent path copies and commit metadata.
    assert!(maximum <= 64, "scalar commit allocated {maximum} times");
}

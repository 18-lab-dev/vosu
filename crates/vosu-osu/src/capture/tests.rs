use super::*;
use crate::publish::FanoutPublisher;
use std::sync::atomic::{AtomicUsize, Ordering};
pub(super) fn state_bundle(generation: u64, reset_epoch: u64) -> Arc<StateBundleSnapshot> {
    use vosu_osu_model::states::HitErrorArray;
    use vosu_osu_state::{
        BassDensityState, BeatmapPPState, GameplayState, GlobalState, LazerMultiSpectatingState,
        MenuState, PreciseState, RankedPlayState, RegularState, ResultScreenState, SettingsState,
        TourneyManagerState, UserState,
    };

    Arc::new(StateBundleSnapshot {
        source: ClientType::Stable,
        generation,
        reset_epoch,
        regular: Arc::new(RegularState {
            global: GlobalState::default(),
            settings: SettingsState::default(),
            beatmap: BeatmapPPState::default(),
            menu: MenuState::default(),
            bass_density: BassDensityState::default(),
            gameplay: GameplayState::default(),
            result_screen: ResultScreenState::default(),
            tourney: TourneyManagerState::default(),
            user: UserState::default(),
            lazer_multi_spectating: LazerMultiSpectatingState::default(),
            ranked_play: RankedPlayState::default(),
            room: Default::default(),
        }),
        precise: Arc::new(PreciseState {
            catcher: None,
            status: 0,
            play_time: 0.0,
            mode: 0,
            key_overlay: Arc::new(Vec::new()),
            keypresses: Arc::new(Vec::new()),
            hit_errors: Arc::new(HitErrorArray::default()),
            total_hit_errors: 0.0,
            committed_hit_error_index: 0,
            unstable_rate: 0.0,
        }),
    })
}

#[test]
fn selection_resource_changes_bypass_both_live_and_precise_api_cadences() {
    let previous = state_bundle(1, 10);
    for field in 0..6 {
        let mut next = (*previous).clone();
        let regular = Arc::make_mut(&mut next.regular);
        match field {
            0 => regular.menu.checksum = Some("new checksum".into()),
            1 => regular.menu.filename = Some("new file".into()),
            2 => regular.menu.folder = Some("new folder".into()),
            3 => regular.menu.audio_filename = Some("new audio".into()),
            4 => regular.menu.background_filename = Some("new background".into()),
            5 => regular.global.songs_folder = "new songs root".into(),
            _ => unreachable!(),
        }
        let next = Arc::new(next);
        for delivery in [Delivery::Live, Delivery::Precise] {
            assert!(Arc::ptr_eq(
                &api_state(Some(&previous), &next, delivery),
                &next
            ));
        }
    }
    let mut ordinary = (*previous).clone();
    Arc::make_mut(&mut ordinary.regular).menu.title = Some("new title only".into());
    assert!(Arc::ptr_eq(
        &api_state(Some(&previous), &Arc::new(ordinary), Delivery::Live),
        &previous
    ));
}

#[test]
fn memory_permission_denial_keeps_typed_identity_in_state_worker_errors() {
    let denied = StateWorkerError::from(vosu_osu_reader::process::MemoryError::PermissionDenied {
        address: 0x1234,
        len: 16,
        message: "operation not permitted".into(),
    });
    assert!(matches!(
        denied,
        StateWorkerError::MemoryPermissionDenied {
            address: 0x1234,
            len: 16,
            ..
        }
    ));

    let fault = StateWorkerError::from(vosu_osu_reader::process::MemoryError::Read {
        address: 0x1234,
        len: 16,
        message: "bad address".into(),
    });
    assert!(matches!(fault, StateWorkerError::Capture(_)));
}

#[tokio::test]
async fn permission_denial_logs_once_and_stops_initialization_without_retrying() {
    let cancel = CancellationToken::new();
    let attempts = AtomicUsize::new(0);
    let errors = AtomicUsize::new(0);

    let worker = tokio::time::timeout(
        Duration::from_millis(50),
        initialize_state_worker::<(), _, _, _>(
            &cancel,
            Duration::from_secs(60),
            || {
                attempts.fetch_add(1, Ordering::SeqCst);
                std::future::ready(Err(StateWorkerError::MemoryPermissionDenied {
                    address: 0x4000,
                    len: 8,
                    message: "permission denied".into(),
                }))
            },
            |_| {
                errors.fetch_add(1, Ordering::SeqCst);
            },
        ),
    )
    .await
    .expect("permission denial must terminate immediately");

    assert!(worker.is_none());
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
    assert_eq!(errors.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn ordinary_initialization_errors_keep_retrying_until_cancelled() {
    let cancel = CancellationToken::new();
    let attempts = AtomicUsize::new(0);
    let errors = AtomicUsize::new(0);

    let worker = initialize_state_worker::<(), _, _, _>(
        &cancel,
        Duration::ZERO,
        || {
            let attempt = attempts.fetch_add(1, Ordering::SeqCst) + 1;
            if attempt == 2 {
                cancel.cancel();
            }
            std::future::ready(Err(StateWorkerError::Capture("transient".into())))
        },
        |_| {
            errors.fetch_add(1, Ordering::SeqCst);
        },
    )
    .await;

    assert!(worker.is_none());
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    assert_eq!(errors.load(Ordering::SeqCst), 0);
}

#[test]
fn fanout_sends_b1_before_bus_and_bus_refusal_never_fails_b1() {
    use crate::{
        bridge_b1::*,
        project::SessionFacts,
        publish::{BusPublisher, SessionHost},
    };
    use vosu_plugin_api::{HostServices, Message, vosu_opcode, vosu_status};
    struct Host(Arc<Mutex<Vec<&'static str>>>, vosu_status);
    impl HostServices for Host {
        fn send(&self, _: &Message, _: u64) -> vosu_status {
            vosu_status::VOSU_OK
        }
        fn send_flat(&self, _: vosu_opcode, _: u32, _: u64, _: &[u8]) -> vosu_status {
            self.0.lock().unwrap().push("bus");
            self.1
        }
    }
    struct Sink(Arc<Mutex<Vec<&'static str>>>);
    impl vosu_surface::LegacySink<B1Event, B1Inputs> for Sink {
        fn inputs(&self) -> watch::Receiver<B1Inputs> {
            watch::channel(B1Inputs::default()).1
        }
        fn send(&self, event: B1Event) {
            if matches!(event, B1Event::Publish { .. }) {
                self.0.lock().unwrap().push("b1");
            }
        }
    }
    for status in [
        vosu_status::VOSU_OK,
        vosu_status::VOSU_ERR_PERMISSION_DENIED,
    ] {
        let order = Arc::new(Mutex::new(Vec::new()));
        let publisher = FanoutPublisher {
            b1: Some(B1Publisher::new(
                B1SessionKey {
                    pid: 61104,
                    generation: 1,
                },
                Arc::new(Sink(order.clone())),
            )),
            bus: Arc::new(BusPublisher::new(
                SessionHost(Arc::new(Host(order.clone(), status))),
                61104,
                Arc::new(std::sync::atomic::AtomicBool::new(true)),
                SessionFacts {
                    client: ClientType::Stable,
                    settings: InstanceSettings {
                        tournament_spectator: false,
                        tournament_ipc_id: None,
                        devserver_endpoint: None,
                    },
                    foreground: None,
                },
            )),
        };
        assert!(publisher.publish(state_bundle(1, 0), Delivery::All).is_ok());
        assert_eq!(*order.lock().unwrap(), ["b1", "bus"]);
    }
}

#![expect(
    clippy::disallowed_methods,
    reason = "isolated temporary archive fixtures"
)]
use super::*;
use std::{io::Write, sync::mpsc};
use vosu_plugin_api::{ActionCancel, ActionInvoke, ActionStatus, Caller, CallerKind};
use zip::write::SimpleFileOptions;
const WAIT: Duration = Duration::from_secs(5);
type PreparationGate = Option<(mpsc::SyncSender<()>, mpsc::Receiver<()>)>;
#[derive(Clone)]
struct Host {
    messages: mpsc::SyncSender<Message>,
    gate: Arc<Mutex<PreparationGate>>,
}
impl HostServices for Host {
    fn send(&self, message: &Message, _: u64) -> vosu_status {
        if matches!(message, Message::ActionProgress(p) if p.fraction == Some(0.5))
            && let Some((ready, release)) = self.gate.lock().unwrap().take()
        {
            ready.try_send(()).unwrap();
            release.recv_timeout(WAIT).unwrap();
        }
        self.messages.try_send(message.clone()).unwrap();
        vosu_status::VOSU_OK
    }
}
fn receive(rx: &mpsc::Receiver<Message>) -> vosu_plugin_api::ActionResult {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Message::ActionResult(result) = rx
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap()
        {
            return result;
        }
    }
}
fn source(root: &Path) -> PathBuf {
    let path = root.join("private-import-sentinel.osz");
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
    zip.start_file("chart.osu", SimpleFileOptions::default())
        .unwrap();
    zip.write_all(b"osu file format v14\n\n[General]\nMode:0\n\n[Metadata]\nTitle:I1\nArtist:Sentinel\nCreator:Fixture\nVersion:Import\n\n[Difficulty]\nHPDrainRate:5\nCircleSize:4\nOverallDifficulty:6\nApproachRate:7\nSliderMultiplier:1.4\nSliderTickRate:1\n\n[TimingPoints]\n0,500,4,2,1,100,1,0\n\n[HitObjects]\n256,192,1000,1,0,0:0:0:0:\n").unwrap();
    zip.finish().unwrap();
    path
}
fn invoke(id: u64, path: &Path) -> Message {
    Message::ActionInvoke(ActionInvoke {
        invocation: id,
        action: crate::actions::IMPORT_ACTION.into(),
        session: Some(917201),
        caller: Caller {
            kind: CallerKind::Cli,
            id: "i1".into(),
        },
        args: CborValue::serialized(&serde_json::json!({"sourcePath":path})).unwrap(),
    })
}
fn install_songs(plugin: &OsuPlugin<Host, OfflineGenerator>, root: &Path, handle: u64) {
    use vosu_osu_state::*;
    let session = plugin.state.lock().unwrap().sessions[&handle].clone();
    let snapshot = StateBundleSnapshot {
        source: ClientType::Stable,
        generation: 1,
        reset_epoch: 0,
        regular: Arc::new(RegularState {
            global: GlobalState {
                songs_folder: root.to_string_lossy().into_owned(),
                ..Default::default()
            },
            settings: Default::default(),
            beatmap: Default::default(),
            menu: Default::default(),
            bass_density: Default::default(),
            gameplay: Default::default(),
            result_screen: Default::default(),
            tourney: Default::default(),
            user: Default::default(),
            lazer_multi_spectating: Default::default(),
            ranked_play: Default::default(),
            room: Default::default(),
        }),
        precise: Arc::new(PreciseState {
            catcher: None,
            status: 0,
            play_time: 0.0,
            mode: 0,
            key_overlay: Arc::new(vec![]),
            keypresses: Arc::new(vec![]),
            hit_errors: Arc::new(Default::default()),
            total_hit_errors: 0.0,
            committed_hit_error_index: 0,
            unstable_rate: 0.0,
        }),
    };
    session
        .publication
        .get()
        .unwrap()
        .bus
        .resources
        .get()
        .unwrap()
        .observe(&snapshot);
}
fn attach(plugin: &OsuPlugin<Host, OfflineGenerator>, root: &Path, handle: u64) {
    let mut facts = attached(handle, 917201, "osu!.exe", Bitness::X86);
    facts.path = Some(root.join("osu!.exe").to_string_lossy().into_owned());
    plugin.dispatch(Message::SessionAttached(facts), 0).unwrap();
    install_songs(plugin, root, handle);
}
#[test]
fn import_exact_route_typed_args_and_queued_terminal_once() {
    let (tx, rx) = mpsc::sync_channel(256);
    let host = Host {
        messages: tx,
        gate: Arc::default(),
    };
    let (plugin, root) = hermetic(host, Platform::Windows);
    attach(&plugin, root.path(), 917201);
    let path = source(root.path());
    let mut wrong = invoke(1, &path);
    if let Message::ActionInvoke(action) = &mut wrong {
        action.action = "vosu.osu.beatmap.export".into();
    }
    plugin.dispatch(wrong, 0).unwrap();
    assert_eq!(receive(&rx).code.as_deref(), Some("unknown_action"));
    let mut wrong = invoke(2, &path);
    if let Message::ActionInvoke(action) = &mut wrong {
        action.args =
            CborValue::serialized(&serde_json::json!({"sourcePath":path, "destination":"unsafe"}))
                .unwrap();
    }
    plugin.dispatch(wrong, 0).unwrap();
    assert_eq!(receive(&rx).code.as_deref(), Some("invalid_arguments"));
    assert_eq!(
        std::fs::read_dir(root.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.path().extension().is_some_and(|s| s == "osz"))
            .count(),
        1
    );
    plugin.dispatch(invoke(3, &path), 0).unwrap();
    let result = receive(&rx);
    assert_eq!(result.invocation, 3);
    assert_eq!(result.status, ActionStatus::Ok);
    let value: serde_json::Value = result.value.unwrap().deserialized().unwrap();
    assert_eq!(
        value,
        serde_json::json!({"client":"stable","disposition":"queued","refreshRequired":true})
    );
    plugin
        .dispatch(Message::ActionCancel(ActionCancel { invocation: 3 }), 0)
        .unwrap();
    wait_until(|| plugin.imports.finished(), "import worker completion");
    plugin.imports.shutdown();
    assert!(!rx.try_iter().any(|m| matches!(m, Message::ActionResult(_))));
    assert_eq!(
        std::fs::read_dir(root.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.path().extension().is_some_and(|s| s == "osz"))
            .count(),
        2
    );
}
#[test]
fn cancellation_and_detach_during_preparation_never_redirect_and_terminal_once() {
    for detach in [false, true] {
        let (tx, rx) = mpsc::sync_channel(256);
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let host = Host {
            messages: tx,
            gate: Arc::new(Mutex::new(Some((ready_tx, release_rx)))),
        };
        let (plugin, root) = hermetic(host, Platform::Windows);
        attach(&plugin, root.path(), 917201);
        let path = source(root.path());
        plugin.dispatch(invoke(7, &path), 0).unwrap();
        ready_rx.recv_timeout(WAIT).unwrap();
        let other = tempfile::tempdir().unwrap();
        if detach {
            plugin
                .dispatch(
                    Message::SessionDetached(SessionDetached {
                        handle: 917201,
                        reason: DetachReason::Exited,
                    }),
                    0,
                )
                .unwrap();
            attach(&plugin, other.path(), 917202);
        } else {
            for _ in 0..2 {
                plugin
                    .dispatch(Message::ActionCancel(ActionCancel { invocation: 7 }), 0)
                    .unwrap();
            }
        }
        release_tx.try_send(()).unwrap();
        let result = receive(&rx);
        assert_eq!(result.invocation, 7);
        assert_eq!(result.code.as_deref(), Some("cancelled"));
        wait_until(
            || plugin.imports.finished(),
            "cancelled import worker completion",
        );
        plugin.imports.shutdown();
        assert!(!rx.try_iter().any(|m| matches!(m, Message::ActionResult(_))));
        assert_eq!(
            std::fs::read_dir(root.path())
                .unwrap()
                .filter_map(Result::ok)
                .filter(|e| e.path().extension().is_some_and(|s| s == "osz"))
                .count(),
            1
        );
        assert_eq!(std::fs::read_dir(other.path()).unwrap().count(), 0);
    }
}

#[test]
fn early_cancellation_is_deduplicated_bounded_and_consumed() {
    let (tx, rx) = mpsc::sync_channel(256);
    let host = Host {
        messages: tx,
        gate: Arc::default(),
    };
    let (plugin, root) = hermetic(host, Platform::Windows);
    attach(&plugin, root.path(), 917201);
    let path = source(root.path());
    for invocation in 100..164 {
        for _ in 0..2 {
            plugin
                .dispatch(Message::ActionCancel(ActionCancel { invocation }), 0)
                .unwrap();
        }
    }
    assert!(
        plugin
            .dispatch(Message::ActionCancel(ActionCancel { invocation: 164 }), 0)
            .is_err()
    );
    plugin.dispatch(invoke(100, &path), 0).unwrap();
    let result = receive(&rx);
    assert_eq!(result.invocation, 100);
    assert_eq!(result.code.as_deref(), Some("cancelled"));
    // Consuming an early cancel makes room again; duplicate cancels did not
    // consume two slots, and saturated cancellation never silently evicts one.
    plugin
        .dispatch(Message::ActionCancel(ActionCancel { invocation: 164 }), 0)
        .unwrap();
    wait_until(
        || plugin.imports.finished(),
        "early-cancelled worker completion",
    );
    plugin.imports.shutdown();
    assert!(!rx.try_iter().any(|m| matches!(m, Message::ActionResult(_))));
    assert_eq!(
        std::fs::read_dir(root.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.path().extension().is_some_and(|s| s == "osz"))
            .count(),
        1
    );
}

#[test]
fn import_worker_capacity_is_real_and_cancellation_releases_workers() {
    let (tx, rx) = mpsc::sync_channel(256);
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    let host = Host {
        messages: tx,
        gate: Arc::new(Mutex::new(Some((ready_tx, release_rx)))),
    };
    let (plugin, root) = hermetic(host, Platform::Windows);
    attach(&plugin, root.path(), 917201);
    let path = source(root.path());
    plugin.dispatch(invoke(1, &path), 0).unwrap();
    ready_rx.recv_timeout(WAIT).unwrap();
    // The first progress callback holds the fixture's gate mutex; the other
    // seven workers cannot pass preparation until it is released.
    for id in 2..=9 {
        plugin.dispatch(invoke(id, &path), 0).unwrap();
    }
    let busy = receive(&rx);
    assert_eq!(busy.invocation, 9);
    assert_eq!(busy.code.as_deref(), Some("busy"));
    for invocation in 1..=8 {
        plugin
            .dispatch(Message::ActionCancel(ActionCancel { invocation }), 0)
            .unwrap();
    }
    release_tx.try_send(()).unwrap();
    let mut ids = std::collections::HashSet::new();
    for _ in 0..8 {
        let result = receive(&rx);
        assert_eq!(result.code.as_deref(), Some("cancelled"));
        assert!(ids.insert(result.invocation));
    }
    assert_eq!(ids, (1..=8).collect());
    wait_until(|| plugin.imports.finished(), "saturated workers completion");
    // Admission reaps finished workers rather than accumulating handles.
    plugin.dispatch(invoke(10, &path), 0).unwrap();
    assert_eq!(receive(&rx).status, ActionStatus::Ok);
    wait_until(
        || plugin.imports.finished(),
        "replacement worker completion",
    );
    plugin.imports.shutdown();
    assert!(!rx.try_iter().any(|m| matches!(m, Message::ActionResult(_))));
}

#[cfg(target_os = "linux")]
#[test]
fn lazer_dispatch_freezes_selected_process_context_and_reports_submitted() {
    use std::{
        io::{BufRead, BufReader},
        os::unix::fs::PermissionsExt,
        process::{Command, Stdio},
    };
    let (tx, rx) = mpsc::sync_channel(256);
    let host = Host {
        messages: tx,
        gate: Arc::default(),
    };
    let (plugin, root) = hermetic(host, Platform::Windows);
    let executable = root.path().join("selected-lazer");
    let output = root.path().join("submitted-context");
    std::fs::write(&executable, b"#!/bin/sh\nif [ \"$#\" -eq 0 ]; then echo ready; read answer; else printf '%s\\n%s\\n' \"$VOSU_I1_CLIENT\" \"$PWD\" > \"$VOSU_I1_OUTPUT\"; fi\n").unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut child = Command::new(&executable)
        .current_dir(root.path())
        .env("VOSU_I1_CLIENT", "selected-lazer-not-default")
        .env("VOSU_I1_OUTPUT", &output)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    let reader = std::thread::spawn(move || {
        let mut line = String::new();
        let result = BufReader::new(stdout).read_line(&mut line);
        let _ = ready_tx.try_send((result, line));
    });
    let (result, line) = ready_rx.recv_timeout(WAIT).unwrap();
    result.unwrap();
    assert_eq!(line.trim(), "ready");
    wait_until(|| reader.is_finished(), "lazer fixture readiness reader");
    reader.join().unwrap();
    let mut facts = attached(917201, child.id(), "osulazer.exe", Bitness::X64);
    facts.path = Some(executable.to_string_lossy().into_owned());
    plugin.dispatch(Message::SessionAttached(facts), 0).unwrap();
    wait_until(
        || plugin.state.lock().unwrap().sessions.contains_key(&917201),
        "lazer import context registration",
    );
    {
        let state = plugin.state.lock().unwrap();
        let (_, context) = state.sessions[&917201].import_context.get().unwrap();
        assert_eq!(context.cwd, root.path());
        assert!(
            context
                .environment
                .contains(&("VOSU_I1_CLIENT".into(), "selected-lazer-not-default".into()))
        );
    }
    let path = source(root.path());
    plugin.dispatch(invoke(91, &path), 0).unwrap();
    let result = receive(&rx);
    child.kill().unwrap();
    let deadline = Instant::now() + WAIT;
    while child.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "lazer fixture did not exit");
        std::thread::yield_now();
    }
    assert_eq!(result.status, ActionStatus::Ok);
    let value: serde_json::Value = result.value.unwrap().deserialized().unwrap();
    assert_eq!(
        value,
        serde_json::json!({"client":"lazer","disposition":"submitted","refreshRequired":false})
    );
    assert_eq!(
        std::fs::read_to_string(output).unwrap(),
        format!("selected-lazer-not-default\n{}\n", root.path().display())
    );
    wait_until(
        || plugin.imports.finished(),
        "lazer submission worker completion",
    );
    plugin.imports.shutdown();
    assert!(!rx.try_iter().any(|m| matches!(m, Message::ActionResult(_))));
}

#[test]
fn action_reentering_public_identify_already_has_a_registered_session() {
    #[derive(Clone)]
    struct ImmediateHost {
        messages: mpsc::SyncSender<Message>,
        plugin: Arc<OnceLock<std::sync::Weak<OsuPlugin<ImmediateHost, OfflineGenerator>>>>,
    }
    impl HostServices for ImmediateHost {
        fn send(&self, message: &Message, _: u64) -> vosu_status {
            if let Message::SessionIdentify(_) = message {
                let plugin = self.plugin.get().unwrap().upgrade().unwrap();
                // Exactly the public visibility boundary, before identify send
                // returns. The old order returned target_gone synchronously.
                plugin
                    .dispatch(
                        invoke(991, &PathBuf::from("/missing-j3-immediate-archive.osz")),
                        0,
                    )
                    .unwrap();
            }
            self.messages.try_send(message.clone()).unwrap();
            vosu_status::VOSU_OK
        }
    }
    let (tx, rx) = mpsc::sync_channel(256);
    let host = ImmediateHost {
        messages: tx,
        plugin: Arc::default(),
    };
    let (plugin, _root) = hermetic(host, Platform::Windows);
    let plugin = Arc::new(plugin);
    plugin
        .host
        .plugin
        .set(Arc::downgrade(&plugin))
        .unwrap_or_else(|_| panic!("set once"));
    plugin
        .dispatch(
            Message::SessionAttached(attached(917201, 783341, "osu!.exe", Bitness::X86)),
            0,
        )
        .unwrap();
    let result = receive(&rx);
    assert_eq!(result.invocation, 991);
    assert_eq!(result.code.as_deref(), Some("invalid_archive"));
    assert!(plugin.state.lock().unwrap().sessions.contains_key(&917201));
    wait_until(|| plugin.imports.finished(), "immediate import completion");
    plugin.imports.shutdown();
    assert!(!rx.try_iter().any(|m| matches!(m, Message::ActionResult(_))));
}

#[cfg(all(feature = "test-harness", debug_assertions))]
#[test]
fn parity_harness_import_uses_the_production_handoff() {
    let root = tempfile::tempdir().unwrap();
    let (tx, rx) = mpsc::sync_channel(256);
    let host = Arc::new(Host {
        messages: tx,
        gate: Arc::default(),
    });
    let imports = crate::actions::ImportActions::default();
    let songs = root.path().join("Songs");
    std::fs::create_dir(&songs).unwrap();
    let source = source(root.path());
    let malformed = root.path().join("malformed.osz");
    std::fs::write(&malformed, b"not an archive").unwrap();
    let lifecycle = tokio_util::sync::CancellationToken::new();
    let call = |id, path: &Path, staged| {
        let Message::ActionInvoke(action) = invoke(id, path) else {
            unreachable!()
        };
        imports.invoke_fixture(&host, action, staged).unwrap();
    };
    let staged = Some((&lifecycle, songs.as_path()));
    call(41, &source, None);
    assert_eq!(receive(&rx).code.as_deref(), Some("target_gone"));
    call(42, &malformed, staged);
    assert_eq!(receive(&rx).code.as_deref(), Some("invalid_archive"));
    assert_eq!(std::fs::read_dir(&songs).unwrap().count(), 0);

    let (entered, preparing) = mpsc::sync_channel(1);
    let (release, continue_work) = mpsc::sync_channel(1);
    *host.gate.lock().unwrap() = Some((entered, continue_work));
    call(43, &source, staged);
    preparing.recv_timeout(WAIT).unwrap();
    imports.cancel(43).unwrap();
    release.send(()).unwrap();
    assert_eq!(receive(&rx).code.as_deref(), Some("cancelled"));
    assert_eq!(std::fs::read_dir(&songs).unwrap().count(), 0);

    call(44, &source, staged);
    let result = receive(&rx);
    assert_eq!(result.status, ActionStatus::Ok);
    assert_eq!(
        result
            .value
            .unwrap()
            .deserialized::<serde_json::Value>()
            .unwrap(),
        serde_json::json!({"client":"stable","disposition":"queued","refreshRequired":true})
    );
    imports.cancel(44).unwrap();
    imports.shutdown();
    let entries: Vec<_> = std::fs::read_dir(&songs).unwrap().map(Result::unwrap).collect();
    assert_eq!(entries.len(), 1);
    let installed = entries[0].path();
    assert_eq!(installed.extension().and_then(|s| s.to_str()), Some("osz"));
    assert_eq!(
        std::fs::read(installed).unwrap(),
        std::fs::read(source).unwrap()
    );
    assert!(!rx.try_iter().any(|m| matches!(m, Message::ActionResult(_))));
}

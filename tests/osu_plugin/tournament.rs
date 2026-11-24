use super::*;
#[test]
fn stable_manager_priority_aggregate_all_sessions_and_generation_replacement() {
    let harness = Harness::new("vosu.osu", 0x6a00_1100, 4);
    let [manager, ordinary, right, left] = harness.pids[..].try_into().unwrap();
    for (pid, args) in [
        (manager, vec!["--tournament"]),
        (ordinary, vec![]),
        (right, vec!["--spectateclient=11"]),
        (left, vec!["--spectateclient=3"]),
    ] {
        harness.host.process_appeared(Harness::record(pid, &args));
    }
    eventually(|| HANDLES.lock().unwrap().len() == 4);
    let handles = HANDLES.lock().unwrap().clone();
    eventually(|| {
        namespace(&harness, handles[&ordinary], "vosu.osu").is_some_and(|v| {
            v["tourney"]["clients"]
                .as_array()
                .is_some_and(|v| v.len() == 2)
        })
    });
    eventually(|| {
        let aggregate =
            namespace(&harness, handles[&ordinary], "vosu.osu").unwrap()["tourney"].clone();
        [manager, ordinary, right, left].iter().all(|pid| {
            namespace(&harness, handles[pid], "vosu.osu").is_some_and(|v| v["tourney"] == aggregate)
        })
    });
    let aggregate = namespace(&harness, handles[&ordinary], "vosu.osu").unwrap()["tourney"].clone();
    assert_eq!(aggregate["clients"][0]["ipcId"], 3.0);
    assert_eq!(aggregate["clients"][1]["ipcId"], 11.0);
    assert_eq!(aggregate["clients"][0]["team"], "left");
    assert_eq!(aggregate["clients"][1]["team"], "right");
    harness.foreground.set(Some(ordinary));
    harness.host.foreground_changed();
    eventually(|| {
        namespace(&harness, handles[&manager], "vosu.client").is_some_and(|v| v["focused"] == true)
    });
    assert_eq!(
        namespace(&harness, handles[&ordinary], "vosu.client").unwrap()["focused"],
        false,
        "priority is not the plain OS foreground rule"
    );
    #[cfg(windows)]
    {
        eventually(|| {
            namespace(&harness, handles[&ordinary], "vosu.osu")
                .is_some_and(|v| v["session"]["focused"] == true)
        });
        assert_eq!(
            namespace(&harness, handles[&manager], "vosu.osu").unwrap()["session"]["focused"],
            false,
            "per-session foreground facts remain accurate for adapter-owned OR"
        );
        let requester = harness
            .host
            .surface_requester(vosu_core::plugin::PluginInstanceId::new(1))
            .unwrap();
        let capture = requester
            .capture(vosu_surface::state::SessionTarget::Focused, &[])
            .unwrap();
        let selected = capture.session.as_ref().unwrap();
        assert_eq!(selected.foreground, Some(false));
        assert!(
            capture
                .roster
                .sessions
                .iter()
                .any(|session| session.owner_instance == selected.owner_instance
                    && session.game == "osu"
                    && session.foreground == Some(true)),
            "all-owned-session OR includes the nonselected foreground session"
        );
    }
    harness.host.process_exited(left);
    eventually(|| {
        namespace(&harness, handles[&ordinary], "vosu.osu").is_some_and(|v| {
            v["tourney"]["clients"]
                .as_array()
                .is_some_and(|v| v.len() == 1)
        })
    });
    harness.host.process_exited(manager);
    eventually(|| {
        namespace(&harness, handles[&ordinary], "vosu.osu").is_some_and(|v| {
            v["tourney"]["clients"]
                .as_array()
                .is_some_and(Vec::is_empty)
        })
    });
    harness
        .host
        .process_appeared(Harness::record(manager, &["--tournament"]));
    eventually(|| HANDLES.lock().unwrap()[&manager] != handles[&manager]);
    let replacement = HANDLES.lock().unwrap()[&manager];
    eventually(|| {
        namespace(&harness, replacement, "vosu.client").is_some_and(|v| v["focused"] == true)
    });
    eventually(|| {
        namespace(&harness, handles[&ordinary], "vosu.osu").is_some_and(|v| {
            v["tourney"]["clients"]
                .as_array()
                .is_some_and(|v| v.len() == 1)
        })
    });
    assert_dispatches_ok();
}

#[test]
fn real_tourney_subscription_drives_and_releases_only_spectator_sampling() {
    use vosu_plugin_api::Selector;
    use vosu_plugin_testkit::subscriber::SubscribeCommand;
    let mut harness = Harness::new("vosu.osu", 0x6a00_1200, 3);
    harness.remove_b1();
    let [manager, spectator, ordinary] = harness.pids[..].try_into().unwrap();
    for (pid, args) in [
        (manager, vec!["--tournament"]),
        (spectator, vec!["--spectateclient=7"]),
        (ordinary, vec![]),
    ] {
        harness.host.process_appeared(Harness::record(pid, &args));
    }
    eventually(|| {
        HANDLES.lock().unwrap().len() == 3
            && harness
                .pids
                .iter()
                .all(|pid| vosu_osu::test_support::session_demand(*pid).is_some())
    });
    let handles = HANDLES.lock().unwrap().clone();
    let (events, commands) = harness.subscriber("test.osu.tourney.demand");
    commands
        .try_send(SubscribeCommand {
            session: None,
            selectors: vec![Selector {
                ns: "vosu.sessions".into(),
                pointer: String::new(),
            }],
        })
        .unwrap();
    let ack = || {
        let deadline = Instant::now() + WAIT;
        loop {
            if let vosu_plugin_testkit::subscriber::SubscriberEvent::SubscribeStatus(status) =
                events
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                    .unwrap()
            {
                assert_eq!(
                    status,
                    vosu_plugin_api::vosu_status::VOSU_OK,
                    "real subscription refused"
                );
                break;
            }
        }
    };
    ack();
    let select = |handle, path: Option<&str>| {
        commands
            .try_send(SubscribeCommand {
                session: Some(handle),
                selectors: path
                    .map(|path| {
                        vec![Selector {
                            ns: "vosu.osu".into(),
                            pointer: path.into(),
                        }]
                    })
                    .unwrap_or_default(),
            })
            .unwrap();
        ack();
    };
    let precise =
        |pid| vosu_osu::test_support::session_demand(pid).is_some_and(|demand| demand.precise);
    assert_eq!(
        harness.pids.iter().filter(|pid| precise(**pid)).count(),
        0,
        "no subscription means zero derived precision"
    );
    select(handles[&manager], Some("/tourney/clients/0/play/hitErrors"));
    eventually(|| precise(manager) && precise(spectator));
    assert!(
        !precise(ordinary),
        "ordinary sessions do not become aggregate sources"
    );
    harness.memories[2].set_status(2);
    select(handles[&ordinary], Some("/play/hitErrors"));
    select(handles[&manager], None);
    eventually(|| precise(ordinary) && !precise(manager) && !precise(spectator));
    select(handles[&ordinary], None);
    eventually(|| harness.pids.iter().all(|pid| !precise(*pid)));
    assert_eq!(
        harness.pids.iter().filter(|pid| precise(**pid)).count(),
        0,
        "replacement/unsubscribe leave zero precise demand"
    );
    select(handles[&manager], Some("/tourney/clients/0/play/hitErrors"));
    eventually(|| precise(spectator));
    harness.host.process_exited(manager);
    eventually(|| !precise(spectator) && !precise(ordinary));
    assert_eq!(
        harness.pids.iter().filter(|pid| precise(**pid)).count(),
        0,
        "manager loss releases the derived sampler"
    );
    harness.host.destroy_all(WAIT);
    eventually(|| vosu_osu::test_support::live_sessions() == 0);
    assert_dispatches_ok();
}

use super::support::*;
use serde_json::json;
use vosu_core::api::{ApiPlan, SessionTarget};

#[test]
fn f5_abi_fixture_identifies_and_session_events_remove_selected_indices() {
    let mut h = Harness::new();
    h.load(
        include_str!("../fixtures/api-v1-plugin/manifest.toml"),
        &[include_str!("../fixtures/api-v1-plugin/namespace.json")],
        &[
            (
                "args.json",
                include_str!("../fixtures/api-v1-plugin/args.json"),
            ),
            (
                "result.json",
                include_str!("../fixtures/api-v1-plugin/result.json"),
            ),
        ],
        f5::vosu_plugin_vtable,
    );
    h.runtime.block_on(bounded(async {
        let server = Server::new(&h, false).await;
        assert_eq!(server.get("/sessions").await.1, json!({"sessions": []}));
        let mut socket = server.ws("/sessions/subscribe").await;
        let source = h.host.api_state_source();
        let mut changes = source
            .subscribe_sessions(ApiPlan::new(&[""]).unwrap())
            .unwrap();
        h.host.process_appeared(record(894_001, "api_v1_target"));
        bounded(async {
            while source.sessions().unwrap().is_empty() {
                changes.updates.changed().await.unwrap();
            }
        })
        .await;
        let id = h.id();
        let summary = server.get(&format!("/sessions/{id}")).await.1;
        assert_eq!(summary["game"], "fixture");
        assert_eq!(summary["variant"], "synthetic");
        assert_eq!(summary["pid"], 894_001);
        assert_eq!(summary["plugin"]["id"], "test.api.fixture");
        assert_eq!(summary.as_object().unwrap().len(), 8);
        assert_eq!(server.get("/sessions/active").await.1, summary);
        let initial = select(&mut socket, &[&format!("/{id}"), "/never-attached"]).await;
        assert_eq!(initial[0], 0);
        assert_eq!(initial[2], json!([[0, summary]]));
        let full = server.get(&format!("/sessions/{id}/state")).await.1;
        assert_eq!(full["vosu.client"], summary);
        h.host.process_exited(894_001);
        let removed = frame(&mut socket).await;
        assert_eq!(removed[0], 1);
        assert_eq!(removed[2], json!([]));
        assert_eq!(removed[3], json!([0]));
        assert!(removed[1].as_u64().unwrap() > initial[1].as_u64().unwrap());
        assert_eq!(
            server.get(&format!("/sessions/{id}")).await.1["error"]["code"],
            "unknown_session"
        );
        drop(socket);
        drop(changes);
        server.shutdown().await;
    }));
}

#[test]
fn active_stream_rebinds_with_fresh_initial_and_monotonic_revision() {
    let mut h = Harness::new();
    let (first, _) = h.publisher("test.s4.focus.first", 894_002);
    let (second, _) = h.publisher("test.s4.focus.second", 894_003);
    let demanded = |handle| {
        !h.host
            .state()
            .subscriptions
            .lock()
            .unwrap()
            .demand(vosu_core::state::SessionKey(handle), Some("play"))
            .selectors
            .is_empty()
    };
    h.runtime.block_on(bounded(async {
        let server = Server::new(&h, false).await;
        let mut socket = server.ws("/sessions/active/subscribe").await;
        let initial = select(&mut socket, &["/vosu.client/game"]).await;
        assert_eq!(initial[2], json!([[0, "testkit"]]));
        assert!(demanded(first));
        assert!(!demanded(second));
        h.foreground.set(Some(894_003));
        h.host.foreground_changed();
        let next = frame(&mut socket).await;
        assert_eq!(
            next[0], 0,
            "equal selected values still reset focus baseline"
        );
        assert_eq!(next[2], initial[2]);
        assert!(next[1].as_u64().unwrap() > initial[1].as_u64().unwrap());
        assert_eq!(server.get("/sessions/active").await.1["pid"], 894_003);
        assert!(
            !demanded(first),
            "active rebinding releases old session demand"
        );
        assert!(
            demanded(second),
            "active rebinding installs new session demand"
        );
        h.host.process_exited(894_002);
        h.host.process_exited(894_003);
        let absent = frame(&mut socket).await;
        assert_eq!(absent[3], json!([0]));
        // Active survives total focus loss, unlike a fixed-id stream.
        assert!(server.hub.connect(SessionTarget::Active).is_err());
        drop(socket);
        server.shutdown().await;
    }));
}

#[test]
fn attached_but_unidentified_session_is_hidden_on_every_route() {
    use vosu_plugin_testkit::session::{self, OnAttach, SessionEvent, SessionPluginConfig};
    let mut h = Harness::new();
    let id = "test.s4.hidden";
    let events = session::register_with_config(
        id,
        SessionPluginConfig {
            on_attach: OnAttach::Ignore,
            ..SessionPluginConfig::default()
        },
    );
    h.load(
        &format!(
            "{}\n[match]\nprocess_names=['{id}']\n[[provides]]\nid='game:fixture'\nversion='*'\n",
            manifest(id)
        ),
        &[],
        &[],
        session::vosu_plugin_vtable,
    );
    h.host.process_appeared(record(894_004, id));
    let SessionEvent::Attached(attached) = events.recv_timeout(WAIT).unwrap() else {
        panic!("attach acknowledgement");
    };
    assert_ne!(attached.handle, 0);
    h.runtime.block_on(bounded(async {
        let server = Server::new(&h, false).await;
        assert_eq!(server.get("/sessions").await.1, json!({"sessions": []}));
        for path in [
            "/sessions/active",
            "/sessions/active/state",
            "/sessions/active/state/vosu.client",
        ] {
            assert_eq!(server.get(path).await.1["error"]["code"], "unknown_session");
        }
        let mut socket = server.ws("/sessions/subscribe").await;
        assert_eq!(select(&mut socket, &[""]).await[2], json!([[0, {}]]));
        let result = bounded(tokio_tungstenite::connect_async(format!(
            "{}/sessions/active/subscribe",
            server.base.replacen("http:", "ws:", 1)
        )))
        .await;
        let tokio_tungstenite::tungstenite::Error::Http(response) = result.unwrap_err() else {
            panic!("HTTP unknown-session rejection");
        };
        assert_eq!(response.status(), 404);
        drop(socket);
        server.shutdown().await;
    }));
}

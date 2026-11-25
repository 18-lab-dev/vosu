//! C0 handoff: real `vosu x` subprocesses and the production J2/broker adapters.
use super::{
    job_fixture as fixture,
    jobs::Authority,
    support::{Harness, bounded, record},
};
use axum::{
    extract::{ConnectInfo, Request, State},
    http::Method,
};
use std::{
    net::SocketAddr,
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};
use vosu::server::{
    api_v1::{
        actions::ActionRoutes, router::ApiV1Router, subscriptions::ApiV1SubscriptionHub,
        test_support::ActionBroker,
    },
    routes::PeerAddress,
};
use vosu_core::actions::{JobOwner, JobRegistry};

#[derive(Clone)]
struct Server {
    api: ApiV1Router,
    broker: ActionBroker,
    calls: Arc<Mutex<Vec<String>>>,
    polls: tokio::sync::mpsc::Sender<()>,
}
async fn handler(
    State(server): State<Server>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    mut request: Request,
) -> axum::response::Response {
    let method = request.method().clone();
    server
        .calls
        .lock()
        .unwrap()
        .push(format!("{method} {}", request.uri()));
    if method == Method::GET {
        let _ = server.polls.try_send(());
    }
    server.broker.attach(&mut request);
    request
        .extensions_mut()
        .insert(ConnectInfo(PeerAddress(peer)));
    server.api.handle(request).await
}
fn command(root: &std::path::Path, action: &str) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_vosu"));
    command
        .args(["x", "fixture.c0.game", action, "{\"n\":23}"])
        .env("XDG_RUNTIME_DIR", root)
        .env("XDG_CONFIG_HOME", root)
        .env("XDG_DATA_HOME", root)
        .env("XDG_CACHE_HOME", root)
        .env("LOCALAPPDATA", root)
        .env("APPDATA", root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command
}
#[test]
fn vosux_production_action_routes() {
    let h = Harness::new();
    let (events, _) = fixture::load(
        &h,
        783321,
        "fixture.c0.game",
        "\n[match]\nprocess_names=['jthree-cli.exe']\n[[provides]]\nid='game:fixture'\nversion='*'\n",
    );
    h.host.process_appeared(record(783322, "jthree-cli.exe"));
    h.runtime.block_on(bounded(async {
        bounded(async {
            while h
                .host
                .api_state_source()
                .session(&vosu_core::api::SessionTarget::Active)
                .is_err()
            {
                tokio::task::yield_now().await;
            }
        })
        .await;
        let session = h.id();
        let source = h.host.api_state_source();
        let hub = ApiV1SubscriptionHub::new(source.clone(), Duration::ZERO);
        let api = ApiV1Router::new(source, hub.clone()).with_actions(ActionRoutes::new(
            h.host.clone(),
            h.host.invocation_executor(),
            JobRegistry::new(),
            Arc::new(Authority(JobOwner::for_tool("unused-cli-tool".into()))),
        ));
        let listener = bounded(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let roots = tempfile::tempdir().unwrap();
        let channel =
            vosu_sys::platform::BuildChannel::from_version(env!("CARGO_PKG_VERSION")).unwrap();
        let broker_path = channel.root(roots.path().join("vosu")).join("broker.json");
        let broker = ActionBroker::publish(broker_path, address, &h.host).unwrap();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let (polls, mut polled) = tokio::sync::mpsc::channel(64);
        let state = Server {
            api,
            broker,
            calls: calls.clone(),
            polls,
        };
        let stop = tokio_util::sync::CancellationToken::new();
        let cancelled = stop.clone();
        let task = tokio::spawn(async move {
            bounded(async {
                axum::serve(
                    listener,
                    axum::Router::new()
                        .fallback(handler)
                        .with_state(state)
                        .into_make_service_with_connect_info::<SocketAddr>(),
                )
                .with_graceful_shutdown(cancelled.cancelled_owned())
                .await
                .unwrap();
            })
            .await;
        });
        for (action, explicit, route) in [
            ("nested.echo", false, "active"),
            ("required", false, "active"),
            ("nested.echo", true, session.as_str()),
            ("none", false, ""),
        ] {
            calls.lock().unwrap().clear();
            let mut cmd = command(roots.path(), action);
            if explicit {
                cmd.args(["--session", &session]);
            }
            let child = cmd.spawn().unwrap();
            bounded(polled.recv()).await.unwrap();
            fixture::timer(&h, 783321, 1);
            let output = bounded(child.wait_with_output()).await.unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(String::from_utf8_lossy(&output.stdout).contains("J3 terminal sentinel"));
            let vosu_plugin_api::Message::ActionInvoke(invoked) = fixture::next(&events, |m| {
                matches!(m, vosu_plugin_api::Message::ActionInvoke(_))
            }) else {
                unreachable!()
            };
            assert_eq!(invoked.action, format!("fixture.c0.game.{action}"));
            assert_eq!(
                invoked.args.deserialized::<serde_json::Value>().unwrap(),
                serde_json::json!({"n":23})
            );
            assert_eq!(invoked.caller.kind, vosu_plugin_api::CallerKind::Cli);
            assert_eq!(invoked.session.is_some(), !route.is_empty());
            let observed = calls.lock().unwrap().clone();
            let expected = if route.is_empty() {
                format!("POST /api/v1/actions/fixture.c0.game.{action}?wait=0")
            } else {
                format!("POST /api/v1/sessions/{route}/actions/fixture.c0.game.{action}?wait=0")
            };
            assert_eq!(observed[0], expected);
            assert!(observed.iter().any(|s| s.starts_with("GET /api/v1/jobs/")
                && s.contains("after=")
                && s.ends_with("&wait=1000")));
            assert_eq!(
                observed.iter().filter(|s| s.starts_with("POST ")).count(),
                1
            );
            while polled.try_recv().is_ok() {}
        }
        calls.lock().unwrap().clear();
        let output = bounded(command(roots.path(), "none").arg("--detach").output())
            .await
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(calls.lock().unwrap().len(), 1, "detach must not GET a job");
        let detached = String::from_utf8(output.stdout).unwrap();
        assert!(detached.trim().starts_with("job-"));
        fixture::timer(&h, 783321, 1);
        // A none action with an explicit target must fail locally, not probe HTTP.
        calls.lock().unwrap().clear();
        let output = bounded(
            command(roots.path(), "none")
                .args(["--session", &session])
                .output(),
        )
        .await
        .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(calls.lock().unwrap().is_empty());
        #[cfg(target_os = "linux")]
        {
            calls.lock().unwrap().clear();
            let child = command(roots.path(), "none").spawn().unwrap();
            let pid = child.id().unwrap();
            bounded(polled.recv()).await.unwrap();
            let signal = bounded(
                tokio::process::Command::new("kill")
                    .args(["-INT", &pid.to_string()])
                    .output(),
            )
            .await
            .unwrap();
            assert!(signal.status.success());
            let output = bounded(child.wait_with_output()).await.unwrap();
            assert_eq!(
                output.status.code(),
                Some(130),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                calls
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|s| s.starts_with("DELETE /api/v1/jobs/"))
                    .count(),
                1
            );
            fixture::timer(&h, 783321, 1);
        }
        stop.cancel();
        bounded(task).await.unwrap();
        hub.shutdown();
    }));
    h.host.destroy_all(super::support::WAIT);
}

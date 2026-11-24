use super::*;
use crate::vosux::Broker;
use axum::{
    Json, Router,
    extract::{Request, State},
    response::IntoResponse,
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use vosu_proptest::bounded;

fn invocation(args: &[&str]) -> Invocation {
    parse("fixture.game", args.iter().map(OsString::from).collect()).unwrap()
}
fn connection(origin: String, mode: ActionSessionMode) -> Connection {
    Connection {
        broker: Broker {
            origin,
            token: "private-broker".into(),
            actions: BTreeMap::from([("fixture.game.echo".into(), mode)]),
        },
        client: reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap(),
    }
}
#[test]
fn vosux_action_parser_preserves_defaults_and_rejects_ambiguous_options() {
    assert_eq!(invocation(&["echo"]).arguments, json!({}));
    let call = invocation(&[
        "nested.echo",
        "{\"value\":17}",
        "--detach",
        "--session",
        "session-c0",
    ]);
    assert_eq!(call.action, "fixture.game.nested.echo");
    assert_eq!(call.arguments, json!({"value":17}));
    assert!(call.detach);
    assert_eq!(call.session.as_deref(), Some("session-c0"));
    for args in [
        vec![],
        vec!["echo", "bad-json"],
        vec!["echo", "{}", "{}"],
        vec!["echo", "--detach", "--detach"],
        vec!["echo", "--session"],
        vec!["../echo"],
    ] {
        assert!(
            parse("fixture.game", args.iter().map(OsString::from).collect()).is_err(),
            "{args:?}"
        );
    }
}
#[test]
fn vosux_routes_from_private_modes_without_probes() {
    for mode in [ActionSessionMode::Required, ActionSessionMode::Optional] {
        let c = connection("http://127.0.0.1:1".into(), mode);
        assert_eq!(
            invocation(&["echo"]).route(&c).unwrap(),
            "/api/v1/sessions/active/actions/fixture.game.echo?wait=0"
        );
        assert_eq!(
            invocation(&["echo", "--session", "session-c0"])
                .route(&c)
                .unwrap(),
            "/api/v1/sessions/session-c0/actions/fixture.game.echo?wait=0"
        );
    }
    let c = connection("http://127.0.0.1:1".into(), ActionSessionMode::None);
    assert_eq!(
        invocation(&["echo"]).route(&c).unwrap(),
        "/api/v1/actions/fixture.game.echo?wait=0"
    );
    assert_eq!(
        invocation(&["echo", "--session", "active"])
            .route(&c)
            .unwrap_err()
            .0,
        2
    );
    assert_eq!(invocation(&["unknown"]).route(&c).unwrap_err().0, 127);
    let c = connection("http://127.0.0.1:1".into(), ActionSessionMode::Optional);
    assert_eq!(
        invocation(&["echo", "--session", "scope /?#"])
            .route(&c)
            .unwrap(),
        "/api/v1/sessions/scope%20%2F%3F%23/actions/fixture.game.echo?wait=0"
    );
}

#[derive(Clone, Default)]
struct Server {
    calls: Arc<Mutex<Vec<String>>>,
    cancel: bool,
    redirect: Option<String>,
    polled: Option<tokio::sync::mpsc::Sender<()>>,
    terminal: Option<Value>,
}
async fn handler(State(state): State<Server>, request: Request) -> axum::response::Response {
    assert_eq!(request.headers()["authorization"], "Bearer private-broker");
    let method = request.method().clone();
    let path = request.uri().to_string();
    state.calls.lock().unwrap().push(format!("{method} {path}"));
    if let Some(target) = &state.redirect {
        return (
            axum::http::StatusCode::TEMPORARY_REDIRECT,
            [("location", target.clone())],
            Json(json!({"error":{"message":"redirect refused"}})),
        )
            .into_response();
    }
    if method == reqwest::Method::POST {
        let body = bounded!(axum::body::to_bytes(request.into_body(), 1024)).unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap(),
            json!({"arguments":{"n":23}})
        );
        assert_eq!(path, "/api/v1/actions/fixture.game.echo?wait=0");
        return Json(
            json!({"id":"job-c0", "state":"running", "revision":7, "progress":{"fraction":0.4}}),
        )
        .into_response();
    }
    if method == reqwest::Method::DELETE {
        assert_eq!(path, "/api/v1/jobs/job-c0");
        return Json(json!({"id":"job-c0", "state":"running", "revision":8})).into_response();
    }
    assert_eq!(path, "/api/v1/jobs/job-c0?after=7&wait=1000");
    if let Some(polled) = &state.polled {
        polled.try_send(()).unwrap();
    }
    if state.cancel {
        let _ = tokio::time::timeout(Duration::from_secs(1), std::future::pending::<()>()).await;
    }
    if let Some(terminal) = state.terminal {
        return Json(terminal).into_response();
    }
    Json(json!({"id":"job-c0", "state":"succeeded", "revision":8, "result":{"n":46}}))
        .into_response()
}
async fn serve(
    state: Server,
) -> (
    String,
    tokio_util::sync::CancellationToken,
    tokio::task::JoinHandle<()>,
) {
    let listener = bounded!(tokio::net::TcpListener::bind("127.0.0.1:0")).unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let stop = tokio_util::sync::CancellationToken::new();
    let cancelled = stop.clone();
    let task = tokio::spawn(async move {
        bounded!(
            Duration::from_secs(10),
            axum::serve(listener, Router::new().fallback(handler).with_state(state))
                .with_graceful_shutdown(cancelled.cancelled_owned())
                .into_future()
        )
        .unwrap();
    });
    (origin, stop, task)
}
#[tokio::test]
async fn vosux_detach_does_not_poll_and_foreground_uses_revision() {
    for detach in [true, false] {
        let state = Server::default();
        let calls = state.calls.clone();
        let (origin, stop, task) = serve(state).await;
        let c = connection(origin, ActionSessionMode::None);
        let args = if detach {
            vec!["echo", "{\"n\":23}", "--detach"]
        } else {
            vec!["echo", "{\"n\":23}"]
        };
        assert_eq!(
            bounded!(execute_with_interrupt(
                &c,
                invocation(&args),
                std::future::pending()
            ))
            .unwrap(),
            0
        );
        assert_eq!(calls.lock().unwrap().len(), if detach { 1 } else { 2 });
        stop.cancel();
        bounded!(task).unwrap();
    }
}
#[tokio::test]
async fn vosux_ctrl_c_sends_delete_once() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    let state = Server {
        cancel: true,
        polled: Some(tx),
        ..Server::default()
    };
    let calls = state.calls.clone();
    let (origin, stop, task) = serve(state).await;
    let c = connection(origin, ActionSessionMode::None);
    let interrupt = async {
        bounded!(rx.recv()).unwrap();
        Ok(())
    };
    assert_eq!(
        bounded!(execute_with_interrupt(
            &c,
            invocation(&["echo", "{\"n\":23}"]),
            interrupt
        ))
        .unwrap(),
        130
    );
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.starts_with("DELETE "))
            .count(),
        1
    );
    stop.cancel();
    bounded!(task).unwrap();
}
#[tokio::test]
async fn vosux_redirect_never_receives_broker_bearer() {
    let destination = Server::default();
    let destination_calls = destination.calls.clone();
    let (target, target_stop, target_task) = serve(destination).await;
    let source = Server {
        redirect: Some(target),
        ..Server::default()
    };
    let (origin, stop, task) = serve(source).await;
    let c = connection(origin, ActionSessionMode::None);
    assert!(
        bounded!(execute_with_interrupt(
            &c,
            invocation(&["echo", "{\"n\":23}"]),
            std::future::pending()
        ))
        .is_err()
    );
    assert!(destination_calls.lock().unwrap().is_empty());
    stop.cancel();
    target_stop.cancel();
    bounded!(task).unwrap();
    bounded!(target_task).unwrap();
}

#[tokio::test]
async fn vosux_root_dispatch_reads_private_metadata_and_returns_terminal_exit_codes() {
    for (terminal, code) in [
        (
            json!({"id":"job-c0","state":"failed","revision":8,"error":{"code":"fixture_failure","message":"Distinct failure sentinel"}}),
            1,
        ),
        (json!({"id":"job-c0","state":"cancelled","revision":8}), 130),
        (
            json!({"id":"job-c0","state":"succeeded","revision":8,"result":47}),
            0,
        ),
    ] {
        let server = Server {
            terminal: Some(terminal),
            ..Server::default()
        };
        let calls = server.calls.clone();
        let (origin, stop, task) = serve(server).await;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("broker.json");
        std::fs::write(&path, serde_json::to_vec(&json!({"origin":origin,"token":"private-broker","actions":{"fixture.game.echo":"none"}})).unwrap()).unwrap();
        let parsed = crate::vosux::parse(
            ["fixture.game", "echo", "{\"n\":23}"]
                .map(OsString::from)
                .into(),
        )
        .unwrap();
        let result = bounded!(crate::vosux::execute_from_broker(parsed, &path));
        if code == 1 {
            let (actual, message) = result.unwrap_err();
            assert_eq!(actual, code);
            assert!(message.contains("fixture_failure"));
            assert!(message.contains("Distinct failure sentinel"));
        } else {
            assert_eq!(result.unwrap(), code);
        }
        assert_eq!(
            calls.lock().unwrap().len(),
            2,
            "one create and one poll, no route probing"
        );
        stop.cancel();
        bounded!(task).unwrap();
    }
}

#[tokio::test]
async fn vosux_rejects_broker_origin_query_before_sending_credentials() {
    let server = Server::default();
    let calls = server.calls.clone();
    let (origin, stop, task) = serve(server).await;
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("broker.json");
    std::fs::write(&path, serde_json::to_vec(&json!({"origin":format!("{origin}/?sentinel=private"),"token":"private-broker","actions":{"fixture.game.echo":"none"}})).unwrap()).unwrap();
    let parsed = crate::vosux::parse(
        ["fixture.game", "echo", "{\"n\":23}"]
            .map(OsString::from)
            .into(),
    )
    .unwrap();
    assert_eq!(
        bounded!(crate::vosux::execute_from_broker(parsed, &path))
            .unwrap_err()
            .0,
        69
    );
    assert!(calls.lock().unwrap().is_empty());
    stop.cancel();
    bounded!(task).unwrap();
}

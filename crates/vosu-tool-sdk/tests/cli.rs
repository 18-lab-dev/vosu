use axum::{Json, Router, extract::State, http::HeaderMap, routing::get};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use vosu_tool_sdk::{Interface, ToolEnv};
#[derive(Clone)]
struct StateData {
    state: Arc<Mutex<Vec<&'static str>>>,
    deleted: Arc<Mutex<bool>>,
}
async fn fixture(
    states: Vec<&'static str>,
) -> (
    vosu_tool_sdk::Client,
    StateData,
    tokio::task::JoinHandle<()>,
) {
    let state = StateData {
        state: Arc::new(Mutex::new(states)),
        deleted: Arc::default(),
    };
    let app=Router::new().route("/tools/test/api/v1/jobs/1",get(|State(s):State<StateData>,headers:HeaderMap|async move {
        assert_eq!(headers["x-vosu-tool-token"],"cli-secret");
        let mut states=s.state.lock().unwrap();
        let state=states[0];if states.len()>1{states.remove(0);}
        Json(json!({"state":state,"message":"fixture","progress":0.5,"result":{"ok":true}}))
    }).delete(|State(s):State<StateData>|async move{*s.deleted.lock().unwrap()=true;Json(json!({"cancelled":true}))})).with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let env = ToolEnv {
        id: "test".into(),
        token: "cli-secret".into(),
        vosu_url: format!("http://{address}"),
        data_dir: "unused".into(),
        cache_dir: "unused".into(),
        interface: Interface::Cli,
    };
    (env.client(), state, task)
}
#[tokio::test]
async fn waits_for_done_and_maps_failures() {
    for (states, code) in [
        (vec!["queued", "running", "done"], 0),
        (vec!["failed"], 1),
        (vec!["cancelled"], 130),
        (vec!["invalid"], 1),
    ] {
        let (client, _, task) = fixture(states).await;
        let mut seen = Vec::new();
        let result = client
            .wait_job_until(
                "test",
                "1",
                |v| seen.push(v["state"].clone()),
                std::future::pending(),
            )
            .await;
        assert_eq!(result.as_ref().err().map_or(0, |e| e.exit_code()), code);
        if code == 0 {
            assert_eq!(seen, vec![json!("queued"), json!("running"), json!("done")]);
            assert_eq!(result.unwrap()["result"]["ok"], true);
        }
        task.abort();
    }
}
#[tokio::test]
async fn interrupt_deletes_job_then_exits_130() {
    let (client, state, task) = fixture(vec!["running"]).await;
    let result: Result<Value, _> = client.wait_job_until("test", "1", |_| {}, async {}).await;
    assert_eq!(result.unwrap_err().exit_code(), 130);
    assert!(*state.deleted.lock().unwrap());
    task.abort();
}
#[tokio::test]
async fn interrupt_while_polling_deletes_job() {
    let (client, state, task) = fixture(vec!["running"]).await;
    let result = client
        .wait_job_until("test", "1", |_| {}, async {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        })
        .await;
    assert_eq!(result.unwrap_err().exit_code(), 130);
    assert!(*state.deleted.lock().unwrap());
    task.abort();
}

#[tokio::test]
async fn status_errors_preserve_bounded_service_messages() {
    use axum::http::StatusCode;
    let app = Router::new()
        .route(
            "/tools/difficulty/api/v1/beatmap/current",
            get(|| async {
                (
                    StatusCode::NOT_FOUND,
                    Json(json!({"error":"no selected beatmap"})),
                )
            }),
        )
        .route(
            "/long",
            get(|| async {
                (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error":format!("\n{}", "x".repeat(1000))})),
                )
            }),
        )
        .route(
            "/huge",
            get(|| async {
                (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error":"x".repeat(20000)})),
                )
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = ToolEnv {
        id: "difficulty".into(),
        token: "secret".into(),
        vosu_url: format!("http://{address}"),
        data_dir: "unused".into(),
        cache_dir: "unused".into(),
        interface: Interface::Cli,
    }
    .client();
    let error = client
        .own_json::<Value>("v1/beatmap/current")
        .await
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "no selected beatmap (HTTP 404 from difficulty /v1/beatmap/current)"
    );
    let error = client
        .get_json::<Value>("/long")
        .await
        .unwrap_err()
        .to_string();
    assert!(!error.contains('\n'));
    assert!(error.len() < 600);
    assert_eq!(
        client
            .get_json::<Value>("/huge")
            .await
            .unwrap_err()
            .to_string(),
        "Bad Request (HTTP 400 from /huge)"
    );
    task.abort();
}

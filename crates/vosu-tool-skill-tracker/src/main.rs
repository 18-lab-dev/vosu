mod detect;
mod rating;
mod stable;
mod store;

use crate::{
    detect::{Detector, FIELDS, Play},
    store::Store,
};
use axum::{
    Json, Router,
    extract::{
        Path as RoutePath, Query, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::watch;
use vosu_tool_sdk::{Client, ToolEnv};

type Error = Box<dyn std::error::Error + Send + Sync>;
struct Session {
    plays: u64,
    baseline: Value,
    last: Option<Play>,
}
struct Job {
    view: Value,
    cancel: Arc<AtomicBool>,
}
struct Inner {
    store: Store,
    session: Session,
    jobs: HashMap<String, Job>,
    connection: String,
}
struct App {
    inner: Mutex<Inner>,
    client: Client,
    updates: watch::Sender<Value>,
    next_job: AtomicU64,
}
type Shared = Arc<App>;
impl App {
    fn snapshot(inner: &Inner) -> Value {
        json!({"profile":inner.store.profile,"current":Self::current(inner)})
    }
    fn current(inner: &Inner) -> Value {
        let mut deltas = serde_json::Map::new();
        for skill in rating::SKILLS {
            deltas.insert(
                skill.into(),
                json!(
                    inner.store.profile["ratings"][skill]
                        .as_f64()
                        .unwrap_or(0.0)
                        - inner.session.baseline[skill].as_f64().unwrap_or(0.0)
                ),
            );
        }
        json!({"plays":inner.session.plays,"rating_deltas":deltas,"last_play":inner.session.last,"connection":inner.connection})
    }
    fn publish(&self, inner: &Inner) {
        self.updates.send_replace(Self::snapshot(inner));
    }
    fn connection(&self, text: &str) {
        let mut i = self.inner.lock().unwrap();
        i.connection = text.into();
        self.publish(&i);
    }
    fn record(&self, p: &mut Play) -> Result<bool, Error> {
        let mut i = self.inner.lock().unwrap();
        let inserted = i.store.insert(p)?;
        if inserted {
            if p.source == "live" {
                i.session.plays += 1;
                i.session.last = Some(p.clone());
            }
            self.publish(&i);
        }
        Ok(inserted)
    }
    fn job(&self, id: &str, state: &str, progress: f64, message: &str, result: Value) {
        let mut i = self.inner.lock().unwrap();
        if let Some(j) = i.jobs.get_mut(id) {
            j.view = json!({"state":state,"progress":progress,"message":message,"result":result});
        }
    }
}
#[derive(Debug)]
struct ApiError(StatusCode, String);
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error":self.1}))).into_response()
    }
}
fn internal(e: impl std::fmt::Display) -> ApiError {
    ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}
fn not_found() -> ApiError {
    ApiError(StatusCode::NOT_FOUND, "not found".into())
}

async fn profile(State(a): State<Shared>) -> Json<Value> {
    Json(a.inner.lock().unwrap().store.profile.clone())
}
async fn current(State(a): State<Shared>) -> Json<Value> {
    Json(App::current(&a.inner.lock().unwrap()))
}
#[derive(Deserialize)]
struct History {
    limit: Option<u32>,
    before: Option<i64>,
    mode: Option<String>,
}
async fn history(
    State(a): State<Shared>,
    Query(q): Query<History>,
) -> Result<Json<Value>, ApiError> {
    if q.mode
        .as_deref()
        .is_some_and(|m| !matches!(m, "osu" | "taiko" | "fruits" | "mania"))
    {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid mode".into()));
    }
    let plays = a
        .inner
        .lock()
        .unwrap()
        .store
        .history(
            q.limit.unwrap_or(50),
            q.before.unwrap_or(i64::MAX),
            q.mode.as_deref(),
        )
        .map_err(internal)?;
    let before = plays.last().map(|p| p.id);
    Ok(Json(json!({"plays":plays,"before":before})))
}
async fn play(
    State(a): State<Shared>,
    RoutePath(id): RoutePath<i64>,
) -> Result<Json<Play>, ApiError> {
    a.inner
        .lock()
        .unwrap()
        .store
        .get(id)
        .map_err(internal)?
        .map(Json)
        .ok_or_else(not_found)
}
async fn delete_play(
    State(a): State<Shared>,
    RoutePath(id): RoutePath<i64>,
) -> Result<StatusCode, ApiError> {
    let mut i = a.inner.lock().unwrap();
    if !i.store.delete(id).map_err(internal)? {
        return Err(not_found());
    }
    if i.session.last.as_ref().is_some_and(|p| p.id == id) {
        i.session.last = None;
    }
    a.publish(&i);
    Ok(StatusCode::NO_CONTENT)
}
async fn stream(State(a): State<Shared>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| stream_socket(a, socket))
}
async fn stream_socket(a: Shared, mut socket: WebSocket) {
    let mut updates = a.updates.subscribe();
    loop {
        let data = updates.borrow_and_update().to_string();
        if socket.send(Message::Text(data.into())).await.is_err() {
            return;
        }
        loop {
            tokio::select! {changed=updates.changed()=>{if changed.is_err(){return;}break;},incoming=socket.recv()=>{match incoming{Some(Ok(Message::Ping(p)))=>{if socket.send(Message::Pong(p)).await.is_err(){return;}},Some(Ok(Message::Close(_)))|None|Some(Err(_))=>return,_=>{}}}}
        }
    }
}

async fn get_job(
    State(a): State<Shared>,
    RoutePath(id): RoutePath<String>,
) -> Result<Json<Value>, ApiError> {
    a.inner
        .lock()
        .unwrap()
        .jobs
        .get(&id)
        .map(|j| Json(j.view.clone()))
        .ok_or_else(not_found)
}
async fn cancel_job(
    State(a): State<Shared>,
    RoutePath(id): RoutePath<String>,
) -> Result<Json<Value>, ApiError> {
    let mut i = a.inner.lock().unwrap();
    let j = i.jobs.get_mut(&id).ok_or_else(not_found)?;
    j.cancel.store(true, Ordering::Relaxed);
    Ok(Json(j.view.clone()))
}
async fn import(State(a): State<Shared>) -> Result<(StatusCode, Json<Value>), ApiError> {
    let context = a.client.context().await.map_err(internal)?;
    if context.client.as_deref() != Some("stable") {
        return Err(ApiError(StatusCode::BAD_REQUEST,"Import requires an attached stable client. Lazer Realm import is unavailable; live lazer recording is supported.".into()));
    }
    let songs = context.songs_dir.ok_or_else(|| {
        ApiError(
            StatusCode::BAD_REQUEST,
            "stable songs directory unavailable".into(),
        )
    })?;
    let root = songs
        .parent()
        .ok_or_else(|| {
            ApiError(
                StatusCode::BAD_REQUEST,
                "songs directory has no parent".into(),
            )
        })?
        .to_owned();
    let id = format!(
        "{}-{}",
        std::process::id(),
        a.next_job.fetch_add(1, Ordering::Relaxed)
    );
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut i = a.inner.lock().unwrap();
        if i.jobs
            .values()
            .any(|j| matches!(j.view["state"].as_str(), Some("running" | "queued")))
        {
            return Err(ApiError(
                StatusCode::CONFLICT,
                "an import is already running".into(),
            ));
        }
        if i.jobs.len() >= 32 {
            i.jobs.clear();
        }
        i.jobs.insert(id.clone(),Job{view:json!({"state":"queued","progress":0.0,"message":"Reading stable databases","result":null}),cancel:cancel.clone()});
    }
    let job_id = id.clone();
    tokio::spawn(async move {
        let result = import_scores(&a, &job_id, &cancel, &root, &songs).await;
        match result {
            Ok(result) => {
                let state = if cancel.load(Ordering::Relaxed) {
                    "cancelled"
                } else {
                    "done"
                };
                a.job(&job_id, state, 1.0, "Import finished", result);
            }
            Err(e) => a.job(&job_id, "failed", 0.0, &e.to_string(), Value::Null),
        }
    });
    Ok((StatusCode::ACCEPTED, Json(json!({"job":id}))))
}
async fn import_scores(
    a: &Shared,
    id: &str,
    cancel: &AtomicBool,
    root: &Path,
    songs: &Path,
) -> Result<Value, Error> {
    a.job(id, "running", 0.0, "Reading stable databases", Value::Null);
    let score_bytes = read_limited(&root.join("scores.db"), 512 * 1024 * 1024).await?;
    let map_bytes = read_limited(&root.join("osu!.db"), 512 * 1024 * 1024).await?;
    let (maps, plays) = tokio::task::spawn_blocking(move || {
        Ok::<_, String>((stable::maps(&map_bytes)?, stable::scores(&score_bytes)?))
    })
    .await??;
    let total = plays.len();
    let mut inserted = 0;
    let mut missing = 0;
    let mut errors = 0;
    for (index, mut p) in plays.into_iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        if p.mode != "mania" {
            continue;
        }
        if let Some(map) = maps.get(&p.checksum) {
            p.title = map.title.clone();
            p.keys = map.keys;
            if map.mode != 3 {
                p.eligible = false;
                p.calculation_error =
                    Some("stable converted mania imports are not supported".into());
            }
            match stable::map_path(songs, map) {
                Ok(path) => match read_limited(&path, 1024 * 1024).await {
                    Ok(bytes) => p.map = Some(bytes),
                    Err(e) => p.calculation_error = Some(e.to_string()),
                },
                Err(e) => p.calculation_error = Some(e),
            }
        } else {
            p.calculation_error = Some("beatmap missing from osu!.db".into());
        }
        if p.map.is_none() {
            missing += 1;
        }
        calculate(&a.client, &mut p).await;
        if p.calculation_error.is_some() {
            errors += 1;
        }
        if a.record(&mut p)? {
            inserted += 1;
        }
        a.job(
            id,
            "running",
            (index + 1) as f64 / total.max(1) as f64,
            "Importing mania scores",
            json!({"inserted":inserted,"missing_maps":missing,"calculation_errors":errors}),
        );
    }
    Ok(
        json!({"inserted":inserted,"missing_maps":missing,"calculation_errors":errors,"total_scores":total}),
    )
}
async fn read_limited(path: &Path, max: u64) -> Result<Vec<u8>, Error> {
    use tokio::io::AsyncReadExt;
    let file = tokio::fs::File::open(path).await?;
    let mut data = Vec::new();
    file.take(max + 1).read_to_end(&mut data).await?;
    if data.len() as u64 > max {
        return Err("file exceeds size limit".into());
    }
    Ok(data)
}
async fn calculate(client: &Client, p: &mut Play) {
    if p.mode != "mania" {
        p.map = None;
        return;
    }
    if let Some(bytes) = p.map.take() {
        let path = format!("v1/calculate?rate={}&score={}", p.rate, p.wife.value);
        match vosu_tool_sdk::send_json::<Value>(
            client
                .tool(reqwest::Method::POST, "difficulty", &path)
                .timeout(Duration::from_secs(30))
                .header("Content-Type", "application/octet-stream")
                .body(bytes),
        )
        .await
        {
            Ok(d) => {
                if d["checksum"].as_str() != Some(p.checksum.as_str()) {
                    p.calculation_error =
                        Some("beatmap checksum changed; difficulty excluded".into());
                    p.eligible = false;
                } else {
                    p.difficulty = d;
                }
            }
            Err(e) => p.calculation_error = Some(e.to_string()),
        }
    } else if p.calculation_error.is_none() {
        p.calculation_error = Some("beatmap file unavailable at play start".into());
    }
}
fn enqueue(a: &Shared, queue: &tokio::sync::mpsc::Sender<Play>, play: Play) {
    if let Err(error) = queue.try_send(play) {
        let mut play = error.into_inner();
        play.map = None;
        play.calculation_error =
            Some("difficulty queue full; play retained without ratings".into());
        if let Err(error) = a.record(&mut play) {
            eprintln!("record uncalculated play: {error}");
        }
    }
}

async fn subscription(a: Shared) {
    // Calculation must not block reading transitions from the latest-state feed.
    let (queue, mut pending) = tokio::sync::mpsc::channel::<Play>(16);
    let worker = a.clone();
    tokio::spawn(async move {
        while let Some(mut play) = pending.recv().await {
            calculate(&worker.client, &mut play).await;
            if let Err(error) = worker.record(&mut play) {
                eprintln!("record play: {error}");
            }
        }
    });
    let mut backoff = 1_u64;
    loop {
        match tokio_tungstenite::connect_async(a.client.ws_url("/websocket/v2/subscribe")).await {
            Ok((mut socket, _)) => {
                let mut d = Detector::default();
                if socket
                    .send(tokio_tungstenite::tungstenite::Message::Text(
                        json!(FIELDS).to_string().into(),
                    ))
                    .await
                    .is_ok()
                {
                    a.connection("connected");
                    while let Some(frame) = socket.next().await {
                        let Ok(frame) = frame else {
                            break;
                        };
                        if let tokio_tungstenite::tungstenite::Message::Text(text) = frame {
                            let Ok(frame) = serde_json::from_str::<Value>(&text) else {
                                break;
                            };
                            let Ok((plays, started)) = d.apply(&frame) else {
                                break;
                            };
                            backoff = 1;
                            // Freeze the selected file at play start, not after a map switch.
                            if started && let Some(p) = d.active.as_mut() {
                                match a.client.context().await {
                                    Ok(ctx) => {
                                        if let Some(map) =
                                            ctx.beatmap.filter(|m| m.checksum == p.checksum)
                                        {
                                            match read_limited(&map.file, 1024 * 1024).await {
                                                Ok(bytes) => p.map = Some(bytes),
                                                Err(e) => p.calculation_error = Some(e.to_string()),
                                            }
                                        }
                                    }
                                    Err(e) => p.calculation_error = Some(e.to_string()),
                                }
                            }
                            for p in plays {
                                enqueue(&a, &queue, p);
                            }
                        }
                    }
                }
                if let Some(p) = d.disconnect() {
                    enqueue(&a, &queue, p);
                }
            }
            Err(e) => eprintln!("skill tracker subscription: {e}"),
        }
        a.connection("reconnecting");
        tokio::time::sleep(Duration::from_secs(backoff)).await;
        backoff = (backoff * 2).min(30);
    }
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let env = ToolEnv::from_env()?;
    tokio::fs::create_dir_all(&env.data_dir).await?;
    let store = Store::open(&env.data_dir.join("plays.sqlite3"))?;
    let baseline = store.profile["ratings"].clone();
    let inner = Inner {
        store,
        session: Session {
            plays: 0,
            baseline,
            last: None,
        },
        jobs: HashMap::new(),
        connection: "connecting".into(),
    };
    let (updates, _) = watch::channel(App::snapshot(&inner));
    let a = Arc::new(App {
        inner: Mutex::new(inner),
        client: env.client(),
        updates,
        next_job: AtomicU64::new(1),
    });
    tokio::spawn(subscription(a.clone()));
    let router = Router::new()
        .route("/v1/profile", get(profile))
        .route("/v1/history", get(history))
        .route("/v1/current", get(current))
        .route("/v1/plays/{id}", get(play).delete(delete_play))
        .route("/v1/stream", get(stream))
        .route("/v1/import", post(import))
        .route("/v1/jobs/{id}", get(get_job).delete(cancel_job))
        .with_state(a);
    vosu_tool_sdk::serve(&env, router).await?;
    Ok(())
}

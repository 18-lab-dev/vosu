mod cli;

use axum::{
    Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc};
use tokio::sync::Semaphore;
use vosu_tool_difficulty::{Difficulty, MAX_BYTES, Params, calculate, checksum};
use vosu_tool_sdk::{Client, ToolEnv};

#[derive(Clone)]
struct App {
    database: PathBuf,
    client: Client,
    worker: Arc<Semaphore>,
    queue: Arc<Semaphore>,
}
struct ApiError(StatusCode, String);
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error":self.1}))).into_response()
    }
}
impl From<String> for ApiError {
    fn from(e: String) -> Self {
        Self(StatusCode::BAD_REQUEST, e)
    }
}
type Result<T, E = ApiError> = std::result::Result<T, E>;
#[derive(Deserialize)]
struct Options {
    #[serde(default = "one")]
    rate: f32,
    score: Option<f32>,
}
fn one() -> f32 {
    1.0
}
impl Options {
    fn params(self) -> Result<Params> {
        Ok(Params::new(self.rate, self.score)?)
    }
}
fn db(path: &std::path::Path) -> std::result::Result<rusqlite::Connection, String> {
    rusqlite::Connection::open(path).map_err(|e| e.to_string())
}
fn cached(
    conn: &rusqlite::Connection,
    md5: &str,
    bytes: Option<&[u8]>,
    p: Params,
) -> std::result::Result<Difficulty, String> {
    use rusqlite::OptionalExtension;
    let key = p.key(md5);
    let value: Option<String> = conn
        .query_row("SELECT result FROM ratings WHERE key=?1", [&key], |r| {
            r.get(0)
        })
        .optional()
        .map_err(|e| e.to_string())?;
    if let Some(value) = value {
        return serde_json::from_str(&value).map_err(|e| e.to_string());
    }
    let stored;
    let bytes = if let Some(bytes) = bytes {
        bytes
    } else {
        stored = conn
            .query_row("SELECT bytes FROM maps WHERE checksum=?1", [md5], |r| {
                r.get::<_, Vec<u8>>(0)
            })
            .optional()
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "not calculated".to_string())?;
        &stored
    };
    let result = calculate(bytes, p)?;
    let serialized = serde_json::to_string(&result).map_err(|e| e.to_string())?;
    conn.execute(
        "INSERT OR IGNORE INTO maps(checksum,bytes) VALUES(?1,?2)",
        rusqlite::params![md5, bytes],
    )
    .map_err(|e| e.to_string())?;
    conn.execute(
        "INSERT OR REPLACE INTO ratings(key,result) VALUES(?1,?2)",
        rusqlite::params![key, serialized],
    )
    .map_err(|e| e.to_string())?;
    Ok(result)
}
impl App {
    async fn work<T: Send + 'static>(
        &self,
        f: impl FnOnce(rusqlite::Connection) -> std::result::Result<T, String> + Send + 'static,
    ) -> Result<T> {
        let queue = self.queue.clone().try_acquire_owned().map_err(|_| {
            ApiError(
                StatusCode::SERVICE_UNAVAILABLE,
                "calculation queue full".into(),
            )
        })?;
        // Serialize native calculators to bound their large working sets. The
        // cache is rechecked after acquiring this permit, coalescing identical work.
        let worker = self
            .worker
            .clone()
            .acquire_owned()
            .await
            .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let path = self.database.clone();
        tokio::task::spawn_blocking(move || {
            let _permits = (queue, worker);
            f(db(&path)?)
        })
        .await
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(|e| {
            ApiError(
                if e == "not calculated" {
                    StatusCode::NOT_FOUND
                } else {
                    StatusCode::BAD_REQUEST
                },
                e,
            )
        })
    }
}
async fn submit(
    State(app): State<App>,
    Query(options): Query<Options>,
    bytes: Bytes,
) -> Result<Json<Difficulty>> {
    let p = options.params()?;
    app.work(move |conn| cached(&conn, &checksum(&bytes), Some(&bytes), p))
        .await
        .map(Json)
}
async fn lookup(
    State(app): State<App>,
    Path(md5): Path<String>,
    Query(options): Query<Options>,
) -> Result<Json<Difficulty>> {
    let p = options.params()?;
    app.work(move |conn| cached(&conn, &md5, None, p))
        .await
        .map(Json)
}
async fn current(
    State(app): State<App>,
    Query(options): Query<Options>,
) -> Result<Json<Difficulty>> {
    let p = options.params()?;
    let map = app
        .client
        .context()
        .await
        .map_err(|e| ApiError(StatusCode::BAD_GATEWAY, e.to_string()))?
        .beatmap
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "no selected beatmap".into()))?;
    app.work(move |conn| {
        use std::io::Read;
        let mut bytes = Vec::new();
        std::fs::File::open(map.file)
            .map_err(|e| e.to_string())?
            .take((MAX_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        cached(&conn, &checksum(&bytes), Some(&bytes), p)
    })
    .await
    .map(Json)
}
async fn rates(State(app): State<App>, Path(md5): Path<String>) -> Result<Json<Value>> {
    app.work(move |conn| {
        let mut rates = Vec::with_capacity(14);
        for tick in (14..=40).step_by(2) {
            let d = cached(
                &conn,
                &md5,
                None,
                Params {
                    rate_tick: tick,
                    score: None,
                },
            )?;
            rates.push(json!({"rate":f64::from(tick)/20.0,"msd":d.msd}));
        }
        Ok(json!({"checksum":md5,"rates":rates}))
    })
    .await
    .map(Json)
}
#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    if std::env::args().nth(1).as_deref() == Some("cli") {
        if let Err(error) = cli::run().await {
            eprintln!("vosux difficulty: {error}");
            std::process::exit(1);
        }
        return Ok(());
    }
    let env = ToolEnv::from_env()?;
    std::fs::create_dir_all(&env.data_dir)?;
    let database = env.data_dir.join("difficulty.sqlite3");
    let conn = rusqlite::Connection::open(&database)?;
    conn.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE IF NOT EXISTS maps(checksum TEXT PRIMARY KEY,bytes BLOB NOT NULL); CREATE TABLE IF NOT EXISTS ratings(key TEXT PRIMARY KEY,result TEXT NOT NULL);")?;
    drop(conn);
    let app = App {
        database,
        client: env.client(),
        worker: Arc::new(Semaphore::new(1)),
        queue: Arc::new(Semaphore::new(16)),
    };
    let router = Router::new()
        .route("/v1/calculate", post(submit))
        .route("/v1/beatmap/current", get(current))
        .route("/v1/beatmap/{md5}", get(lookup))
        .route("/v1/beatmap/{md5}/rates", get(rates))
        .layer(DefaultBodyLimit::max(MAX_BYTES))
        .with_state(app);
    vosu_tool_sdk::serve(&env, router).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cache_is_durable_and_separates_parameters() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("cache.sqlite3");
        let conn = db(&path).unwrap();
        conn.execute_batch("CREATE TABLE maps(checksum TEXT PRIMARY KEY,bytes BLOB NOT NULL); CREATE TABLE ratings(key TEXT PRIMARY KEY,result TEXT NOT NULL);").unwrap();
        let bytes = include_bytes!("../tests/fixtures/cyclic.osu");
        let hash = checksum(bytes);
        let p = Params::new(1.0, None).unwrap();
        cached(&conn, &hash, Some(bytes), p).unwrap();
        drop(conn);
        let conn = db(&path).unwrap();
        let d = cached(&conn, &hash, None, p).unwrap();
        assert_eq!(d.checksum, hash);
        let ssr = cached(&conn, &hash, None, Params::new(1.1, Some(0.95)).unwrap()).unwrap();
        assert!(ssr.ssr.is_some());
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM ratings", [], |r| r.get::<_, u32>(0))
                .unwrap(),
            2
        );
        assert_eq!(
            cached(&conn, "missing", None, p).unwrap_err(),
            "not calculated"
        );
    }
}

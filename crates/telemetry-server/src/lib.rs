mod dashboard;

use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use rusqlite::{params, Connection, ErrorCode, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use telemetry_core::{
    BatchResponse, EventBatch, ProviderSnapshot, RejectedEvent, RollupSnapshot, SCHEMA_VERSION,
};
use tokio::sync::{mpsc, oneshot};

const WRITE_QUEUE_CAPACITY: usize = 256;
const WRITE_QUEUE_WAIT_TIMEOUT: Duration = Duration::from_secs(30);
const WRITE_RESULT_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone)]
pub struct ServerState {
    pub db: Arc<Mutex<Connection>>,
    pub db_path: PathBuf,
    pub auth_token: Option<String>,
    write_tx: mpsc::Sender<WriteTask>,
}

enum WriteTask {
    Events {
        batch: EventBatch,
        response: oneshot::Sender<(StatusCode, BatchResponse)>,
    },
    Rollup {
        snapshot: Box<RollupSnapshot>,
        response: oneshot::Sender<StatusCode>,
    },
    Providers {
        snapshot: ProviderSnapshot,
        response: oneshot::Sender<StatusCode>,
    },
}

impl ServerState {
    pub fn new(db: Connection, db_path: PathBuf, auth_token: Option<String>) -> Self {
        let db = Arc::new(Mutex::new(db));
        let (write_tx, write_rx) = mpsc::channel(WRITE_QUEUE_CAPACITY);
        spawn_write_worker(Arc::clone(&db), write_rx);
        Self {
            db,
            db_path,
            auth_token,
            write_tx,
        }
    }
}

fn spawn_write_worker(db: Arc<Mutex<Connection>>, mut write_rx: mpsc::Receiver<WriteTask>) {
    tokio::spawn(async move {
        while let Some(task) = write_rx.recv().await {
            match task {
                WriteTask::Events { batch, response } => {
                    let worker_db = Arc::clone(&db);
                    let result =
                        tokio::task::spawn_blocking(move || process_events(&worker_db, batch))
                            .await
                            .unwrap_or((
                                StatusCode::INTERNAL_SERVER_ERROR,
                                BatchResponse::default(),
                            ));
                    let _ = response.send(result);
                }
                WriteTask::Rollup { snapshot, response } => {
                    let worker_db = Arc::clone(&db);
                    let result =
                        tokio::task::spawn_blocking(move || process_rollup(&worker_db, *snapshot))
                            .await
                            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
                    let _ = response.send(result);
                }
                WriteTask::Providers { snapshot, response } => {
                    let worker_db = Arc::clone(&db);
                    let result = tokio::task::spawn_blocking(move || {
                        process_provider_snapshot(&worker_db, snapshot)
                    })
                    .await
                    .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
                    let _ = response.send(result);
                }
            }
        }
    });
}

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub schema_version: u32,
}

#[derive(Debug, Deserialize)]
pub struct UsageQuery {
    pub from: Option<i64>,
    pub to: Option<i64>,
    pub node_id: Option<String>,
    pub model: Option<String>,
}

pub fn init_db(path: impl AsRef<Path>) -> anyhow::Result<Connection> {
    if let Some(parent) = path
        .as_ref()
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    let conn = Connection::open(path)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA foreign_keys=ON;
         CREATE TABLE IF NOT EXISTS usage_events (
             event_id TEXT PRIMARY KEY,
             node_id TEXT NOT NULL,
             request_id TEXT NOT NULL,
             created_at INTEGER NOT NULL,
             app_type TEXT NOT NULL,
             provider_id TEXT NOT NULL,
             model TEXT NOT NULL,
             request_model TEXT NOT NULL DEFAULT '',
             pricing_model TEXT NOT NULL DEFAULT '',
             input_tokens INTEGER NOT NULL,
             output_tokens INTEGER NOT NULL,
             cache_read_tokens INTEGER NOT NULL,
             cache_creation_tokens INTEGER NOT NULL,
             input_token_semantics INTEGER NOT NULL DEFAULT 0,
             total_cost_usd TEXT NOT NULL,
             latency_ms INTEGER NOT NULL,
             status_code INTEGER NOT NULL,
             is_streaming INTEGER NOT NULL,
             data_source TEXT NOT NULL DEFAULT '',
             received_at INTEGER NOT NULL,
             UNIQUE(node_id, request_id)
         );
         CREATE INDEX IF NOT EXISTS idx_usage_events_created
             ON usage_events(created_at);
         CREATE INDEX IF NOT EXISTS idx_usage_events_node
             ON usage_events(node_id, created_at);
         CREATE TABLE IF NOT EXISTS provider_catalog (
             node_id TEXT NOT NULL,
             app_type TEXT NOT NULL,
             provider_id TEXT NOT NULL,
             name TEXT NOT NULL,
             updated_at INTEGER NOT NULL,
             PRIMARY KEY (node_id, app_type, provider_id)
         );
         CREATE TABLE IF NOT EXISTS usage_daily_snapshots (
             snapshot_key TEXT PRIMARY KEY,
             node_id TEXT NOT NULL,
             date TEXT NOT NULL,
             app_type TEXT NOT NULL,
             provider_id TEXT NOT NULL,
             model TEXT NOT NULL,
             request_model TEXT NOT NULL,
             pricing_model TEXT NOT NULL,
             request_count INTEGER NOT NULL,
             success_count INTEGER NOT NULL,
             input_tokens INTEGER NOT NULL,
             output_tokens INTEGER NOT NULL,
             cache_read_tokens INTEGER NOT NULL,
             cache_creation_tokens INTEGER NOT NULL,
             total_cost_usd TEXT NOT NULL,
             avg_latency_ms INTEGER NOT NULL,
             received_at INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS ingest_batches (
             batch_id TEXT PRIMARY KEY,
             node_id TEXT NOT NULL,
             received_at INTEGER NOT NULL,
             event_count INTEGER NOT NULL
         );",
    )?;
    let columns = conn
        .prepare("PRAGMA table_info(usage_events)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    if !columns
        .iter()
        .any(|column| column == "input_token_semantics")
    {
        conn.execute(
            "ALTER TABLE usage_events
             ADD COLUMN input_token_semantics INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
    }
    Ok(conn)
}

fn authorized(headers: &HeaderMap, expected: &Option<String>) -> bool {
    let Some(expected) = expected else {
        return true;
    };
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(|v| v == format!("Bearer {expected}"))
        .unwrap_or(false)
}

fn is_retryable_sqlite_error(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(inner, _)
            if matches!(inner.code, ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
    )
}

fn database_error_status(error: &rusqlite::Error) -> StatusCode {
    if is_retryable_sqlite_error(error) {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::INTERNAL_SERVER_ERROR
    }
}

pub async fn health(State(state): State<ServerState>) -> impl IntoResponse {
    let ok = state
        .db
        .lock()
        .ok()
        .and_then(|db| db.query_row("SELECT 1", [], |_| Ok(())).ok())
        .is_some();
    (
        if ok {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        },
        Json(HealthResponse {
            status: if ok { "ok" } else { "degraded" },
            schema_version: SCHEMA_VERSION,
        }),
    )
}

pub async fn ingest(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(batch): Json<EventBatch>,
) -> impl IntoResponse {
    if !authorized(&headers, &state.auth_token) {
        return (StatusCode::UNAUTHORIZED, Json(BatchResponse::default()));
    }
    if batch.schema_version != SCHEMA_VERSION
        || batch.events.len() > 1000
        || batch.events.iter().any(|e| e.node_id != batch.node_id)
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(BatchResponse {
                rejected: batch
                    .events
                    .into_iter()
                    .map(|e| RejectedEvent {
                        event_id: e.event_id,
                        reason: "invalid batch".into(),
                    })
                    .collect(),
                ..Default::default()
            }),
        );
    }
    let (response_tx, response_rx) = oneshot::channel();
    let task = WriteTask::Events {
        batch,
        response: response_tx,
    };
    match tokio::time::timeout(WRITE_QUEUE_WAIT_TIMEOUT, state.write_tx.send(task)).await {
        Ok(Ok(())) => {}
        Ok(Err(_)) | Err(_) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(BatchResponse::default()),
            )
        }
    }
    match tokio::time::timeout(WRITE_RESULT_TIMEOUT, response_rx).await {
        Ok(Ok((status, response))) => (status, Json(response)),
        Ok(Err(_)) | Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(BatchResponse::default()),
        ),
    }
}

fn process_events(db: &Arc<Mutex<Connection>>, batch: EventBatch) -> (StatusCode, BatchResponse) {
    let mut response = BatchResponse::default();
    let now = chrono::Utc::now().timestamp();
    let Ok(db) = db.lock() else {
        return (StatusCode::INTERNAL_SERVER_ERROR, response);
    };
    let tx = match db.unchecked_transaction() {
        Ok(tx) => tx,
        Err(error) => return (database_error_status(&error), response),
    };
    for event in &batch.events {
        let exists: Option<String> = match tx
            .query_row(
                "SELECT event_id FROM usage_events
                 WHERE event_id = ?1 OR (node_id = ?2 AND request_id = ?3)",
                params![event.event_id, event.node_id, event.request_id],
                |row| row.get(0),
            )
            .optional()
        {
            Ok(value) => value,
            Err(error) => return (database_error_status(&error), BatchResponse::default()),
        };
        if exists.is_some() {
            response.duplicates.push(event.event_id.clone());
            continue;
        }
        let result = tx.execute(
            "INSERT INTO usage_events (
                 event_id,node_id,request_id,created_at,app_type,provider_id,model,
                 request_model,pricing_model,input_tokens,output_tokens,
                 cache_read_tokens,cache_creation_tokens,input_token_semantics,
                 total_cost_usd,latency_ms,status_code,is_streaming,data_source,received_at
             ) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            params![
                event.event_id,
                event.node_id,
                event.request_id,
                event.created_at,
                event.app_type,
                event.provider_id,
                event.model,
                event.request_model.clone().unwrap_or_default(),
                event.pricing_model.clone().unwrap_or_default(),
                event.input_tokens,
                event.output_tokens,
                event.cache_read_tokens,
                event.cache_creation_tokens,
                event.input_token_semantics,
                event.total_cost_usd,
                event.latency_ms,
                event.status_code,
                event.is_streaming as i64,
                event.data_source,
                now
            ],
        );
        match result {
            Ok(_) => response.accepted.push(event.event_id.clone()),
            Err(error) if is_retryable_sqlite_error(&error) => {
                return (StatusCode::SERVICE_UNAVAILABLE, BatchResponse::default())
            }
            Err(error) => response.rejected.push(RejectedEvent {
                event_id: event.event_id.clone(),
                reason: error.to_string(),
            }),
        }
    }
    if let Err(error) = tx.commit() {
        return (database_error_status(&error), BatchResponse::default());
    }
    (StatusCode::OK, response)
}

fn process_rollup(db: &Arc<Mutex<Connection>>, snapshot: RollupSnapshot) -> StatusCode {
    let Ok(db) = db.lock() else {
        return StatusCode::INTERNAL_SERVER_ERROR;
    };
    let result = db.execute(
        "INSERT INTO usage_daily_snapshots (
             snapshot_key,node_id,date,app_type,provider_id,model,request_model,
             pricing_model,request_count,success_count,input_tokens,output_tokens,
             cache_read_tokens,cache_creation_tokens,total_cost_usd,avg_latency_ms,received_at
         ) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)
         ON CONFLICT(snapshot_key) DO UPDATE SET
             request_count=excluded.request_count,
             success_count=excluded.success_count,
             input_tokens=excluded.input_tokens,
             output_tokens=excluded.output_tokens,
             cache_read_tokens=excluded.cache_read_tokens,
             cache_creation_tokens=excluded.cache_creation_tokens,
             total_cost_usd=excluded.total_cost_usd,
             avg_latency_ms=excluded.avg_latency_ms,
             received_at=excluded.received_at",
        params![
            snapshot.snapshot_key,
            snapshot.node_id,
            snapshot.date,
            snapshot.app_type,
            snapshot.provider_id,
            snapshot.model,
            snapshot.request_model,
            snapshot.pricing_model,
            snapshot.request_count,
            snapshot.success_count,
            snapshot.input_tokens,
            snapshot.output_tokens,
            snapshot.cache_read_tokens,
            snapshot.cache_creation_tokens,
            snapshot.total_cost_usd,
            snapshot.avg_latency_ms,
            chrono::Utc::now().timestamp()
        ],
    );
    match result {
        Ok(_) => StatusCode::OK,
        Err(error) => database_error_status(&error),
    }
}

fn process_provider_snapshot(
    db: &Arc<Mutex<Connection>>,
    snapshot: ProviderSnapshot,
) -> StatusCode {
    let Ok(db) = db.lock() else {
        return StatusCode::INTERNAL_SERVER_ERROR;
    };
    let tx = match db.unchecked_transaction() {
        Ok(tx) => tx,
        Err(error) => return database_error_status(&error),
    };
    let updated_at = chrono::Utc::now().timestamp();
    for provider in snapshot.providers {
        if let Err(error) = tx.execute(
            "INSERT INTO provider_catalog (node_id, app_type, provider_id, name, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(node_id, app_type, provider_id) DO UPDATE SET
                 name=excluded.name,
                 updated_at=excluded.updated_at",
            params![
                &snapshot.node_id,
                provider.app_type,
                provider.provider_id,
                provider.name,
                updated_at
            ],
        ) {
            return database_error_status(&error);
        }
    }
    match tx.commit() {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(error) => database_error_status(&error),
    }
}

pub async fn ingest_rollup(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(snapshot): Json<RollupSnapshot>,
) -> impl IntoResponse {
    if !authorized(&headers, &state.auth_token) {
        return StatusCode::UNAUTHORIZED;
    }
    let (response_tx, response_rx) = oneshot::channel();
    let task = WriteTask::Rollup {
        snapshot: Box::new(snapshot),
        response: response_tx,
    };
    match tokio::time::timeout(WRITE_QUEUE_WAIT_TIMEOUT, state.write_tx.send(task)).await {
        Ok(Ok(())) => {}
        Ok(Err(_)) | Err(_) => return StatusCode::SERVICE_UNAVAILABLE,
    }
    match tokio::time::timeout(WRITE_RESULT_TIMEOUT, response_rx).await {
        Ok(Ok(status)) => status,
        Ok(Err(_)) | Err(_) => StatusCode::SERVICE_UNAVAILABLE,
    }
}

pub async fn ingest_provider_snapshot(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(snapshot): Json<ProviderSnapshot>,
) -> impl IntoResponse {
    if !authorized(&headers, &state.auth_token) {
        return StatusCode::UNAUTHORIZED;
    }
    if snapshot.schema_version != SCHEMA_VERSION
        || snapshot.node_id.trim().is_empty()
        || snapshot.providers.len() > 10_000
        || snapshot.providers.iter().any(|provider| {
            provider.app_type.trim().is_empty()
                || provider.provider_id.trim().is_empty()
                || provider.name.trim().is_empty()
        })
    {
        return StatusCode::BAD_REQUEST;
    }
    let (response_tx, response_rx) = oneshot::channel();
    let task = WriteTask::Providers {
        snapshot,
        response: response_tx,
    };
    match tokio::time::timeout(WRITE_QUEUE_WAIT_TIMEOUT, state.write_tx.send(task)).await {
        Ok(Ok(())) => {}
        Ok(Err(_)) | Err(_) => return StatusCode::SERVICE_UNAVAILABLE,
    }
    match tokio::time::timeout(WRITE_RESULT_TIMEOUT, response_rx).await {
        Ok(Ok(status)) => status,
        Ok(Err(_)) | Err(_) => StatusCode::SERVICE_UNAVAILABLE,
    }
}

pub async fn summary(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Query(query): Query<UsageQuery>,
) -> impl IntoResponse {
    if !authorized(&headers, &state.auth_token) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error":"unauthorized"})),
        );
    }
    let Ok(db) = state.db.lock() else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error":"database"})),
        );
    };
    let mut sql = "SELECT COUNT(*), COALESCE(SUM(input_tokens),0), COALESCE(SUM(output_tokens),0), COALESCE(SUM(cache_read_tokens),0), COALESCE(SUM(cache_creation_tokens),0), COALESCE(SUM(CAST(total_cost_usd AS REAL)),0) FROM usage_events WHERE 1=1".to_string();
    let mut values: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    if let Some(v) = query.from {
        sql.push_str(" AND created_at >= ?");
        values.push(Box::new(v));
    }
    if let Some(v) = query.to {
        sql.push_str(" AND created_at <= ?");
        values.push(Box::new(v));
    }
    if let Some(node_id) = query.node_id {
        sql.push_str(" AND node_id = ?");
        values.push(Box::new(node_id));
    }
    if let Some(model) = query.model {
        sql.push_str(" AND model = ?");
        values.push(Box::new(model));
    }
    let mut stmt = match db.prepare(&sql) {
        Ok(s) => s,
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error":"database"})),
            )
        }
    };
    let params: Vec<&dyn rusqlite::ToSql> = values.iter().map(|v| v.as_ref()).collect();
    let row = stmt.query_row(params.as_slice(), |r| {
        Ok(serde_json::json!({
            "totalRequests": r.get::<_, i64>(0)?,
            "inputTokens": r.get::<_, i64>(1)?,
            "outputTokens": r.get::<_, i64>(2)?,
            "cacheReadTokens": r.get::<_, i64>(3)?,
            "cacheCreationTokens": r.get::<_, i64>(4)?,
            "totalCostUsd": r.get::<_, f64>(5)?
        }))
    });
    match row {
        Ok(value) => (StatusCode::OK, Json(value)),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error":"query"})),
        ),
    }
}

pub fn router(state: ServerState) -> Router {
    Router::new()
        .route("/healthz", get(health))
        .route("/v1/events/batch", post(ingest))
        .route("/v1/rollups/snapshot", post(ingest_rollup))
        .route("/v1/providers/snapshot", post(ingest_provider_snapshot))
        .route("/v1/usage/summary", get(summary))
        .merge(dashboard::routes())
        .with_state(state)
}

pub async fn serve(
    db_path: PathBuf,
    listen: SocketAddr,
    auth_token: Option<String>,
) -> anyhow::Result<()> {
    let db_path_for_queries = db_path.clone();
    let state = ServerState::new(
        init_db(&db_path).map_err(|error| {
            anyhow::anyhow!(
                "initialize telemetry database {}: {error}",
                db_path.display()
            )
        })?,
        db_path_for_queries,
        auth_token,
    );
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .map_err(|error| anyhow::anyhow!("bind telemetry-server listener {listen}: {error}"))?;
    eprintln!("telemetry-server listening on http://{listen}");
    axum::serve(
        listener,
        router(state).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;

    #[test]
    fn schema_initializes() {
        let db = init_db(":memory:").unwrap();
        assert!(db
            .query_row("SELECT 1 FROM usage_events LIMIT 1", [], |_| Ok(()))
            .is_err());
        let providers: i64 = db
            .query_row("SELECT COUNT(*) FROM provider_catalog", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(providers, 0);
    }

    #[test]
    fn provider_snapshot_upserts_current_name() {
        let db = Arc::new(Mutex::new(init_db(":memory:").unwrap()));
        let snapshot = ProviderSnapshot {
            schema_version: SCHEMA_VERSION,
            node_id: "node-a".into(),
            providers: vec![telemetry_core::ProviderEntry {
                app_type: "codex".into(),
                provider_id: "provider-a".into(),
                name: "DeepSeek".into(),
            }],
        };
        assert_eq!(
            process_provider_snapshot(&db, snapshot),
            StatusCode::NO_CONTENT
        );
        let renamed = ProviderSnapshot {
            schema_version: SCHEMA_VERSION,
            node_id: "node-a".into(),
            providers: vec![telemetry_core::ProviderEntry {
                app_type: "codex".into(),
                provider_id: "provider-a".into(),
                name: "DeepSeek Renamed".into(),
            }],
        };
        assert_eq!(
            process_provider_snapshot(&db, renamed),
            StatusCode::NO_CONTENT
        );
        let name: String = db
            .lock()
            .unwrap()
            .query_row(
                "SELECT name FROM provider_catalog
                 WHERE node_id = 'node-a' AND app_type = 'codex' AND provider_id = 'provider-a'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(name, "DeepSeek Renamed");
    }

    #[tokio::test]
    async fn provider_snapshot_endpoint_requires_auth_and_persists_names() {
        let state = ServerState::new(
            init_db(":memory:").unwrap(),
            PathBuf::from("telemetry.db"),
            Some("secret".into()),
        );
        let snapshot = ProviderSnapshot {
            schema_version: SCHEMA_VERSION,
            node_id: "node-a".into(),
            providers: vec![telemetry_core::ProviderEntry {
                app_type: "codex".into(),
                provider_id: "provider-a".into(),
                name: "DeepSeek".into(),
            }],
        };
        let payload = serde_json::to_vec(&snapshot).unwrap();
        let unauthorized = router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/providers/snapshot")
                    .header("content-type", "application/json")
                    .body(Body::from(payload.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

        let response = router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/providers/snapshot")
                    .header("authorization", "Bearer secret")
                    .header("content-type", "application/json")
                    .body(Body::from(payload))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let name: String = state
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT name FROM provider_catalog
                 WHERE node_id = 'node-a' AND app_type = 'codex' AND provider_id = 'provider-a'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(name, "DeepSeek");
    }

    #[test]
    fn old_schema_migrates_input_token_semantics() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("telemetry.db");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE usage_events (
                    event_id TEXT PRIMARY KEY,
                    node_id TEXT NOT NULL,
                    request_id TEXT NOT NULL,
                    created_at INTEGER NOT NULL,
                    app_type TEXT NOT NULL,
                    provider_id TEXT NOT NULL,
                    model TEXT NOT NULL,
                    request_model TEXT NOT NULL DEFAULT '',
                    pricing_model TEXT NOT NULL DEFAULT '',
                    input_tokens INTEGER NOT NULL,
                    output_tokens INTEGER NOT NULL,
                    cache_read_tokens INTEGER NOT NULL,
                    cache_creation_tokens INTEGER NOT NULL,
                    total_cost_usd TEXT NOT NULL,
                    latency_ms INTEGER NOT NULL,
                    status_code INTEGER NOT NULL,
                    is_streaming INTEGER NOT NULL,
                    data_source TEXT NOT NULL DEFAULT '',
                    received_at INTEGER NOT NULL,
                    UNIQUE(node_id, request_id)
                 );",
            )
            .unwrap();
        drop(connection);
        let migrated = init_db(&path).unwrap();
        let semantics: i64 = migrated
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('usage_events')
                 WHERE name = 'input_token_semantics'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(semantics, 1);
    }

    #[test]
    fn sqlite_lock_errors_are_reported_as_service_unavailable() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("telemetry.db");
        let writer = Connection::open(&path).unwrap();
        writer
            .execute_batch(
                "CREATE TABLE lock_probe (value INTEGER);
                 BEGIN EXCLUSIVE;",
            )
            .unwrap();

        let contender = Connection::open(&path).unwrap();
        contender
            .busy_timeout(std::time::Duration::from_millis(1))
            .unwrap();
        let error = contender
            .execute("INSERT INTO lock_probe VALUES (1)", [])
            .unwrap_err();

        assert!(is_retryable_sqlite_error(&error));
        assert_eq!(
            database_error_status(&error),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }
}

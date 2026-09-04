mod admin;
mod dashboard;
pub mod nodes;
mod quota;
mod sync_v2;

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
    collections::HashMap,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use telemetry_core::{
    event_id, rollup_key, BatchResponse, EventBatch, ProviderSnapshot, RejectedEvent,
    RollupSnapshot, SCHEMA_VERSION,
};
use tokio::sync::{mpsc, oneshot};

const WRITE_QUEUE_CAPACITY: usize = 256;
const WRITE_QUEUE_WAIT_TIMEOUT: Duration = Duration::from_secs(30);
const WRITE_RESULT_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone)]
pub struct ServerState {
    pub db: Arc<Mutex<Connection>>,
    pub db_path: PathBuf,
    pub admin_password: Option<String>,
    pub admin_sessions: Arc<Mutex<HashMap<String, Instant>>>,
    write_tx: mpsc::Sender<WriteTask>,
}

enum WriteTask {
    Events {
        node_id: String,
        batch: EventBatch,
        response: oneshot::Sender<(StatusCode, BatchResponse)>,
    },
    Rollup {
        node_id: String,
        snapshot: Box<RollupSnapshot>,
        response: oneshot::Sender<StatusCode>,
    },
    Providers {
        node_id: String,
        snapshot: ProviderSnapshot,
        response: oneshot::Sender<StatusCode>,
    },
}

impl ServerState {
    pub fn new(db: Connection, db_path: PathBuf, admin_password: Option<String>) -> Self {
        let db = Arc::new(Mutex::new(db));
        let (write_tx, write_rx) = mpsc::channel(WRITE_QUEUE_CAPACITY);
        spawn_write_worker(Arc::clone(&db), write_rx);
        Self {
            db,
            db_path,
            admin_password,
            admin_sessions: Arc::new(Mutex::new(HashMap::new())),
            write_tx,
        }
    }
}

fn spawn_write_worker(db: Arc<Mutex<Connection>>, mut write_rx: mpsc::Receiver<WriteTask>) {
    tokio::spawn(async move {
        while let Some(task) = write_rx.recv().await {
            match task {
                WriteTask::Events {
                    node_id,
                    batch,
                    response,
                } => {
                    let worker_db = Arc::clone(&db);
                    let result = tokio::task::spawn_blocking(move || {
                        process_events(&worker_db, node_id, batch)
                    })
                    .await
                    .unwrap_or((StatusCode::INTERNAL_SERVER_ERROR, BatchResponse::default()));
                    let _ = response.send(result);
                }
                WriteTask::Rollup {
                    node_id,
                    snapshot,
                    response,
                } => {
                    let worker_db = Arc::clone(&db);
                    let result = tokio::task::spawn_blocking(move || {
                        process_rollup(&worker_db, node_id, *snapshot)
                    })
                    .await
                    .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
                    let _ = response.send(result);
                }
                WriteTask::Providers {
                    node_id,
                    snapshot,
                    response,
                } => {
                    let worker_db = Arc::clone(&db);
                    let result = tokio::task::spawn_blocking(move || {
                        process_provider_snapshot(&worker_db, node_id, snapshot)
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
             content_hash TEXT NOT NULL DEFAULT '',
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
             input_token_semantics INTEGER NOT NULL DEFAULT 2,
             total_cost_usd TEXT NOT NULL,
             avg_latency_ms INTEGER NOT NULL,
             day_start_utc INTEGER NOT NULL DEFAULT 0,
             day_end_utc INTEGER NOT NULL DEFAULT 0,
             content_hash TEXT NOT NULL DEFAULT '',
             received_at INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_usage_snapshots_node_day
             ON usage_daily_snapshots(node_id, day_start_utc, day_end_utc);
         CREATE TABLE IF NOT EXISTS sync_generations (
             generation_id TEXT PRIMARY KEY,
             node_id TEXT NOT NULL,
             source_kind TEXT NOT NULL,
             replace_all INTEGER NOT NULL,
             status TEXT NOT NULL,
             manifest_hash TEXT NOT NULL DEFAULT '',
             result_json TEXT,
             created_at INTEGER NOT NULL,
             committed_at INTEGER
         );
         CREATE TABLE IF NOT EXISTS staged_event_mutations (
             generation_id TEXT NOT NULL,
             request_id TEXT NOT NULL,
             operation TEXT NOT NULL,
             content_hash TEXT NOT NULL,
             payload_json TEXT,
             PRIMARY KEY (generation_id, request_id),
             FOREIGN KEY (generation_id) REFERENCES sync_generations(generation_id)
                 ON DELETE CASCADE
         );
         CREATE TABLE IF NOT EXISTS staged_rollup_mutations (
             generation_id TEXT NOT NULL,
             snapshot_key TEXT NOT NULL,
             operation TEXT NOT NULL,
             content_hash TEXT NOT NULL,
             payload_json TEXT,
             PRIMARY KEY (generation_id, snapshot_key),
             FOREIGN KEY (generation_id) REFERENCES sync_generations(generation_id)
                 ON DELETE CASCADE
         );
         CREATE TABLE IF NOT EXISTS staged_providers (
             generation_id TEXT NOT NULL,
             app_type TEXT NOT NULL,
             provider_id TEXT NOT NULL,
             name TEXT NOT NULL,
             PRIMARY KEY (generation_id, app_type, provider_id),
             FOREIGN KEY (generation_id) REFERENCES sync_generations(generation_id)
                 ON DELETE CASCADE
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
    ensure_column(
        &conn,
        "usage_events",
        "content_hash",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_column(
        &conn,
        "usage_daily_snapshots",
        "input_token_semantics",
        "INTEGER NOT NULL DEFAULT 2",
    )?;
    ensure_column(
        &conn,
        "usage_daily_snapshots",
        "day_start_utc",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(
        &conn,
        "usage_daily_snapshots",
        "day_end_utc",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(
        &conn,
        "usage_daily_snapshots",
        "content_hash",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    nodes::ensure_schema(&conn)?;
    quota::ensure_schema(&conn)?;
    Ok(conn)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebuildV2Summary {
    pub nodes: usize,
    pub providers: usize,
}

/// Build a fresh protocol-v2 central database while preserving only node
/// identities/token hashes and current provider labels. Usage is deliberately
/// omitted: each node must repopulate it with a forced v2 replacement.
pub fn rebuild_v2_metadata(
    source_path: impl AsRef<Path>,
    target_path: impl AsRef<Path>,
) -> anyhow::Result<RebuildV2Summary> {
    let source_path = source_path.as_ref();
    let target_path = target_path.as_ref();
    if source_path == target_path {
        anyhow::bail!("source and target database paths must differ");
    }
    if target_path.exists() {
        anyhow::bail!("target database already exists: {}", target_path.display());
    }
    let source = Connection::open_with_flags(
        source_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )?;
    let has_nodes: bool = source.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='nodes')",
        [],
        |row| row.get(0),
    )?;
    if !has_nodes {
        anyhow::bail!("source database has no nodes table");
    }
    let node_rows = {
        let mut statement = source.prepare(
            "SELECT uuid,node_name,token_hash,created_at,updated_at FROM nodes ORDER BY uuid",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    let has_providers: bool = source.query_row(
        "SELECT EXISTS(
           SELECT 1 FROM sqlite_master WHERE type='table' AND name='provider_catalog'
         )",
        [],
        |row| row.get(0),
    )?;
    let provider_rows = if has_providers {
        let mut statement = source.prepare(
            "SELECT node_id,app_type,provider_id,name,updated_at
             FROM provider_catalog ORDER BY node_id,app_type,provider_id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })?;
        rows.collect::<Result<Vec<_>, _>>()?
    } else {
        Vec::new()
    };

    let mut target = init_db(target_path)?;
    let transaction = target.transaction()?;
    for row in &node_rows {
        transaction.execute(
            "INSERT INTO nodes(uuid,node_name,token_hash,created_at,updated_at)
             VALUES (?1,?2,?3,?4,?5)",
            params![row.0, row.1, row.2, row.3, row.4],
        )?;
    }
    for row in &provider_rows {
        transaction.execute(
            "INSERT INTO provider_catalog(node_id,app_type,provider_id,name,updated_at)
             VALUES (?1,?2,?3,?4,?5)",
            params![row.0, row.1, row.2, row.3, row.4],
        )?;
    }
    transaction.commit()?;
    let integrity: String = target.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    if integrity != "ok" {
        anyhow::bail!("rebuilt v2 database failed integrity_check: {integrity}");
    }
    Ok(RebuildV2Summary {
        nodes: node_rows.len(),
        providers: provider_rows.len(),
    })
}

fn ensure_column(
    connection: &Connection,
    table: &str,
    column: &str,
    declaration: &str,
) -> anyhow::Result<()> {
    let columns = connection
        .prepare(&format!("PRAGMA table_info({table})"))?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    if !columns.iter().any(|candidate| candidate == column) {
        connection.execute(
            &format!("ALTER TABLE {table} ADD COLUMN {column} {declaration}"),
            [],
        )?;
    }
    Ok(())
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .filter(|value| !value.is_empty())
}

pub(crate) fn authenticated_node(
    headers: &HeaderMap,
    db: &Arc<Mutex<Connection>>,
) -> Option<String> {
    let token = bearer_token(headers)?;
    let Ok(db) = db.lock() else {
        return None;
    };
    nodes::authorized_uuid(&db, token)
}

fn authorized_any_node(headers: &HeaderMap, db: &Arc<Mutex<Connection>>) -> bool {
    authenticated_node(headers, db).is_some()
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
    let Some(node_id) = authenticated_node(&headers, &state.db) else {
        return (StatusCode::UNAUTHORIZED, Json(BatchResponse::default()));
    };
    if batch.schema_version != SCHEMA_VERSION || batch.events.len() > 1000 {
        return (
            StatusCode::BAD_REQUEST,
            Json(BatchResponse {
                rejected: batch
                    .events
                    .into_iter()
                    .map(|e| RejectedEvent {
                        event_id: event_id(&node_id, &e.request_id),
                        reason: "invalid batch".into(),
                    })
                    .collect(),
                ..Default::default()
            }),
        );
    }
    let (response_tx, response_rx) = oneshot::channel();
    let task = WriteTask::Events {
        node_id,
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

fn process_events(
    db: &Arc<Mutex<Connection>>,
    node_id: String,
    batch: EventBatch,
) -> (StatusCode, BatchResponse) {
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
        let generated_event_id = event_id(&node_id, &event.request_id);
        let exists: Option<String> = match tx
            .query_row(
                "SELECT event_id FROM usage_events
                 WHERE event_id = ?1 OR (node_id = ?2 AND request_id = ?3)",
                params![&generated_event_id, &node_id, &event.request_id],
                |row| row.get(0),
            )
            .optional()
        {
            Ok(value) => value,
            Err(error) => return (database_error_status(&error), BatchResponse::default()),
        };
        if exists.is_some() {
            response.duplicates.push(generated_event_id);
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
                &generated_event_id,
                &node_id,
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
            Ok(_) => response.accepted.push(generated_event_id),
            Err(error) if is_retryable_sqlite_error(&error) => {
                return (StatusCode::SERVICE_UNAVAILABLE, BatchResponse::default())
            }
            Err(error) => response.rejected.push(RejectedEvent {
                event_id: generated_event_id,
                reason: error.to_string(),
            }),
        }
    }
    if let Err(error) = tx.commit() {
        return (database_error_status(&error), BatchResponse::default());
    }
    (StatusCode::OK, response)
}

fn process_rollup(
    db: &Arc<Mutex<Connection>>,
    node_id: String,
    snapshot: RollupSnapshot,
) -> StatusCode {
    let Ok(db) = db.lock() else {
        return StatusCode::INTERNAL_SERVER_ERROR;
    };
    let snapshot_key = rollup_key(
        &node_id,
        &snapshot.date,
        &snapshot.app_type,
        &snapshot.provider_id,
        &snapshot.model,
        &snapshot.request_model,
        &snapshot.pricing_model,
    );
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
            snapshot_key,
            node_id,
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
    node_id: String,
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
                &node_id,
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
    let Some(node_id) = authenticated_node(&headers, &state.db) else {
        return StatusCode::UNAUTHORIZED;
    };
    let (response_tx, response_rx) = oneshot::channel();
    let task = WriteTask::Rollup {
        node_id,
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
    let Some(node_id) = authenticated_node(&headers, &state.db) else {
        return StatusCode::UNAUTHORIZED;
    };
    if snapshot.schema_version != SCHEMA_VERSION
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
        node_id,
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
    if !authorized_any_node(&headers, &state.db) {
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

async fn v1_upgrade_required() -> impl IntoResponse {
    (
        StatusCode::UPGRADE_REQUIRED,
        Json(serde_json::json!({
            "error": "telemetry protocol v2 is required"
        })),
    )
}

pub fn router(state: ServerState) -> Router {
    Router::new()
        .route("/healthz", get(health))
        .route("/v1/events/batch", post(v1_upgrade_required))
        .route("/v1/rollups/snapshot", post(v1_upgrade_required))
        .route("/v1/providers/snapshot", post(v1_upgrade_required))
        .route("/v1/usage/summary", get(v1_upgrade_required))
        .merge(dashboard::routes())
        .merge(admin::routes())
        .merge(quota::ingest_routes())
        .merge(sync_v2::routes())
        .with_state(state)
}

pub async fn serve(
    db_path: PathBuf,
    listen: SocketAddr,
    admin_password: Option<String>,
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
        admin_password,
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
    fn rebuild_v2_preserves_auth_metadata_but_not_usage() {
        let directory = tempfile::tempdir().unwrap();
        let source_path = directory.path().join("legacy.db");
        let target_path = directory.path().join("v2.db");
        let source = init_db(&source_path).unwrap();
        let (node, _token) = nodes::create(&source, "node-a").unwrap();
        let token_hash: String = source
            .query_row(
                "SELECT token_hash FROM nodes WHERE uuid=?1",
                [&node.uuid],
                |row| row.get(0),
            )
            .unwrap();
        source
            .execute(
                "INSERT INTO provider_catalog(node_id,app_type,provider_id,name,updated_at)
                 VALUES (?1,'codex','provider-a','Provider A',10)",
                [&node.uuid],
            )
            .unwrap();
        source
            .execute(
                "INSERT INTO usage_events (
                   event_id,node_id,request_id,created_at,app_type,provider_id,model,
                   input_tokens,output_tokens,cache_read_tokens,cache_creation_tokens,
                   total_cost_usd,latency_ms,status_code,is_streaming,received_at
                 ) VALUES ('event-a',?1,'request-a',10,'codex','provider-a','gpt-5',
                           1,1,0,0,'0',1,200,1,10)",
                [&node.uuid],
            )
            .unwrap();
        drop(source);

        let summary = rebuild_v2_metadata(&source_path, &target_path).unwrap();
        assert_eq!(
            summary,
            RebuildV2Summary {
                nodes: 1,
                providers: 1
            }
        );
        let target = Connection::open(&target_path).unwrap();
        let copied_hash: String = target
            .query_row(
                "SELECT token_hash FROM nodes WHERE uuid=?1",
                [&node.uuid],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(copied_hash, token_hash);
        assert_eq!(
            target
                .query_row("SELECT COUNT(*) FROM provider_catalog", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            target
                .query_row("SELECT COUNT(*) FROM usage_events", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert!(rebuild_v2_metadata(&source_path, &target_path).is_err());
    }

    #[test]
    fn provider_snapshot_upserts_current_name() {
        let db = Arc::new(Mutex::new(init_db(":memory:").unwrap()));
        let snapshot = ProviderSnapshot {
            schema_version: SCHEMA_VERSION,
            providers: vec![telemetry_core::ProviderEntry {
                app_type: "codex".into(),
                provider_id: "provider-a".into(),
                name: "DeepSeek".into(),
            }],
        };
        assert_eq!(
            process_provider_snapshot(&db, "node-a".into(), snapshot),
            StatusCode::NO_CONTENT
        );
        let renamed = ProviderSnapshot {
            schema_version: SCHEMA_VERSION,
            providers: vec![telemetry_core::ProviderEntry {
                app_type: "codex".into(),
                provider_id: "provider-a".into(),
                name: "DeepSeek Renamed".into(),
            }],
        };
        assert_eq!(
            process_provider_snapshot(&db, "node-a".into(), renamed),
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
    async fn v1_ingest_endpoints_require_protocol_upgrade() {
        let db = init_db(":memory:").unwrap();
        let state = ServerState::new(db, PathBuf::from("telemetry.db"), None);
        for endpoint in [
            "/v1/events/batch",
            "/v1/rollups/snapshot",
            "/v1/providers/snapshot",
        ] {
            let response = router(state.clone())
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(endpoint)
                        .header("content-type", "application/json")
                        .body(Body::from("{}"))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UPGRADE_REQUIRED);
        }
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

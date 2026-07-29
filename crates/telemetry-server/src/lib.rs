mod dashboard;

use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use telemetry_core::{BatchResponse, EventBatch, RejectedEvent, RollupSnapshot, SCHEMA_VERSION};

#[derive(Clone)]
pub struct ServerState {
    pub db: Arc<Mutex<Connection>>,
    pub db_path: PathBuf,
    pub auth_token: Option<String>,
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
    let mut response = BatchResponse::default();
    let now = chrono::Utc::now().timestamp();
    let Ok(db) = state.db.lock() else {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(response));
    };
    let tx = match db.unchecked_transaction() {
        Ok(tx) => tx,
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(response)),
    };
    for event in &batch.events {
        let exists: Option<String> = tx.query_row("SELECT event_id FROM usage_events WHERE event_id = ?1 OR (node_id = ?2 AND request_id = ?3)", params![event.event_id, event.node_id, event.request_id], |r| r.get(0)).optional().unwrap_or(None);
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
            Err(error) => response.rejected.push(RejectedEvent {
                event_id: event.event_id.clone(),
                reason: error.to_string(),
            }),
        }
    }
    if tx.commit().is_err() {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(BatchResponse::default()),
        );
    }
    (StatusCode::OK, Json(response))
}

pub async fn ingest_rollup(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(snapshot): Json<RollupSnapshot>,
) -> impl IntoResponse {
    if !authorized(&headers, &state.auth_token) {
        return StatusCode::UNAUTHORIZED;
    }
    let Ok(db) = state.db.lock() else {
        return StatusCode::INTERNAL_SERVER_ERROR;
    };
    let result = db.execute("INSERT INTO usage_daily_snapshots (snapshot_key,node_id,date,app_type,provider_id,model,request_model,pricing_model,request_count,success_count,input_tokens,output_tokens,cache_read_tokens,cache_creation_tokens,total_cost_usd,avg_latency_ms,received_at) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT(snapshot_key) DO UPDATE SET request_count=excluded.request_count,success_count=excluded.success_count,input_tokens=excluded.input_tokens,output_tokens=excluded.output_tokens,cache_read_tokens=excluded.cache_read_tokens,cache_creation_tokens=excluded.cache_creation_tokens,total_cost_usd=excluded.total_cost_usd,avg_latency_ms=excluded.avg_latency_ms,received_at=excluded.received_at", params![snapshot.snapshot_key,snapshot.node_id,snapshot.date,snapshot.app_type,snapshot.provider_id,snapshot.model,snapshot.request_model,snapshot.pricing_model,snapshot.request_count,snapshot.success_count,snapshot.input_tokens,snapshot.output_tokens,snapshot.cache_read_tokens,snapshot.cache_creation_tokens,snapshot.total_cost_usd,snapshot.avg_latency_ms,chrono::Utc::now().timestamp()]);
    if result.is_ok() {
        StatusCode::OK
    } else {
        StatusCode::INTERNAL_SERVER_ERROR
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
    let state = ServerState {
        db: Arc::new(Mutex::new(init_db(db_path)?)),
        db_path: db_path_for_queries,
        auth_token,
    };
    let listener = tokio::net::TcpListener::bind(listen).await?;
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
    #[test]
    fn schema_initializes() {
        let db = init_db(":memory:").unwrap();
        assert!(db
            .query_row("SELECT 1 FROM usage_events LIMIT 1", [], |_| Ok(()))
            .is_err());
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
}

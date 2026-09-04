use crate::{nodes, ServerState};
use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, patch, post},
    Json, Router,
};
use rusqlite::{Connection, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};
use uuid::Uuid;

const SESSION_COOKIE: &str = "telemetry_admin_session";
const SESSION_TTL: Duration = Duration::from_secs(12 * 60 * 60);
const ADMIN_HTML: &str = include_str!("../web/admin.html");
const ADMIN_CSS: &str = include_str!("../web/admin.css");
const ADMIN_JS: &str = include_str!("../web/admin.js");

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub password: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeNameRequest {
    pub node_name: String,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LogKind {
    Request,
    Quota,
}

#[derive(Debug, Deserialize)]
pub struct LogPreviewQuery {
    pub kind: LogKind,
    pub node_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogPurgeRequest {
    pub kind: LogKind,
    pub node_id: Option<String>,
    pub confirmation: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogOperationResponse {
    pub kind: LogKind,
    pub node_id: Option<String>,
    pub confirmation: String,
    pub counts: BTreeMap<String, u64>,
    pub total_rows: u64,
    pub purged: bool,
    pub warning: &'static str,
}

fn json_error(status: StatusCode, code: &str, message: impl Into<String>) -> Response {
    (
        status,
        Json(serde_json::json!({
            "code": code,
            "message": message.into(),
        })),
    )
        .into_response()
}

fn cookie_value(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(|cookies| {
            cookies.split(';').find_map(|cookie| {
                let (name, value) = cookie.trim().split_once('=')?;
                (name == SESSION_COOKIE).then(|| value.to_owned())
            })
        })
}

fn valid_password(expected: &str, supplied: &str) -> bool {
    let expected = Sha256::digest(expected.as_bytes());
    let supplied = Sha256::digest(supplied.as_bytes());
    expected
        .iter()
        .zip(supplied.iter())
        .fold(0u8, |difference, (left, right)| difference | (left ^ right))
        == 0
}

fn authenticated(state: &ServerState, headers: &HeaderMap) -> bool {
    if state.admin_password.is_none() {
        return false;
    }
    let Some(session) = cookie_value(headers) else {
        return false;
    };
    let Ok(mut sessions) = state.admin_sessions.lock() else {
        return false;
    };
    let Some(expires_at) = sessions.get(&session).copied() else {
        return false;
    };
    if expires_at <= Instant::now() {
        sessions.remove(&session);
        return false;
    }
    true
}

fn require_admin(state: &ServerState, headers: &HeaderMap) -> Result<(), Box<Response>> {
    if state.admin_password.is_none() {
        return Err(Box::new(json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "admin_disabled",
            "ADMIN_PASSWORD is not configured",
        )));
    }
    if authenticated(state, headers) {
        Ok(())
    } else {
        Err(Box::new(json_error(
            StatusCode::UNAUTHORIZED,
            "admin_auth_required",
            "administrator login required",
        )))
    }
}

fn normalize_node_name(name: String) -> Result<String, Box<Response>> {
    let name = name.trim().to_owned();
    if name.is_empty() {
        return Err(Box::new(json_error(
            StatusCode::BAD_REQUEST,
            "invalid_node_name",
            "nodeName cannot be empty",
        )));
    }
    if name.len() > 128 {
        return Err(Box::new(json_error(
            StatusCode::BAD_REQUEST,
            "invalid_node_name",
            "nodeName cannot exceed 128 bytes",
        )));
    }
    Ok(name)
}

fn database_error(error: impl std::fmt::Display) -> Response {
    json_error(
        StatusCode::SERVICE_UNAVAILABLE,
        "database_unavailable",
        error.to_string(),
    )
}

fn normalize_log_node_id(node_id: Option<String>) -> Result<Option<String>, Box<Response>> {
    let Some(node_id) = node_id else {
        return Ok(None);
    };
    let node_id = node_id.trim().to_owned();
    if node_id.is_empty() || node_id.len() > 128 || node_id.chars().any(char::is_control) {
        return Err(Box::new(json_error(
            StatusCode::BAD_REQUEST,
            "invalid_node_id",
            "node_id must identify one existing node",
        )));
    }
    Ok(Some(node_id))
}

fn require_existing_node(
    connection: &Connection,
    node_id: Option<&str>,
) -> Result<(), Box<Response>> {
    let Some(node_id) = node_id else {
        return Ok(());
    };
    let exists = connection
        .query_row("SELECT 1 FROM nodes WHERE uuid=?1", [node_id], |_| Ok(()))
        .optional()
        .map_err(|error| Box::new(database_error(error)))?
        .is_some();
    if exists {
        Ok(())
    } else {
        Err(Box::new(json_error(
            StatusCode::NOT_FOUND,
            "node_not_found",
            "node not found",
        )))
    }
}

fn log_confirmation(kind: LogKind, node_id: Option<&str>) -> String {
    let kind = match kind {
        LogKind::Request => "REQUEST",
        LogKind::Quota => "QUOTA",
    };
    format!("DELETE {kind} LOGS {}", node_id.unwrap_or("ALL"))
}

fn log_warning(kind: LogKind) -> &'static str {
    match kind {
        LogKind::Request => {
            "Server deletion does not change the client baseline. Deleted rows return only after a deliberate full re-upload; active clients can still upload new logs."
        }
        LogKind::Quota => {
            "Quota deletion is server-local. Active clients can publish new samples and current provider state again."
        }
    }
}

fn direct_count(
    connection: &Connection,
    table: &str,
    node_id: Option<&str>,
) -> rusqlite::Result<u64> {
    let sql = if node_id.is_some() {
        format!("SELECT COUNT(*) FROM {table} WHERE node_id=?1")
    } else {
        format!("SELECT COUNT(*) FROM {table}")
    };
    match node_id {
        Some(node_id) => connection.query_row(&sql, [node_id], |row| row.get(0)),
        None => connection.query_row(&sql, [], |row| row.get(0)),
    }
}

fn staged_count(
    connection: &Connection,
    table: &str,
    node_id: Option<&str>,
) -> rusqlite::Result<u64> {
    let sql = if node_id.is_some() {
        format!(
            "SELECT COUNT(*) FROM {table} s
             JOIN sync_generations g ON g.generation_id=s.generation_id
             WHERE g.node_id=?1"
        )
    } else {
        format!("SELECT COUNT(*) FROM {table}")
    };
    match node_id {
        Some(node_id) => connection.query_row(&sql, [node_id], |row| row.get(0)),
        None => connection.query_row(&sql, [], |row| row.get(0)),
    }
}

fn collect_log_counts(
    connection: &Connection,
    kind: LogKind,
    node_id: Option<&str>,
) -> rusqlite::Result<BTreeMap<String, u64>> {
    let mut counts = BTreeMap::new();
    match kind {
        LogKind::Request => {
            for table in [
                "usage_events",
                "usage_daily_snapshots",
                "usage_hourly_cache",
                "usage_cache_partitions",
                "sync_generations",
                "ingest_batches",
            ] {
                counts.insert(table.to_owned(), direct_count(connection, table, node_id)?);
            }
            for table in [
                "staged_event_mutations",
                "staged_rollup_mutations",
                "staged_providers",
            ] {
                counts.insert(table.to_owned(), staged_count(connection, table, node_id)?);
            }
        }
        LogKind::Quota => {
            for table in [
                "quota_metrics",
                "quota_observations",
                "quota_provider_states",
            ] {
                counts.insert(table.to_owned(), direct_count(connection, table, node_id)?);
            }
        }
    }
    Ok(counts)
}

fn delete_direct(
    transaction: &Transaction<'_>,
    table: &str,
    node_id: Option<&str>,
) -> rusqlite::Result<u64> {
    let sql = if node_id.is_some() {
        format!("DELETE FROM {table} WHERE node_id=?1")
    } else {
        format!("DELETE FROM {table}")
    };
    match node_id {
        Some(node_id) => transaction.execute(&sql, [node_id]),
        None => transaction.execute(&sql, []),
    }
    .map(|count| count as u64)
}

fn delete_staged(
    transaction: &Transaction<'_>,
    table: &str,
    node_id: Option<&str>,
) -> rusqlite::Result<u64> {
    let sql = if node_id.is_some() {
        format!(
            "DELETE FROM {table}
             WHERE generation_id IN (
               SELECT generation_id FROM sync_generations WHERE node_id=?1
             )"
        )
    } else {
        format!("DELETE FROM {table}")
    };
    match node_id {
        Some(node_id) => transaction.execute(&sql, [node_id]),
        None => transaction.execute(&sql, []),
    }
    .map(|count| count as u64)
}

fn purge_logs(
    connection: &mut Connection,
    kind: LogKind,
    node_id: Option<&str>,
) -> rusqlite::Result<BTreeMap<String, u64>> {
    let transaction = connection.transaction()?;
    let mut counts = BTreeMap::new();
    match kind {
        LogKind::Request => {
            for table in [
                "staged_event_mutations",
                "staged_rollup_mutations",
                "staged_providers",
            ] {
                counts.insert(
                    table.to_owned(),
                    delete_staged(&transaction, table, node_id)?,
                );
            }
            for table in [
                "sync_generations",
                "usage_hourly_cache",
                "usage_cache_partitions",
                "usage_events",
                "usage_daily_snapshots",
                "ingest_batches",
            ] {
                counts.insert(
                    table.to_owned(),
                    delete_direct(&transaction, table, node_id)?,
                );
            }
        }
        LogKind::Quota => {
            for table in [
                "quota_metrics",
                "quota_observations",
                "quota_provider_states",
            ] {
                counts.insert(
                    table.to_owned(),
                    delete_direct(&transaction, table, node_id)?,
                );
            }
        }
    }
    transaction.commit()?;
    Ok(counts)
}

fn log_response(
    kind: LogKind,
    node_id: Option<String>,
    counts: BTreeMap<String, u64>,
    purged: bool,
) -> LogOperationResponse {
    let total_rows = counts.values().sum();
    LogOperationResponse {
        kind,
        confirmation: log_confirmation(kind, node_id.as_deref()),
        node_id,
        counts,
        total_rows,
        purged,
        warning: log_warning(kind),
    }
}

async fn index() -> Html<&'static str> {
    Html(ADMIN_HTML)
}

async fn styles() -> Response {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        ADMIN_CSS,
    )
        .into_response()
}

async fn script() -> Response {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        ADMIN_JS,
    )
        .into_response()
}

async fn login(State(state): State<ServerState>, Json(payload): Json<LoginRequest>) -> Response {
    let Some(expected) = state.admin_password.as_deref() else {
        return json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "admin_disabled",
            "ADMIN_PASSWORD is not configured",
        );
    };
    if !valid_password(expected, &payload.password) {
        return json_error(
            StatusCode::UNAUTHORIZED,
            "invalid_password",
            "invalid administrator password",
        );
    }
    let session = format!("as_{}", Uuid::new_v4().simple());
    if let Ok(mut sessions) = state.admin_sessions.lock() {
        sessions.insert(session.clone(), Instant::now() + SESSION_TTL);
    } else {
        return json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "session_unavailable",
            "administrator session store is unavailable",
        );
    }
    let cookie = format!(
        "{SESSION_COOKIE}={session}; Path=/admin; Max-Age={}; HttpOnly; SameSite=Lax",
        SESSION_TTL.as_secs()
    );
    let mut response = Json(serde_json::json!({"ok": true})).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&cookie).expect("session cookie is valid"),
    );
    response
}

async fn logout(State(state): State<ServerState>, headers: HeaderMap) -> Response {
    if let Some(session) = cookie_value(&headers) {
        if let Ok(mut sessions) = state.admin_sessions.lock() {
            sessions.remove(&session);
        }
    }
    let mut response = Json(serde_json::json!({"ok": true})).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_static(
            "telemetry_admin_session=; Path=/admin; Max-Age=0; HttpOnly; SameSite=Lax",
        ),
    );
    response
}

async fn session(State(state): State<ServerState>, headers: HeaderMap) -> Response {
    if state.admin_password.is_none() {
        return json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "admin_disabled",
            "ADMIN_PASSWORD is not configured",
        );
    }
    if authenticated(&state, &headers) {
        Json(serde_json::json!({"authenticated": true, "adminEnabled": true})).into_response()
    } else {
        json_error(
            StatusCode::UNAUTHORIZED,
            "admin_auth_required",
            "administrator login required",
        )
    }
}

async fn list_nodes(State(state): State<ServerState>, headers: HeaderMap) -> Response {
    if let Err(response) = require_admin(&state, &headers) {
        return *response;
    }
    let Ok(db) = state.db.lock() else {
        return database_error("database lock unavailable");
    };
    match nodes::list(&db) {
        Ok(nodes) => Json(nodes).into_response(),
        Err(error) => database_error(error),
    }
}

async fn create_node(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(payload): Json<NodeNameRequest>,
) -> Response {
    if let Err(response) = require_admin(&state, &headers) {
        return *response;
    }
    let node_name = match normalize_node_name(payload.node_name) {
        Ok(value) => value,
        Err(response) => return *response,
    };
    let Ok(db) = state.db.lock() else {
        return database_error("database lock unavailable");
    };
    match nodes::create(&db, &node_name) {
        Ok((node, token)) => Json(serde_json::json!({
            "node": node,
            "token": token,
            "tokenShownOnce": true,
        }))
        .into_response(),
        Err(error) => json_error(
            StatusCode::CONFLICT,
            "node_create_failed",
            error.to_string(),
        ),
    }
}

async fn rename_node(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Path(uuid): Path<String>,
    Json(payload): Json<NodeNameRequest>,
) -> Response {
    if let Err(response) = require_admin(&state, &headers) {
        return *response;
    }
    let node_name = match normalize_node_name(payload.node_name) {
        Ok(value) => value,
        Err(response) => return *response,
    };
    let Ok(db) = state.db.lock() else {
        return database_error("database lock unavailable");
    };
    match nodes::rename(&db, &uuid, &node_name) {
        Ok(Some(node)) => Json(node).into_response(),
        Ok(None) => json_error(StatusCode::NOT_FOUND, "node_not_found", "node not found"),
        Err(error) => json_error(
            StatusCode::CONFLICT,
            "node_rename_failed",
            error.to_string(),
        ),
    }
}

async fn regenerate_token(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Path(uuid): Path<String>,
) -> Response {
    if let Err(response) = require_admin(&state, &headers) {
        return *response;
    }
    let Ok(db) = state.db.lock() else {
        return database_error("database lock unavailable");
    };
    match nodes::regenerate_token(&db, &uuid) {
        Ok(Some((node, token))) => Json(serde_json::json!({
            "node": node,
            "token": token,
            "tokenShownOnce": true,
        }))
        .into_response(),
        Ok(None) => json_error(StatusCode::NOT_FOUND, "node_not_found", "node not found"),
        Err(error) => database_error(error),
    }
}

async fn revoke_token(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Path(uuid): Path<String>,
) -> Response {
    if let Err(response) = require_admin(&state, &headers) {
        return *response;
    }
    let Ok(db) = state.db.lock() else {
        return database_error("database lock unavailable");
    };
    match nodes::revoke_token(&db, &uuid) {
        Ok(Some(node)) => Json(node).into_response(),
        Ok(None) => json_error(StatusCode::NOT_FOUND, "node_not_found", "node not found"),
        Err(error) => database_error(error),
    }
}

async fn preview_logs(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Query(query): Query<LogPreviewQuery>,
) -> Response {
    if let Err(response) = require_admin(&state, &headers) {
        return *response;
    }
    let node_id = match normalize_log_node_id(query.node_id) {
        Ok(value) => value,
        Err(response) => return *response,
    };
    let Ok(db) = state.db.lock() else {
        return database_error("database lock unavailable");
    };
    if let Err(response) = require_existing_node(&db, node_id.as_deref()) {
        return *response;
    }
    match collect_log_counts(&db, query.kind, node_id.as_deref()) {
        Ok(counts) => Json(log_response(query.kind, node_id, counts, false)).into_response(),
        Err(error) => database_error(error),
    }
}

async fn purge_logs_handler(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(payload): Json<LogPurgeRequest>,
) -> Response {
    if let Err(response) = require_admin(&state, &headers) {
        return *response;
    }
    let node_id = match normalize_log_node_id(payload.node_id) {
        Ok(value) => value,
        Err(response) => return *response,
    };
    let expected = log_confirmation(payload.kind, node_id.as_deref());
    if payload.confirmation != expected {
        return json_error(
            StatusCode::BAD_REQUEST,
            "confirmation_mismatch",
            format!("confirmation must exactly match: {expected}"),
        );
    }
    let Ok(mut db) = state.db.lock() else {
        return database_error("database lock unavailable");
    };
    if let Err(response) = require_existing_node(&db, node_id.as_deref()) {
        return *response;
    }
    match purge_logs(&mut db, payload.kind, node_id.as_deref()) {
        Ok(counts) => Json(log_response(payload.kind, node_id, counts, true)).into_response(),
        Err(error) => database_error(error),
    }
}

pub fn routes() -> Router<ServerState> {
    Router::new()
        .route("/admin", get(index))
        .route("/admin/", get(index))
        .route("/admin/styles.css", get(styles))
        .route("/admin/app.js", get(script))
        .route("/admin/login", post(login))
        .route("/admin/logout", post(logout))
        .route("/admin/api/session", get(session))
        .route("/admin/api/nodes", get(list_nodes).post(create_node))
        .route("/admin/api/nodes/:uuid", patch(rename_node))
        .route(
            "/admin/api/nodes/:uuid/token",
            post(regenerate_token).delete(revoke_token),
        )
        .route("/admin/api/logs/preview", get(preview_logs))
        .route("/admin/api/logs/purge", post(purge_logs_handler))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{init_db, nodes, router};
    use axum::{body::Body, http::Request};
    use std::path::PathBuf;
    use tower::ServiceExt;

    async fn response_json(response: Response) -> serde_json::Value {
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    fn json_body(value: serde_json::Value) -> Body {
        Body::from(serde_json::to_vec(&value).unwrap())
    }

    fn seed_logs(connection: &Connection, node_id: &str, suffix: &str) {
        connection
            .execute(
                "INSERT INTO usage_events (
                   event_id,node_id,request_id,created_at,app_type,provider_id,model,
                   request_model,pricing_model,input_tokens,output_tokens,cache_read_tokens,
                   cache_creation_tokens,input_token_semantics,total_cost_usd,latency_ms,
                   status_code,is_streaming,data_source,content_hash,received_at
                 ) VALUES (?1,?2,?3,100,'codex','provider','model','','',1,2,3,4,2,
                           '0.1',5,200,0,'proxy','hash',100)",
                rusqlite::params![
                    format!("{node_id}:codex:req-{suffix}"),
                    node_id,
                    format!("req-{suffix}")
                ],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO usage_daily_snapshots (
                   snapshot_key,node_id,date,app_type,provider_id,model,request_model,
                   pricing_model,request_count,success_count,input_tokens,output_tokens,
                   cache_read_tokens,cache_creation_tokens,input_token_semantics,total_cost_usd,
                   avg_latency_ms,day_start_utc,day_end_utc,content_hash,received_at
                 ) VALUES (?1,?2,'1970-01-01','codex','provider','model','','',1,1,1,2,3,4,
                           2,'0.1',5,0,86400,'hash',100)",
                rusqlite::params![format!("snapshot-{suffix}"), node_id],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO provider_catalog(node_id,app_type,provider_id,name,updated_at)
                 VALUES (?1,'codex','provider',?2,100)",
                rusqlite::params![node_id, format!("Provider {suffix}")],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO usage_cache_partitions(node_id,hour_start,state,updated_at)
                 VALUES (?1,0,'clean',100)",
                [node_id],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO usage_hourly_cache (
                   node_id,hour_start,app_type,provider_app_type,provider_id,model,
                   request_model,pricing_model,data_source,request_count,success_count,
                   input_tokens,output_tokens,cache_read_tokens,cache_creation_tokens,
                   total_cost_usd,latency_total_ms,first_event_at,last_event_at
                 ) VALUES (?1,0,'codex','codex','provider','model','','','proxy',1,1,
                           1,2,3,4,'0.1',5,100,100)",
                [node_id],
            )
            .unwrap();
        let generation_id = format!("generation-{suffix}");
        connection
            .execute(
                "INSERT INTO sync_generations (
                   generation_id,node_id,replace_all,status,created_at
                 ) VALUES (?1,?2,0,'open',100)",
                rusqlite::params![generation_id, node_id],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO staged_event_mutations (
                   generation_id,app_type,request_id,operation,content_hash,payload_json
                 ) VALUES (?1,'codex',?2,'delete','hash',NULL)",
                rusqlite::params![generation_id, format!("staged-{suffix}")],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO staged_rollup_mutations (
                   generation_id,snapshot_key,operation,content_hash,payload_json
                 ) VALUES (?1,?2,'delete','hash',NULL)",
                rusqlite::params![generation_id, format!("staged-rollup-{suffix}")],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO staged_providers(generation_id,app_type,provider_id,name)
                 VALUES (?1,'codex','provider','Provider')",
                [generation_id.as_str()],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO ingest_batches(batch_id,node_id,received_at,event_count)
                 VALUES (?1,?2,100,1)",
                rusqlite::params![format!("batch-{suffix}"), node_id],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO quota_provider_states (
                   node_id,app_type,provider_id,provider_name,status,target_kind,
                   checked_at,last_success_at,received_at
                 ) VALUES (?1,'codex','provider','Provider','ok',NULL,100,100,100)",
                [node_id],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO quota_observations (
                   node_id,observation_id,content_hash,app_type,provider_id,sampled_at,received_at
                 ) VALUES (?1,?2,'hash','codex','provider',100,100)",
                rusqlite::params![node_id, format!("observation-{suffix}")],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO quota_metrics (
                   node_id,observation_id,metric_key,metric_label,metric_kind,
                   utilization_percent,used,remaining,total,unit,resets_at
                 ) VALUES (?1,?2,'five_hour','5h','utilizationPercent',50,NULL,NULL,NULL,NULL,NULL)",
                rusqlite::params![node_id, format!("observation-{suffix}")],
            )
            .unwrap();
    }

    #[tokio::test]
    async fn log_purge_is_authenticated_typed_scoped_and_confirmed() {
        let state = ServerState::new(
            init_db(":memory:").unwrap(),
            PathBuf::from("telemetry.db"),
            Some("unit-test-admin".to_owned()),
        );
        let (node_a, _) = nodes::create(&state.db.lock().unwrap(), "LB13").unwrap();
        let (node_b, _) = nodes::create(&state.db.lock().unwrap(), "LB16").unwrap();
        {
            let connection = state.db.lock().unwrap();
            seed_logs(&connection, &node_a.uuid, "a");
            seed_logs(&connection, &node_b.uuid, "b");
        }
        state.admin_sessions.lock().unwrap().insert(
            "purge-test".to_owned(),
            Instant::now() + Duration::from_secs(60),
        );
        let cookie = "telemetry_admin_session=purge-test";
        let app = router(state.clone());

        let unauthorized = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/admin/api/logs/preview?kind=request&node_id={}",
                        node_a.uuid
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

        let preview = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/admin/api/logs/preview?kind=request&node_id={}",
                        node_a.uuid
                    ))
                    .header(header::COOKIE, cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(preview.status(), StatusCode::OK);
        let preview = response_json(preview).await;
        assert_eq!(preview["totalRows"], 9);
        assert_eq!(preview["counts"]["usage_events"], 1);
        let confirmation = preview["confirmation"].as_str().unwrap().to_owned();

        let rejected = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/api/logs/purge")
                    .header(header::COOKIE, cookie)
                    .header("content-type", "application/json")
                    .body(json_body(serde_json::json!({
                        "kind": "request",
                        "nodeId": node_a.uuid,
                        "confirmation": "DELETE REQUEST LOGS"
                    })))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);

        let purged = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/api/logs/purge")
                    .header(header::COOKIE, cookie)
                    .header("content-type", "application/json")
                    .body(json_body(serde_json::json!({
                        "kind": "request",
                        "nodeId": node_a.uuid,
                        "confirmation": confirmation
                    })))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(purged.status(), StatusCode::OK);
        let purged = response_json(purged).await;
        assert_eq!(purged["totalRows"], 9);
        assert_eq!(purged["purged"], true);
        {
            let connection = state.db.lock().unwrap();
            assert_eq!(
                direct_count(&connection, "usage_events", Some(&node_a.uuid)).unwrap(),
                0
            );
            assert_eq!(
                direct_count(&connection, "usage_events", Some(&node_b.uuid)).unwrap(),
                1
            );
            assert_eq!(
                direct_count(&connection, "quota_observations", Some(&node_a.uuid)).unwrap(),
                1
            );
            assert_eq!(
                direct_count(&connection, "provider_catalog", Some(&node_a.uuid)).unwrap(),
                1
            );
            assert!(nodes::list(&connection)
                .unwrap()
                .iter()
                .any(|node| node.uuid == node_a.uuid));
        }

        let quota_purged = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/api/logs/purge")
                    .header(header::COOKIE, cookie)
                    .header("content-type", "application/json")
                    .body(json_body(serde_json::json!({
                        "kind": "quota",
                        "confirmation": "DELETE QUOTA LOGS ALL"
                    })))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(quota_purged.status(), StatusCode::OK);
        let quota_purged = response_json(quota_purged).await;
        assert_eq!(quota_purged["totalRows"], 6);
        let connection = state.db.lock().unwrap();
        assert_eq!(
            direct_count(&connection, "quota_observations", None).unwrap(),
            0
        );
        assert_eq!(
            direct_count(&connection, "usage_events", Some(&node_b.uuid)).unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn admin_login_and_node_token_lifecycle() {
        let state = ServerState::new(
            init_db(":memory:").unwrap(),
            PathBuf::from("telemetry.db"),
            Some("unit-test-admin".to_owned()),
        );
        let app = router(state.clone());

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/login")
                    .header("content-type", "application/json")
                    .body(json_body(serde_json::json!({"password":"wrong"})))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/login")
                    .header("content-type", "application/json")
                    .body(json_body(serde_json::json!({"password":"unit-test-admin"})))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let cookie = response
            .headers()
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/api/nodes")
                    .header(header::COOKIE, &cookie)
                    .header("content-type", "application/json")
                    .body(json_body(serde_json::json!({"nodeName":"test-node"})))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let created = response_json(response).await;
        let uuid = created["node"]["uuid"].as_str().unwrap().to_owned();
        let token = created["token"].as_str().unwrap().to_owned();
        assert!(created["tokenShownOnce"].as_bool().unwrap());
        assert!(nodes::authorized_node(
            &state.db.lock().unwrap(),
            &uuid,
            &token
        ));

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri(format!("/admin/api/nodes/{uuid}"))
                    .header(header::COOKIE, &cookie)
                    .header("content-type", "application/json")
                    .body(json_body(serde_json::json!({"nodeName":"renamed-node"})))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/admin/api/nodes/{uuid}/token"))
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let regenerated = response_json(response).await;
        let replacement = regenerated["token"].as_str().unwrap();
        assert!(!nodes::authorized_node(
            &state.db.lock().unwrap(),
            &uuid,
            &token
        ));
        assert!(nodes::authorized_node(
            &state.db.lock().unwrap(),
            &uuid,
            replacement
        ));

        let response = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/admin/api/nodes/{uuid}/token"))
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(!nodes::authorized_node(
            &state.db.lock().unwrap(),
            &uuid,
            replacement
        ));
    }
}

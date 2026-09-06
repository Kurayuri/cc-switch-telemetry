use crate::{authenticated_node, ServerState};
use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use rusqlite::{params, params_from_iter, types::Value, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use telemetry_core::{
    QuotaBatchResponse, QuotaMetric, QuotaMetricKind, QuotaObservation, QuotaProviderState,
    QuotaProviderStatus, QuotaTargetKind, QuotaUploadBatch, SCHEMA_VERSION,
};
use uuid::Uuid;

const MAX_PROVIDER_STATES: usize = 1_000;
const MAX_OBSERVATIONS: usize = 2_000;
const MAX_METRICS: usize = 10_000;
const MAX_LABEL_BYTES: usize = 256;
const TARGET_POINTS_PER_SERIES: i64 = 2_000;

pub fn ensure_schema(connection: &Connection) -> anyhow::Result<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS quota_provider_states (
             node_id TEXT NOT NULL,
             app_type TEXT NOT NULL,
             provider_id TEXT NOT NULL,
             provider_name TEXT NOT NULL,
             status TEXT NOT NULL,
             target_kind TEXT,
             checked_at INTEGER NOT NULL,
             last_success_at INTEGER,
             received_at INTEGER NOT NULL,
             PRIMARY KEY (node_id, app_type, provider_id),
             FOREIGN KEY (node_id) REFERENCES nodes(uuid) ON DELETE CASCADE
         );
         CREATE INDEX IF NOT EXISTS idx_quota_provider_state_node
             ON quota_provider_states(node_id, provider_id, checked_at);
         CREATE TABLE IF NOT EXISTS quota_observations (
             node_id TEXT NOT NULL,
             observation_id TEXT NOT NULL,
             content_hash TEXT NOT NULL,
             app_type TEXT NOT NULL,
             provider_id TEXT NOT NULL,
             sampled_at INTEGER NOT NULL,
             received_at INTEGER NOT NULL,
             PRIMARY KEY (node_id, observation_id),
             FOREIGN KEY (node_id) REFERENCES nodes(uuid) ON DELETE CASCADE
         );
         CREATE INDEX IF NOT EXISTS idx_quota_observation_series_time
             ON quota_observations(node_id, provider_id, sampled_at);
         CREATE INDEX IF NOT EXISTS idx_quota_observation_time
             ON quota_observations(sampled_at);
         CREATE TABLE IF NOT EXISTS quota_metrics (
             node_id TEXT NOT NULL,
             observation_id TEXT NOT NULL,
             metric_key TEXT NOT NULL,
             metric_label TEXT NOT NULL,
             metric_kind TEXT NOT NULL,
             utilization_percent REAL,
             used REAL,
             remaining REAL,
             total REAL,
             unit TEXT,
             resets_at INTEGER,
             PRIMARY KEY (node_id, observation_id, metric_key),
             FOREIGN KEY (node_id, observation_id)
                 REFERENCES quota_observations(node_id, observation_id) ON DELETE CASCADE
         );
         CREATE INDEX IF NOT EXISTS idx_quota_metric_series
             ON quota_metrics(node_id, metric_key, observation_id);",
    )?;
    Ok(())
}

fn error(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(serde_json::json!({ "code": code, "message": message })),
    )
        .into_response()
}

fn status_text(status: &QuotaProviderStatus) -> &'static str {
    match status {
        QuotaProviderStatus::Ok => "ok",
        QuotaProviderStatus::NotAvailable => "not_available",
        QuotaProviderStatus::CredentialParseFailed => "credential_parse_failed",
        QuotaProviderStatus::LoginExpired => "login_expired",
        QuotaProviderStatus::QueryFailed => "query_failed",
        QuotaProviderStatus::CommandFailed => "command_failed",
        QuotaProviderStatus::TimedOut => "timed_out",
        QuotaProviderStatus::InvalidOutput => "invalid_output",
    }
}

fn target_text(target: &QuotaTargetKind) -> &'static str {
    match target {
        QuotaTargetKind::SubscriptionTool => "subscriptionTool",
        QuotaTargetKind::CodexOAuth => "codexOAuth",
        QuotaTargetKind::UsageScript => "usageScript",
    }
}

fn metric_kind_text(kind: &QuotaMetricKind) -> &'static str {
    match kind {
        QuotaMetricKind::UtilizationPercent => "utilizationPercent",
        QuotaMetricKind::Balance => "balance",
    }
}

fn valid_text(value: &str, max: usize) -> bool {
    let value = value.trim();
    !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}

fn valid_optional_number(value: Option<f64>) -> bool {
    value.is_none_or(f64::is_finite)
}

fn valid_metric(metric: &QuotaMetric) -> bool {
    if !valid_text(&metric.key, 128)
        || !valid_text(&metric.label, MAX_LABEL_BYTES)
        || !valid_optional_number(metric.utilization_percent)
        || !valid_optional_number(metric.used)
        || !valid_optional_number(metric.remaining)
        || !valid_optional_number(metric.total)
        || metric
            .utilization_percent
            .is_some_and(|value| !(0.0..=100.0).contains(&value))
        || metric
            .unit
            .as_deref()
            .is_some_and(|unit| !valid_text(unit, 32))
    {
        return false;
    }
    match metric.kind {
        QuotaMetricKind::UtilizationPercent => metric
            .utilization_percent
            .is_some_and(|value| (0.0..=100.0).contains(&value)),
        QuotaMetricKind::Balance => {
            metric.used.is_some() || metric.remaining.is_some() || metric.total.is_some()
        }
    }
}

fn valid_state(state: &QuotaProviderState) -> bool {
    state.app_type == "codex"
        && valid_text(&state.provider_id, 256)
        && valid_text(&state.provider_name, MAX_LABEL_BYTES)
        && state.checked_at > 0
}

fn valid_observation(observation: &QuotaObservation) -> bool {
    Uuid::parse_str(&observation.observation_id).is_ok()
        && observation.app_type == "codex"
        && valid_text(&observation.provider_id, 256)
        && observation.sampled_at > 0
        && !observation.metrics.is_empty()
        && observation.metrics.iter().all(valid_metric)
        && observation
            .metrics
            .iter()
            .map(|metric| &metric.key)
            .collect::<BTreeSet<_>>()
            .len()
            == observation.metrics.len()
}

fn content_hash(observation: &QuotaObservation) -> Result<String, serde_json::Error> {
    let bytes = serde_json::to_vec(observation)?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn validate_batch(batch: &QuotaUploadBatch) -> Result<(), &'static str> {
    if batch.schema_version != SCHEMA_VERSION {
        return Err("telemetry protocol v3 is required");
    }
    let metric_count = batch
        .observations
        .iter()
        .map(|observation| observation.metrics.len())
        .sum::<usize>();
    if batch.provider_states.len() > MAX_PROVIDER_STATES
        || batch.observations.len() > MAX_OBSERVATIONS
        || metric_count > MAX_METRICS
    {
        return Err("quota batch is too large");
    }
    if batch
        .provider_states
        .iter()
        .any(|state| !valid_state(state))
        || batch
            .observations
            .iter()
            .any(|observation| !valid_observation(observation))
    {
        return Err("invalid quota batch");
    }
    let states = batch
        .provider_states
        .iter()
        .map(|state| (&state.app_type, &state.provider_id))
        .collect::<BTreeSet<_>>();
    if states.len() != batch.provider_states.len() {
        return Err("duplicate quota provider state");
    }
    let observations = batch
        .observations
        .iter()
        .map(|observation| &observation.observation_id)
        .collect::<BTreeSet<_>>();
    if observations.len() != batch.observations.len() {
        return Err("duplicate observation id in batch");
    }
    Ok(())
}

fn store_batch(
    connection: &mut Connection,
    node_id: &str,
    batch: &QuotaUploadBatch,
) -> Result<QuotaBatchResponse, StoreError> {
    let now = chrono::Utc::now().timestamp();
    let transaction = connection.transaction()?;
    let mut response = QuotaBatchResponse::default();

    for state in &batch.provider_states {
        transaction.execute(
            "INSERT INTO quota_provider_states (
                 node_id,app_type,provider_id,provider_name,status,target_kind,
                 checked_at,received_at
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
             ON CONFLICT(node_id,app_type,provider_id) DO UPDATE SET
                 provider_name=excluded.provider_name,
                 status=excluded.status,
                 target_kind=excluded.target_kind,
                 checked_at=excluded.checked_at,
                 received_at=excluded.received_at
             WHERE excluded.checked_at >= quota_provider_states.checked_at",
            params![
                node_id,
                state.app_type,
                state.provider_id,
                state.provider_name,
                status_text(&state.status),
                state.target_kind.as_ref().map(target_text),
                state.checked_at,
                now,
            ],
        )?;
    }
    response.provider_states = batch.provider_states.len();

    for observation in &batch.observations {
        let hash = content_hash(observation).map_err(StoreError::Encode)?;
        let existing = transaction
            .query_row(
                "SELECT content_hash FROM quota_observations
                 WHERE node_id=?1 AND observation_id=?2",
                params![node_id, observation.observation_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if let Some(existing) = existing {
            if existing != hash {
                return Err(StoreError::Conflict);
            }
            response.duplicates.push(observation.observation_id.clone());
            continue;
        }
        let has_provider: bool = transaction.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM quota_provider_states
                 WHERE node_id=?1 AND app_type=?2 AND provider_id=?3
             )",
            params![node_id, observation.app_type, observation.provider_id],
            |row| row.get(0),
        )?;
        if !has_provider {
            return Err(StoreError::MissingProvider);
        }
        transaction.execute(
            "INSERT INTO quota_observations (
                 node_id,observation_id,content_hash,app_type,provider_id,sampled_at,received_at
             ) VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![
                node_id,
                observation.observation_id,
                hash,
                observation.app_type,
                observation.provider_id,
                observation.sampled_at,
                now,
            ],
        )?;
        for metric in &observation.metrics {
            transaction.execute(
                "INSERT INTO quota_metrics (
                     node_id,observation_id,metric_key,metric_label,metric_kind,
                     utilization_percent,used,remaining,total,unit,resets_at
                 ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                params![
                    node_id,
                    observation.observation_id,
                    metric.key,
                    metric.label,
                    metric_kind_text(&metric.kind),
                    metric.utilization_percent,
                    metric.used,
                    metric.remaining,
                    metric.total,
                    metric.unit,
                    metric.resets_at,
                ],
            )?;
        }
        transaction.execute(
            "UPDATE quota_provider_states
             SET last_success_at=MAX(COALESCE(last_success_at,0),?1)
             WHERE node_id=?2 AND app_type=?3 AND provider_id=?4",
            params![
                observation.sampled_at,
                node_id,
                observation.app_type,
                observation.provider_id,
            ],
        )?;
        response.accepted.push(observation.observation_id.clone());
    }
    transaction.commit()?;
    Ok(response)
}

#[derive(Debug)]
enum StoreError {
    Sqlite(rusqlite::Error),
    Encode(serde_json::Error),
    Conflict,
    MissingProvider,
}

impl From<rusqlite::Error> for StoreError {
    fn from(value: rusqlite::Error) -> Self {
        Self::Sqlite(value)
    }
}

pub async fn ingest(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(batch): Json<QuotaUploadBatch>,
) -> Response {
    let Some(node_id) = authenticated_node(&headers, &state.db) else {
        return error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "invalid node token",
        );
    };
    if let Err(message) = validate_batch(&batch) {
        return error(StatusCode::BAD_REQUEST, "invalid_quota_batch", message);
    }
    let Ok(mut connection) = state.db.lock() else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "database_unavailable",
            "database unavailable",
        );
    };
    match store_batch(&mut connection, &node_id, &batch) {
        Ok(response) => (StatusCode::OK, Json(response)).into_response(),
        Err(StoreError::Conflict) => error(
            StatusCode::CONFLICT,
            "observation_conflict",
            "observation id already exists with different content",
        ),
        Err(StoreError::MissingProvider) => error(
            StatusCode::BAD_REQUEST,
            "missing_provider_state",
            "observation has no provider state",
        ),
        Err(StoreError::Sqlite(error_value)) => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "database_unavailable",
            &error_value.to_string(),
        ),
        Err(StoreError::Encode(error_value)) => error(
            StatusCode::BAD_REQUEST,
            "invalid_observation",
            &error_value.to_string(),
        ),
    }
}

pub fn ingest_routes() -> Router<ServerState> {
    Router::new()
        .route("/v3/quota/observations", post(ingest))
        .route("/v2/quota/observations", post(super::upgrade_required))
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct QuotaDashboardQuery {
    pub from: Option<i64>,
    pub to: Option<i64>,
    pub bucket: Option<String>,
    pub node_id: Option<String>,
    pub provider_id: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaDashboardResponse {
    pub range: QuotaRange,
    pub providers: Vec<QuotaProviderView>,
    pub data_scope: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaRange {
    pub from: i64,
    pub to: i64,
    pub bucket: String,
    pub bucket_seconds: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaProviderView {
    pub node_id: String,
    pub node_name: String,
    pub provider_id: String,
    pub provider_name: String,
    pub status: String,
    pub target_kind: Option<String>,
    pub checked_at: i64,
    pub last_success_at: Option<i64>,
    pub current: Vec<QuotaCurrentMetric>,
    pub series: Vec<QuotaSeries>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaCurrentMetric {
    pub key: String,
    pub label: String,
    pub kind: String,
    pub unit: Option<String>,
    pub sampled_at: i64,
    pub utilization_percent: Option<f64>,
    pub used: Option<f64>,
    pub remaining: Option<f64>,
    pub total: Option<f64>,
    pub resets_at: Option<i64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaSeries {
    pub key: String,
    pub label: String,
    pub kind: String,
    pub unit: Option<String>,
    pub points: Vec<QuotaPoint>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaPoint {
    pub segment_id: i64,
    pub sampled_at: i64,
    pub utilization_percent: Option<f64>,
    pub used: Option<f64>,
    pub remaining: Option<f64>,
    pub total: Option<f64>,
    pub resets_at: Option<i64>,
}

#[derive(Debug)]
struct ResolvedQuotaQuery {
    from: i64,
    to: i64,
    bucket_seconds: i64,
    bucket_label: String,
    node_id: Option<String>,
    provider_id: Option<String>,
}

fn normalize_filter(value: Option<String>, name: &str) -> Result<Option<String>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.trim().to_owned();
    if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        return Err(format!("invalid {name}"));
    }
    Ok(Some(value))
}

fn nice_bucket(required: i64) -> i64 {
    [
        60, 300, 900, 1_800, 3_600, 7_200, 21_600, 43_200, 86_400, 604_800, 2_592_000,
    ]
    .into_iter()
    .find(|candidate| *candidate >= required)
    .unwrap_or(required)
}

fn parse_bucket(value: Option<&str>, duration: i64) -> Result<(i64, String), String> {
    let text = value.unwrap_or("auto");
    if text == "auto" {
        let seconds = nice_bucket(
            ((duration + TARGET_POINTS_PER_SERIES - 1) / TARGET_POINTS_PER_SERIES).max(60),
        );
        return Ok((seconds, bucket_label(seconds)));
    }
    let (digits, multiplier) = if let Some(value) = text.strip_suffix("mo") {
        (value, 2_592_000)
    } else if let Some(value) = text.strip_suffix('d') {
        (value, 86_400)
    } else if let Some(value) = text.strip_suffix('h') {
        (value, 3_600)
    } else if let Some(value) = text.strip_suffix('m') {
        (value, 60)
    } else if let Some(value) = text.strip_suffix('s') {
        (value, 1)
    } else {
        return Err("invalid bucket".to_owned());
    };
    let count = digits
        .parse::<i64>()
        .map_err(|_| "invalid bucket".to_owned())?;
    let seconds = count
        .checked_mul(multiplier)
        .filter(|value| *value > 0)
        .ok_or_else(|| "invalid bucket".to_owned())?;
    Ok((seconds, bucket_label(seconds)))
}

fn bucket_label(seconds: i64) -> String {
    for (suffix, unit) in [("mo", 2_592_000), ("d", 86_400), ("h", 3_600), ("m", 60)] {
        if seconds % unit == 0 {
            return format!("{}{}", seconds / unit, suffix);
        }
    }
    format!("{seconds}s")
}

fn resolve_query(query: QuotaDashboardQuery) -> Result<ResolvedQuotaQuery, String> {
    let now = chrono::Utc::now().timestamp();
    let to = query.to.unwrap_or(now);
    let from = query.from.unwrap_or(to - 7 * 86_400);
    if from < 0 || to <= from {
        return Err("invalid quota time range".to_owned());
    }
    let duration = to - from;
    let (bucket_seconds, bucket_label) = parse_bucket(query.bucket.as_deref(), duration)?;
    Ok(ResolvedQuotaQuery {
        from,
        to,
        bucket_seconds,
        bucket_label,
        node_id: normalize_filter(query.node_id, "node_id")?,
        provider_id: normalize_filter(query.provider_id, "provider_id")?,
    })
}

fn public_alias(value: String, fallback: &str) -> String {
    if value.contains('@') || value.chars().any(char::is_control) {
        fallback.to_owned()
    } else {
        value
    }
}

#[derive(Debug, Clone)]
struct MetricRow {
    segment_id: i64,
    node_id: String,
    provider_id: String,
    key: String,
    label: String,
    kind: String,
    unit: Option<String>,
    sampled_at: i64,
    utilization_percent: Option<f64>,
    used: Option<f64>,
    remaining: Option<f64>,
    total: Option<f64>,
    resets_at: Option<i64>,
}

fn metric_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MetricRow> {
    Ok(MetricRow {
        segment_id: row.get(12)?,
        node_id: row.get(0)?,
        provider_id: row.get(1)?,
        key: row.get(2)?,
        label: row.get(3)?,
        kind: row.get(4)?,
        unit: row.get(5)?,
        sampled_at: row.get(6)?,
        utilization_percent: row.get(7)?,
        used: row.get(8)?,
        remaining: row.get(9)?,
        total: row.get(10)?,
        resets_at: row.get(11)?,
    })
}

fn query_metric_rows(
    connection: &Connection,
    query: &ResolvedQuotaQuery,
    current_only: bool,
) -> rusqlite::Result<Vec<MetricRow>> {
    let mut filters = vec!["o.app_type='codex'".to_owned()];
    let mut values = Vec::<Value>::new();
    if !current_only {
        filters.push(format!("o.sampled_at >= ?{}", values.len() + 1));
        values.push(Value::Integer(query.from));
        filters.push(format!("o.sampled_at <= ?{}", values.len() + 1));
        values.push(Value::Integer(query.to));
    }
    if let Some(node_id) = &query.node_id {
        filters.push(format!("o.node_id = ?{}", values.len() + 1));
        values.push(Value::Text(node_id.clone()));
    }
    if let Some(provider_id) = &query.provider_id {
        filters.push(format!("o.provider_id = ?{}", values.len() + 1));
        values.push(Value::Text(provider_id.clone()));
    }
    let identity = "node_id,provider_id,metric_key,metric_kind,COALESCE(unit,'')";
    let raw = format!(
        "SELECT o.node_id,o.provider_id,m.metric_key,m.metric_label,m.metric_kind,m.unit,
                o.sampled_at,m.utilization_percent,m.used,m.remaining,m.total,m.resets_at,
                o.observation_id,
                CASE WHEN m.utilization_percent IS NOT NULL OR
                    (m.total>0 AND (m.used IS NOT NULL OR m.remaining IS NOT NULL))
                    THEN 0 ELSE 1 END AS value_axis
         FROM quota_observations o JOIN quota_metrics m
           ON m.node_id=o.node_id AND m.observation_id=o.observation_id
         WHERE {}",
        filters.join(" AND ")
    );
    let columns = "node_id,provider_id,metric_key,metric_label,metric_kind,unit,sampled_at,
                   utilization_percent,used,remaining,total,resets_at";
    let sql = if current_only {
        format!(
            "WITH raw AS ({raw}), ranked AS (
            SELECT *, ROW_NUMBER() OVER (PARTITION BY {identity}
                ORDER BY sampled_at DESC,observation_id DESC) AS position FROM raw
        ) SELECT {columns},0 AS segment_id FROM ranked WHERE position=1
          ORDER BY node_id,provider_id,metric_key,sampled_at"
        )
    } else {
        format!(
            "WITH raw AS ({raw}) SELECT {columns},0 AS segment_id FROM raw
            ORDER BY {identity},sampled_at,observation_id"
        )
    };
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(values), metric_row)?;
    if current_only {
        return rows.collect();
    }
    retain_history(rows, query.from, query.bucket_seconds)
}

fn same_metric(a: &MetricRow, b: &MetricRow) -> bool {
    a.node_id == b.node_id
        && a.provider_id == b.provider_id
        && a.key == b.key
        && a.kind == b.kind
        && a.unit.as_deref().unwrap_or("") == b.unit.as_deref().unwrap_or("")
}
fn percentage_axis(row: &MetricRow) -> bool {
    row.utilization_percent.is_some()
        || (row.total.is_some_and(|total| total > 0.0)
            && (row.used.is_some() || row.remaining.is_some()))
}
// Ordered input needs only the pending bucket tail, not several SQL window sorts.
fn retain_history(
    rows: impl Iterator<Item = rusqlite::Result<MetricRow>>,
    from: i64,
    bucket_seconds: i64,
) -> rusqlite::Result<Vec<MetricRow>> {
    let mut retained = Vec::new();
    let mut pending: Option<(MetricRow, i64, bool)> = None;
    for result in rows {
        let mut row = result?;
        let bucket = (row.sampled_at - from) / bucket_seconds;
        let mut head = true;
        if let Some((previous, previous_bucket, previous_is_head)) = pending.take() {
            let same = same_metric(&previous, &row);
            let split = !same
                || row.sampled_at - previous.sampled_at > 600
                || percentage_axis(&previous) != percentage_axis(&row);
            row.segment_id = if !same {
                1
            } else {
                previous.segment_id + i64::from(split)
            };
            if (split || bucket != previous_bucket) && !previous_is_head {
                retained.push(previous);
            }
            head = split;
        } else {
            row.segment_id = 1;
        }
        if head {
            retained.push(row.clone());
        }
        pending = Some((row, bucket, head));
    }
    if let Some((tail, _, false)) = pending {
        retained.push(tail);
    }
    Ok(retained)
}

#[derive(Debug, Deserialize)]
pub struct QuotaAtQuery {
    pub at: i64,
}

fn query_at(path: &std::path::Path, at: i64) -> anyhow::Result<serde_json::Value> {
    let connection = Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )?;
    connection.busy_timeout(std::time::Duration::from_secs(5))?;
    let mut statement = connection.prepare("WITH ranked AS (
        SELECT o.node_id,o.provider_id,m.metric_key,m.metric_label,m.metric_kind,m.unit,
               o.sampled_at,m.utilization_percent,m.used,m.remaining,m.total,m.resets_at,
               ROW_NUMBER() OVER (PARTITION BY o.node_id,o.provider_id,m.metric_key,m.metric_kind,COALESCE(m.unit,'')
                 ORDER BY o.sampled_at DESC,o.observation_id DESC) AS position
        FROM quota_observations o JOIN quota_metrics m
          ON m.node_id=o.node_id AND m.observation_id=o.observation_id
        WHERE o.app_type='codex' AND o.sampled_at>=?1 AND o.sampled_at<=?2
    ) SELECT node_id,provider_id,metric_key,metric_label,metric_kind,unit,sampled_at,
             utilization_percent,used,remaining,total,resets_at,0 FROM ranked WHERE position=1")?;
    let metrics = statement.query_map(params![at.saturating_sub(600), at], metric_row)?
        .map(|result| result.map(|row| serde_json::json!({
            "nodeId": row.node_id, "providerId": row.provider_id,
            "key": row.key, "label": public_alias(row.label, "Metric"), "kind": row.kind,
            "unit": row.unit, "sampledAt": row.sampled_at, "utilizationPercent": row.utilization_percent,
            "used": row.used, "remaining": row.remaining, "total": row.total, "resetsAt": row.resets_at
        }))).collect::<Result<Vec<_>, _>>()?;
    Ok(serde_json::json!({"at": at, "metrics": metrics}))
}

pub async fn at(State(state): State<ServerState>, Query(query): Query<QuotaAtQuery>) -> Response {
    if query.at < 0 {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_query",
            "at must be nonnegative",
        );
    }
    match tokio::task::spawn_blocking(move || query_at(&state.db_path, query.at)).await {
        Ok(Ok(result)) => Json(result).into_response(),
        Ok(Err(err)) => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "query_failed",
            &err.to_string(),
        ),
        Err(err) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "query_failed",
            &err.to_string(),
        ),
    }
}

fn query_dashboard(
    path: &std::path::Path,
    query: ResolvedQuotaQuery,
) -> anyhow::Result<QuotaDashboardResponse> {
    let connection = Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )?;
    connection.busy_timeout(std::time::Duration::from_secs(5))?;

    let mut state_sql = "SELECT s.node_id,n.node_name,s.provider_id,s.provider_name,s.status,
                                s.target_kind,s.checked_at,s.last_success_at
                         FROM quota_provider_states s JOIN nodes n ON n.uuid=s.node_id
                         WHERE s.app_type='codex'"
        .to_owned();
    let mut state_values = Vec::<Value>::new();
    if let Some(node_id) = &query.node_id {
        state_sql.push_str(" AND s.node_id=?");
        state_values.push(Value::Text(node_id.clone()));
    }
    if let Some(provider_id) = &query.provider_id {
        state_sql.push_str(" AND s.provider_id=?");
        state_values.push(Value::Text(provider_id.clone()));
    }
    state_sql.push_str(" ORDER BY n.node_name COLLATE NOCASE,s.provider_name COLLATE NOCASE");
    let mut providers = BTreeMap::<(String, String), QuotaProviderView>::new();
    let mut statement = connection.prepare(&state_sql)?;
    let states = statement.query_map(params_from_iter(state_values), |row| {
        let node_id = row.get::<_, String>(0)?;
        let provider_id = row.get::<_, String>(2)?;
        Ok((
            (node_id.clone(), provider_id.clone()),
            QuotaProviderView {
                node_id,
                node_name: public_alias(row.get(1)?, "Node"),
                provider_id,
                provider_name: public_alias(row.get(3)?, "Provider"),
                status: row.get(4)?,
                target_kind: row.get(5)?,
                checked_at: row.get(6)?,
                last_success_at: row.get(7)?,
                current: Vec::new(),
                series: Vec::new(),
            },
        ))
    })?;
    for state in states {
        let (key, provider) = state?;
        providers.insert(key, provider);
    }

    for row in query_metric_rows(&connection, &query, true)? {
        if let Some(provider) = providers.get_mut(&(row.node_id, row.provider_id)) {
            provider.current.push(QuotaCurrentMetric {
                key: row.key,
                label: public_alias(row.label, "Metric"),
                kind: row.kind,
                unit: row.unit,
                sampled_at: row.sampled_at,
                utilization_percent: row.utilization_percent,
                used: row.used,
                remaining: row.remaining,
                total: row.total,
                resets_at: row.resets_at,
            });
        }
    }

    let mut series = BTreeMap::<(String, String, String, String, String), QuotaSeries>::new();
    for row in query_metric_rows(&connection, &query, false)? {
        let unit_key = row.unit.clone().unwrap_or_default();
        let key = (
            row.node_id.clone(),
            row.provider_id.clone(),
            row.key.clone(),
            row.kind.clone(),
            unit_key,
        );
        let target = series.entry(key).or_insert_with(|| QuotaSeries {
            key: row.key,
            label: public_alias(row.label, "Metric"),
            kind: row.kind,
            unit: row.unit,
            points: Vec::new(),
        });
        target.points.push(QuotaPoint {
            segment_id: row.segment_id,
            sampled_at: row.sampled_at,
            utilization_percent: row.utilization_percent,
            used: row.used,
            remaining: row.remaining,
            total: row.total,
            resets_at: row.resets_at,
        });
    }
    for ((node_id, provider_id, _, _, _), item) in series {
        if let Some(provider) = providers.get_mut(&(node_id, provider_id)) {
            provider.series.push(item);
        }
    }

    Ok(QuotaDashboardResponse {
        range: QuotaRange {
            from: query.from,
            to: query.to,
            bucket: query.bucket_label,
            bucket_seconds: query.bucket_seconds,
        },
        providers: providers.into_values().collect(),
        data_scope: "redacted normalized quota only",
    })
}

pub async fn dashboard(
    State(state): State<ServerState>,
    Query(query): Query<QuotaDashboardQuery>,
) -> Response {
    let query = match resolve_query(query) {
        Ok(query) => query,
        Err(message) => {
            return error(StatusCode::BAD_REQUEST, "invalid_query", &message);
        }
    };
    let path = state.db_path.clone();
    match tokio::task::spawn_blocking(move || query_dashboard(&path, query)).await {
        Ok(Ok(response)) => Json(response).into_response(),
        Ok(Err(error_value)) => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "database_unavailable",
            &error_value.to_string(),
        ),
        Err(error_value) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "query_failed",
            &error_value.to_string(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{nodes, router, ServerState};
    use axum::{
        body::{to_bytes, Body},
        extract::connect_info::MockConnectInfo,
        http::Request,
    };
    use std::net::SocketAddr;
    use tower::ServiceExt;

    fn metric(value: f64) -> QuotaMetric {
        QuotaMetric {
            key: "five-hour".to_owned(),
            label: "5 hour".to_owned(),
            kind: QuotaMetricKind::UtilizationPercent,
            utilization_percent: Some(value),
            used: None,
            remaining: None,
            total: None,
            unit: Some("%".to_owned()),
            resets_at: Some(1_800_000_000),
        }
    }

    fn batch(id: &str, sampled_at: i64, value: f64) -> QuotaUploadBatch {
        QuotaUploadBatch {
            schema_version: SCHEMA_VERSION,
            provider_states: vec![QuotaProviderState {
                app_type: "codex".to_owned(),
                provider_id: "provider-a".to_owned(),
                provider_name: "Provider A".to_owned(),
                status: QuotaProviderStatus::Ok,
                target_kind: Some(QuotaTargetKind::CodexOAuth),
                checked_at: sampled_at,
            }],
            observations: vec![QuotaObservation {
                observation_id: id.to_owned(),
                app_type: "codex".to_owned(),
                provider_id: "provider-a".to_owned(),
                sampled_at,
                metrics: vec![metric(value)],
            }],
        }
    }

    #[test]
    fn duplicate_is_idempotent_and_conflicting_content_is_atomic() {
        let mut connection = Connection::open_in_memory().unwrap();
        nodes::ensure_schema(&connection).unwrap();
        ensure_schema(&connection).unwrap();
        let (node, _) = nodes::create(&connection, "node-a").unwrap();
        let id = Uuid::new_v4().to_string();
        let first = batch(&id, 100, 20.0);
        assert_eq!(
            store_batch(&mut connection, &node.uuid, &first)
                .unwrap()
                .accepted,
            vec![id.clone()]
        );
        assert_eq!(
            store_batch(&mut connection, &node.uuid, &first)
                .unwrap()
                .duplicates,
            vec![id.clone()]
        );
        let conflict = batch(&id, 100, 30.0);
        assert!(matches!(
            store_batch(&mut connection, &node.uuid, &conflict),
            Err(StoreError::Conflict)
        ));
        let value: f64 = connection
            .query_row("SELECT utilization_percent FROM quota_metrics", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(value, 20.0);

        let new_id = Uuid::new_v4().to_string();
        let mut atomic = batch(&new_id, 200, 40.0);
        atomic
            .observations
            .push(batch(&id, 100, 30.0).observations.remove(0));
        assert!(matches!(
            store_batch(&mut connection, &node.uuid, &atomic),
            Err(StoreError::Conflict)
        ));
        let count: i64 = connection
            .query_row("SELECT COUNT(*) FROM quota_observations", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(
            count, 1,
            "the new first row must roll back with the conflict"
        );
    }

    #[test]
    fn balance_utilization_must_stay_within_percentage_bounds() {
        let mut balance = metric(50.0);
        balance.kind = QuotaMetricKind::Balance;
        balance.used = Some(5.0);
        balance.total = Some(10.0);
        assert!(valid_metric(&balance));
        balance.utilization_percent = Some(100.1);
        assert!(!valid_metric(&balance));
    }

    #[test]
    fn auto_bucket_returns_last_real_value_and_keeps_nodes_isolated() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("telemetry.db");
        let mut connection = crate::init_db(&path).unwrap();
        let (node_a, _) = nodes::create(&connection, "node-a").unwrap();
        let (node_b, _) = nodes::create(&connection, "node-b").unwrap();
        store_batch(
            &mut connection,
            &node_a.uuid,
            &batch(&Uuid::new_v4().to_string(), 100, 10.0),
        )
        .unwrap();
        store_batch(
            &mut connection,
            &node_a.uuid,
            &batch(&Uuid::new_v4().to_string(), 150, 30.0),
        )
        .unwrap();
        store_batch(
            &mut connection,
            &node_b.uuid,
            &batch(&Uuid::new_v4().to_string(), 150, 80.0),
        )
        .unwrap();
        drop(connection);

        let response = query_dashboard(
            &path,
            ResolvedQuotaQuery {
                from: 60,
                to: 200,
                bucket_seconds: 60,
                bucket_label: "1m".to_owned(),
                node_id: Some(node_a.uuid),
                provider_id: None,
            },
        )
        .unwrap();
        assert_eq!(response.providers.len(), 1);
        let points = &response.providers[0].series[0].points;
        assert_eq!(points.len(), 2);
        assert_eq!(points[1].utilization_percent, Some(30.0));
    }

    #[test]
    fn raw_continuity_preserves_600_second_boundary_inside_large_buckets() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("telemetry.db");
        let mut connection = crate::init_db(&path).unwrap();
        let (node, _) = nodes::create(&connection, "node").unwrap();
        for (index, timestamp) in [100, 699, 1299, 1900].into_iter().enumerate() {
            store_batch(
                &mut connection,
                &node.uuid,
                &batch(&Uuid::new_v4().to_string(), timestamp, index as f64),
            )
            .unwrap();
        }
        let response = query_dashboard(
            &path,
            ResolvedQuotaQuery {
                from: 0,
                to: 4000,
                bucket_seconds: 3600,
                bucket_label: "1h".into(),
                node_id: None,
                provider_id: None,
            },
        )
        .unwrap();
        let points = &response.providers[0].series[0].points;
        assert_eq!(
            points.iter().map(|p| p.sampled_at).collect::<Vec<_>>(),
            [100, 1299, 1900]
        );
        assert_eq!(points[0].segment_id, points[1].segment_id);
        assert_ne!(points[1].segment_id, points[2].segment_id);
    }

    #[test]
    fn dense_raw_samples_remain_connected_across_coarse_buckets() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("telemetry.db");
        let mut connection = crate::init_db(&path).unwrap();
        let (node, _) = nodes::create(&connection, "node").unwrap();
        for timestamp in (100..=2500).step_by(60) {
            store_batch(
                &mut connection,
                &node.uuid,
                &batch(&Uuid::new_v4().to_string(), timestamp, 10.0),
            )
            .unwrap();
        }
        let response = query_dashboard(
            &path,
            ResolvedQuotaQuery {
                from: 0,
                to: 4000,
                bucket_seconds: 900,
                bucket_label: "15m".into(),
                node_id: None,
                provider_id: None,
            },
        )
        .unwrap();
        let points = &response.providers[0].series[0].points;
        assert!(points
            .windows(2)
            .any(|p| p[1].sampled_at - p[0].sampled_at > 600));
        assert!(points
            .iter()
            .all(|point| point.segment_id == points[0].segment_id));
        assert_eq!(points.first().unwrap().sampled_at, 100);
        assert_eq!(points.last().unwrap().sampled_at, 2500);
    }

    #[tokio::test]
    async fn ingest_uses_bearer_node_mapping_and_dashboard_is_redacted() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("telemetry.db");
        let connection = crate::init_db(&path).unwrap();
        let (node, token) = nodes::create(&connection, "node@example.com").unwrap();
        let state = ServerState::new(connection, path, None);
        let observation_id = Uuid::new_v4().to_string();
        let mut payload = batch(&observation_id, 1_800_000_000, 55.0);
        payload.provider_states[0].provider_name = "account@example.com".to_owned();
        payload.observations[0].metrics[0].label = "secret@example.com".to_owned();
        let body = serde_json::to_vec(&payload).unwrap();

        let unauthorized = router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v3/quota/observations")
                    .header("content-type", "application/json")
                    .body(Body::from(body.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

        let accepted = router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v3/quota/observations")
                    .header("content-type", "application/json")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(accepted.status(), StatusCode::OK);
        let stored_node: String = state
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT node_id FROM quota_observations WHERE observation_id=?1",
                [&observation_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored_node, node.uuid);

        let dashboard = router(state)
            .layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 12345))))
            .oneshot(
                Request::builder()
                    .uri("/v3/dashboard/quota?from=1799999900&to=1800000100&bucket=1m")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(dashboard.status(), StatusCode::OK);
        let bytes = to_bytes(dashboard.into_body(), usize::MAX).await.unwrap();
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(text.contains("\"nodeName\":\"Node\""));
        assert!(text.contains("\"providerName\":\"Provider\""));
        assert!(text.contains("\"label\":\"Metric\""));
        for forbidden in [
            "node@example.com",
            "account@example.com",
            "secret@example.com",
            "accountId",
            "credentialMessage",
            "rawError",
            "contentHash",
        ] {
            assert!(!text.contains(forbidden), "leaked {forbidden}: {text}");
        }
    }
}

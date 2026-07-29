use crate::ServerState;
use axum::{
    body::Body,
    extract::{ConnectInfo, Query, Request, State},
    http::{header, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Redirect, Response},
    routing::get,
    Json, Router,
};
use rusqlite::{params_from_iter, types::Value, Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, net::SocketAddr, path::Path, time::Duration};

const DASHBOARD_HTML: &str = include_str!("../web/index.html");
const DASHBOARD_CSS: &str = include_str!("../web/styles.css");
const DASHBOARD_JS: &str = include_str!("../web/app.js");
const DASHBOARD_I18N_JS: &str = include_str!("../web/i18n.js");
const MAX_RANGE_SECONDS: i64 = 365 * 24 * 60 * 60;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct DashboardQuery {
    pub from: Option<i64>,
    pub to: Option<i64>,
    pub bucket: Option<String>,
    pub tz_offset_minutes: Option<i32>,
    pub node_id: Option<String>,
    pub app_type: Option<String>,
    pub provider_id: Option<String>,
    pub model: Option<String>,
    pub data_source: Option<String>,
}

#[derive(Debug, Clone)]
struct ResolvedQuery {
    from: i64,
    to: i64,
    bucket: Bucket,
    tz_offset_minutes: i32,
    node_id: Option<String>,
    app_type: Option<String>,
    provider_id: Option<String>,
    model: Option<String>,
    data_source: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
enum Bucket {
    FiveMinutes,
    Hour,
    Day,
}

impl Bucket {
    fn seconds(self) -> i64 {
        match self {
            Self::FiveMinutes => 5 * 60,
            Self::Hour => 60 * 60,
            Self::Day => 24 * 60 * 60,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::FiveMinutes => "5m",
            Self::Hour => "1h",
            Self::Day => "1d",
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OverviewResponse {
    pub range: RangeResponse,
    pub summary: SummaryResponse,
    pub coverage: CoverageResponse,
    pub trend: Vec<TrendPoint>,
    pub breakdowns: BreakdownsResponse,
    pub data_scope: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RangeResponse {
    pub from: i64,
    pub to: i64,
    pub bucket: &'static str,
    pub tz_offset_minutes: i32,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SummaryResponse {
    pub total_requests: i64,
    pub successful_requests: i64,
    pub success_rate: f64,
    pub fresh_input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
    pub real_total_tokens: i64,
    pub cache_hit_rate: f64,
    pub total_cost_usd: f64,
    pub avg_latency_ms: f64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageResponse {
    pub first_event_at: Option<i64>,
    pub last_event_at: Option<i64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrendPoint {
    pub bucket_start: i64,
    pub total_requests: i64,
    pub successful_requests: i64,
    pub success_rate: f64,
    pub real_total_tokens: i64,
    pub total_cost_usd: f64,
    pub avg_latency_ms: f64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BreakdownsResponse {
    pub nodes: Vec<BreakdownItem>,
    pub apps: Vec<BreakdownItem>,
    pub providers: Vec<BreakdownItem>,
    pub models: Vec<BreakdownItem>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BreakdownItem {
    pub key: String,
    pub total_requests: i64,
    pub success_rate: f64,
    pub real_total_tokens: i64,
    pub total_cost_usd: f64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FiltersResponse {
    pub nodes: Vec<String>,
    pub apps: Vec<String>,
    pub providers: Vec<String>,
    pub models: Vec<String>,
    pub data_sources: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EventsQuery {
    pub from: Option<i64>,
    pub to: Option<i64>,
    pub bucket: Option<String>,
    pub tz_offset_minutes: Option<i32>,
    pub node_id: Option<String>,
    pub app_type: Option<String>,
    pub provider_id: Option<String>,
    pub model: Option<String>,
    pub data_source: Option<String>,
    pub limit: Option<usize>,
    pub before_created_at: Option<i64>,
    pub before_event_id: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventsResponse {
    pub items: Vec<EventItem>,
    pub next_cursor: Option<EventCursor>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventCursor {
    pub before_created_at: i64,
    pub before_event_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventItem {
    pub event_id: String,
    pub request_id: String,
    pub created_at: i64,
    pub node_id: String,
    pub app_type: String,
    pub provider_id: String,
    pub model: String,
    pub request_model: String,
    pub fresh_input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
    pub real_total_tokens: i64,
    pub total_cost_usd: f64,
    pub latency_ms: i64,
    pub status_code: i64,
    pub is_streaming: bool,
    pub data_source: String,
}

#[derive(Debug)]
enum ApiError {
    BadRequest(String),
    Database(String),
}

#[derive(Serialize)]
struct ErrorResponse<'a> {
    code: &'a str,
    message: String,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            Self::BadRequest(message) => (StatusCode::BAD_REQUEST, "invalid_query", message),
            Self::Database(message) => (
                StatusCode::SERVICE_UNAVAILABLE,
                "database_unavailable",
                message,
            ),
        };
        (status, Json(ErrorResponse { code, message })).into_response()
    }
}

fn normalized_filter(value: Option<String>, name: &str) -> Result<Option<String>, ApiError> {
    match value.map(|value| value.trim().to_owned()) {
        Some(value) if value.len() > 128 => {
            Err(ApiError::BadRequest(format!("{name} is too long")))
        }
        Some(value) if !value.is_empty() => Ok(Some(value)),
        _ => Ok(None),
    }
}

fn resolve_query(query: DashboardQuery) -> Result<ResolvedQuery, ApiError> {
    let now = chrono::Utc::now().timestamp();
    let to = query.to.unwrap_or(now);
    let from = query.from.unwrap_or(to - 24 * 60 * 60);
    if from >= to {
        return Err(ApiError::BadRequest("from must be less than to".to_owned()));
    }
    if to - from > MAX_RANGE_SECONDS {
        return Err(ApiError::BadRequest(
            "time range cannot exceed 365 days".to_owned(),
        ));
    }
    let tz_offset_minutes = query.tz_offset_minutes.unwrap_or(0);
    if !(-840..=840).contains(&tz_offset_minutes) {
        return Err(ApiError::BadRequest(
            "tz_offset_minutes must be between -840 and 840".to_owned(),
        ));
    }
    let bucket = match query.bucket.as_deref().unwrap_or("auto") {
        "auto" if to - from <= 6 * 60 * 60 => Bucket::FiveMinutes,
        "auto" if to - from <= 48 * 60 * 60 => Bucket::Hour,
        "auto" => Bucket::Day,
        "5m" => Bucket::FiveMinutes,
        "1h" => Bucket::Hour,
        "1d" => Bucket::Day,
        _ => {
            return Err(ApiError::BadRequest(
                "bucket must be auto, 5m, 1h, or 1d".to_owned(),
            ))
        }
    };
    Ok(ResolvedQuery {
        from,
        to,
        bucket,
        tz_offset_minutes,
        node_id: normalized_filter(query.node_id, "node_id")?,
        app_type: normalized_filter(query.app_type, "app_type")?,
        provider_id: normalized_filter(query.provider_id, "provider_id")?,
        model: normalized_filter(query.model, "model")?,
        data_source: normalized_filter(query.data_source, "data_source")?,
    })
}

fn open_read_connection(path: &Path) -> anyhow::Result<Connection> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    connection.busy_timeout(Duration::from_secs(2))?;
    connection.execute_batch("PRAGMA query_only=ON;")?;
    Ok(connection)
}

fn fresh_input_sql(alias: &str) -> String {
    format!(
        "CASE \
           WHEN {alias}.input_token_semantics = 2 THEN {alias}.input_tokens \
           WHEN {alias}.app_type IN ('codex','gemini','grokbuild') \
                AND {alias}.input_token_semantics = 1 \
                AND {alias}.input_tokens >= \
                    ({alias}.cache_read_tokens + {alias}.cache_creation_tokens) \
           THEN ({alias}.input_tokens - {alias}.cache_read_tokens - \
                 {alias}.cache_creation_tokens) \
           WHEN {alias}.app_type IN ('codex','gemini','grokbuild') \
                AND {alias}.input_token_semantics = 0 \
                AND {alias}.input_tokens >= {alias}.cache_read_tokens \
           THEN ({alias}.input_tokens - {alias}.cache_read_tokens) \
           ELSE {alias}.input_tokens END"
    )
}

fn where_clause(query: &ResolvedQuery) -> (String, Vec<Value>) {
    let mut conditions = vec![
        "l.created_at >= ?".to_owned(),
        "l.created_at < ?".to_owned(),
    ];
    let mut values = vec![Value::Integer(query.from), Value::Integer(query.to)];
    for (column, value) in [
        ("node_id", &query.node_id),
        ("app_type", &query.app_type),
        ("provider_id", &query.provider_id),
        ("model", &query.model),
        ("data_source", &query.data_source),
    ] {
        if let Some(value) = value {
            conditions.push(format!("l.{column} = ?"));
            values.push(Value::Text(value.clone()));
        }
    }
    (conditions.join(" AND "), values)
}

fn summary_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<(SummaryResponse, CoverageResponse)> {
    let total_requests = row.get::<_, i64>(0)?;
    let successful_requests = row.get::<_, i64>(1)?;
    let fresh_input_tokens = row.get::<_, i64>(2)?;
    let output_tokens = row.get::<_, i64>(3)?;
    let cache_read_tokens = row.get::<_, i64>(4)?;
    let cache_creation_tokens = row.get::<_, i64>(5)?;
    let real_total_tokens = row.get::<_, i64>(6)?;
    let cacheable = fresh_input_tokens + cache_creation_tokens + cache_read_tokens;
    Ok((
        SummaryResponse {
            total_requests,
            successful_requests,
            success_rate: percentage(successful_requests, total_requests),
            fresh_input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_creation_tokens,
            real_total_tokens,
            cache_hit_rate: ratio(cache_read_tokens, cacheable),
            total_cost_usd: row.get(7)?,
            avg_latency_ms: row.get(8)?,
        },
        CoverageResponse {
            first_event_at: row.get(9)?,
            last_event_at: row.get(10)?,
        },
    ))
}

fn percentage(numerator: i64, denominator: i64) -> f64 {
    if denominator > 0 {
        numerator as f64 / denominator as f64 * 100.0
    } else {
        0.0
    }
}

fn ratio(numerator: i64, denominator: i64) -> f64 {
    if denominator > 0 {
        numerator as f64 / denominator as f64
    } else {
        0.0
    }
}

fn query_overview(path: &Path, query: ResolvedQuery) -> anyhow::Result<OverviewResponse> {
    let connection = open_read_connection(path)?;
    let fresh_input = fresh_input_sql("l");
    let real_total = format!(
        "({fresh_input} + l.output_tokens + l.cache_creation_tokens + l.cache_read_tokens)"
    );
    let (where_sql, values) = where_clause(&query);
    let summary_sql = format!(
        "SELECT COUNT(*),
                COALESCE(SUM(CASE WHEN l.status_code >= 200 AND l.status_code < 300
                                  THEN 1 ELSE 0 END), 0),
                COALESCE(SUM({fresh_input}), 0),
                COALESCE(SUM(l.output_tokens), 0),
                COALESCE(SUM(l.cache_read_tokens), 0),
                COALESCE(SUM(l.cache_creation_tokens), 0),
                COALESCE(SUM({real_total}), 0),
                COALESCE(SUM(CAST(l.total_cost_usd AS REAL)), 0.0),
                COALESCE(AVG(l.latency_ms), 0.0),
                MIN(l.created_at),
                MAX(l.created_at)
         FROM usage_events l WHERE {where_sql}"
    );
    let (summary, coverage) = connection.query_row(
        &summary_sql,
        params_from_iter(values.iter()),
        summary_from_row,
    )?;

    let trend = query_trend(&connection, &query, &fresh_input, &real_total)?;
    let breakdowns = BreakdownsResponse {
        nodes: query_breakdown(&connection, &query, "node_id", &fresh_input, &real_total)?,
        apps: query_breakdown(&connection, &query, "app_type", &fresh_input, &real_total)?,
        providers: query_breakdown(
            &connection,
            &query,
            "provider_id",
            &fresh_input,
            &real_total,
        )?,
        models: query_breakdown(&connection, &query, "model", &fresh_input, &real_total)?,
    };
    Ok(OverviewResponse {
        range: RangeResponse {
            from: query.from,
            to: query.to,
            bucket: query.bucket.label(),
            tz_offset_minutes: query.tz_offset_minutes,
        },
        summary,
        coverage,
        trend,
        breakdowns,
        data_scope: "detailOnly",
    })
}

fn query_trend(
    connection: &Connection,
    query: &ResolvedQuery,
    fresh_input: &str,
    real_total: &str,
) -> anyhow::Result<Vec<TrendPoint>> {
    let seconds = query.bucket.seconds();
    let offset = if matches!(query.bucket, Bucket::Day) {
        i64::from(query.tz_offset_minutes) * 60
    } else {
        0
    };
    let bucket_start = format!("(((l.created_at + {offset}) / {seconds}) * {seconds}) - {offset}");
    let (where_sql, values) = where_clause(query);
    let sql = format!(
        "SELECT {bucket_start},
                COUNT(*),
                COALESCE(SUM(CASE WHEN l.status_code >= 200 AND l.status_code < 300
                                  THEN 1 ELSE 0 END), 0),
                COALESCE(SUM({real_total}), 0),
                COALESCE(SUM(CAST(l.total_cost_usd AS REAL)), 0.0),
                COALESCE(AVG(l.latency_ms), 0.0),
                COALESCE(SUM({fresh_input}), 0)
         FROM usage_events l
         WHERE {where_sql}
         GROUP BY 1
         ORDER BY 1"
    );
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(values.iter()), |row| {
        let requests = row.get::<_, i64>(1)?;
        let successful = row.get::<_, i64>(2)?;
        Ok(TrendPoint {
            bucket_start: row.get(0)?,
            total_requests: requests,
            successful_requests: successful,
            success_rate: percentage(successful, requests),
            real_total_tokens: row.get(3)?,
            total_cost_usd: row.get(4)?,
            avg_latency_ms: row.get(5)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

fn query_breakdown(
    connection: &Connection,
    query: &ResolvedQuery,
    dimension: &str,
    fresh_input: &str,
    real_total: &str,
) -> anyhow::Result<Vec<BreakdownItem>> {
    debug_assert!(matches!(
        dimension,
        "node_id" | "app_type" | "provider_id" | "model"
    ));
    let (where_sql, values) = where_clause(query);
    let sql = format!(
        "SELECT l.{dimension},
                COUNT(*),
                COALESCE(SUM(CASE WHEN l.status_code >= 200 AND l.status_code < 300
                                  THEN 1 ELSE 0 END), 0),
                COALESCE(SUM({real_total}), 0),
                COALESCE(SUM(CAST(l.total_cost_usd AS REAL)), 0.0),
                COALESCE(SUM({fresh_input}), 0)
         FROM usage_events l
         WHERE {where_sql}
         GROUP BY l.{dimension}
         ORDER BY 4 DESC, 1 ASC
         LIMIT 10"
    );
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(values.iter()), |row| {
        let requests = row.get::<_, i64>(1)?;
        let successful = row.get::<_, i64>(2)?;
        Ok(BreakdownItem {
            key: row.get(0)?,
            total_requests: requests,
            success_rate: percentage(successful, requests),
            real_total_tokens: row.get(3)?,
            total_cost_usd: row.get(4)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

fn query_filters(path: &Path, query: ResolvedQuery) -> anyhow::Result<FiltersResponse> {
    let connection = open_read_connection(path)?;
    let (where_sql, values) = where_clause(&query);
    let sql = format!(
        "SELECT l.node_id, l.app_type, l.provider_id, l.model, l.data_source
         FROM usage_events l WHERE {where_sql}"
    );
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(values.iter()), |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
        ))
    })?;
    let mut nodes = BTreeSet::new();
    let mut apps = BTreeSet::new();
    let mut providers = BTreeSet::new();
    let mut models = BTreeSet::new();
    let mut data_sources = BTreeSet::new();
    for row in rows {
        let (node, app, provider, model, source) = row?;
        nodes.insert(node);
        apps.insert(app);
        providers.insert(provider);
        models.insert(model);
        data_sources.insert(source);
    }
    Ok(FiltersResponse {
        nodes: nodes.into_iter().collect(),
        apps: apps.into_iter().collect(),
        providers: providers.into_iter().collect(),
        models: models.into_iter().collect(),
        data_sources: data_sources.into_iter().collect(),
    })
}

fn query_events(
    path: &Path,
    query: ResolvedQuery,
    limit: usize,
    before: Option<(i64, String)>,
) -> anyhow::Result<EventsResponse> {
    let connection = open_read_connection(path)?;
    let fresh_input = fresh_input_sql("l");
    let real_total = format!(
        "({fresh_input} + l.output_tokens + l.cache_creation_tokens + l.cache_read_tokens)"
    );
    let (mut where_sql, mut values) = where_clause(&query);
    if let Some((created_at, event_id)) = &before {
        where_sql.push_str(" AND (l.created_at < ? OR (l.created_at = ? AND l.event_id < ?))");
        values.push(Value::Integer(*created_at));
        values.push(Value::Integer(*created_at));
        values.push(Value::Text(event_id.clone()));
    }
    values.push(Value::Integer((limit + 1) as i64));
    let sql = format!(
        "SELECT l.event_id, l.request_id, l.created_at, l.node_id, l.app_type,
                l.provider_id, l.model, l.request_model, {fresh_input},
                l.output_tokens, l.cache_read_tokens, l.cache_creation_tokens,
                {real_total}, CAST(l.total_cost_usd AS REAL), l.latency_ms,
                l.status_code, l.is_streaming, l.data_source
         FROM usage_events l
         WHERE {where_sql}
         ORDER BY l.created_at DESC, l.event_id DESC
         LIMIT ?"
    );
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(values.iter()), |row| {
        Ok(EventItem {
            event_id: row.get(0)?,
            request_id: row.get(1)?,
            created_at: row.get(2)?,
            node_id: row.get(3)?,
            app_type: row.get(4)?,
            provider_id: row.get(5)?,
            model: row.get(6)?,
            request_model: row.get(7)?,
            fresh_input_tokens: row.get(8)?,
            output_tokens: row.get(9)?,
            cache_read_tokens: row.get(10)?,
            cache_creation_tokens: row.get(11)?,
            real_total_tokens: row.get(12)?,
            total_cost_usd: row.get(13)?,
            latency_ms: row.get(14)?,
            status_code: row.get(15)?,
            is_streaming: row.get::<_, i64>(16)? != 0,
            data_source: row.get(17)?,
        })
    })?;
    let mut items = rows.collect::<Result<Vec<_>, _>>()?;
    let has_more = items.len() > limit;
    items.truncate(limit);
    let next_cursor = if has_more {
        items.last().map(|item| EventCursor {
            before_created_at: item.created_at,
            before_event_id: item.event_id.clone(),
        })
    } else {
        None
    };
    Ok(EventsResponse { items, next_cursor })
}

async fn overview(
    State(state): State<ServerState>,
    Query(query): Query<DashboardQuery>,
) -> Result<Json<OverviewResponse>, ApiError> {
    let query = resolve_query(query)?;
    let path = state.db_path.clone();
    let result = tokio::task::spawn_blocking(move || query_overview(&path, query))
        .await
        .map_err(|error| ApiError::Database(error.to_string()))?
        .map_err(|error| ApiError::Database(error.to_string()))?;
    Ok(Json(result))
}

async fn filters(
    State(state): State<ServerState>,
    Query(query): Query<DashboardQuery>,
) -> Result<Json<FiltersResponse>, ApiError> {
    let query = resolve_query(query)?;
    let path = state.db_path.clone();
    let result = tokio::task::spawn_blocking(move || query_filters(&path, query))
        .await
        .map_err(|error| ApiError::Database(error.to_string()))?
        .map_err(|error| ApiError::Database(error.to_string()))?;
    Ok(Json(result))
}

async fn events(
    State(state): State<ServerState>,
    Query(query): Query<EventsQuery>,
) -> Result<Json<EventsResponse>, ApiError> {
    let filter = resolve_query(DashboardQuery {
        from: query.from,
        to: query.to,
        bucket: query.bucket,
        tz_offset_minutes: query.tz_offset_minutes,
        node_id: query.node_id,
        app_type: query.app_type,
        provider_id: query.provider_id,
        model: query.model,
        data_source: query.data_source,
    })?;
    let limit = query.limit.unwrap_or(50);
    if !(1..=200).contains(&limit) {
        return Err(ApiError::BadRequest(
            "limit must be between 1 and 200".to_owned(),
        ));
    }
    let before = match (query.before_created_at, query.before_event_id) {
        (Some(created_at), Some(event_id)) => Some((
            created_at,
            normalized_filter(Some(event_id), "before_event_id")?.unwrap(),
        )),
        (None, None) => None,
        _ => {
            return Err(ApiError::BadRequest(
                "before_created_at and before_event_id must be supplied together".to_owned(),
            ))
        }
    };
    let path = state.db_path.clone();
    let result = tokio::task::spawn_blocking(move || query_events(&path, filter, limit, before))
        .await
        .map_err(|error| ApiError::Database(error.to_string()))?
        .map_err(|error| ApiError::Database(error.to_string()))?;
    Ok(Json(result))
}

async fn local_only(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    request: Request,
    next: Next,
) -> Response {
    if peer.ip().is_loopback() {
        next.run(request).await
    } else {
        (
            StatusCode::FORBIDDEN,
            Json(ErrorResponse {
                code: "local_only",
                message: "dashboard is available only from the server host".to_owned(),
            }),
        )
            .into_response()
    }
}

fn static_response(content_type: &'static str, body: &'static str) -> Response {
    let mut response = Response::new(Body::from(body));
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'none'; connect-src 'self'; img-src 'self' data:; \
             style-src 'self'; script-src 'self'; base-uri 'none'; frame-ancestors 'none'",
        ),
    );
    response
}

async fn index() -> Response {
    static_response("text/html; charset=utf-8", DASHBOARD_HTML)
}

async fn styles() -> Response {
    static_response("text/css; charset=utf-8", DASHBOARD_CSS)
}

async fn script() -> Response {
    static_response("text/javascript; charset=utf-8", DASHBOARD_JS)
}

async fn i18n_script() -> Response {
    static_response("text/javascript; charset=utf-8", DASHBOARD_I18N_JS)
}

async fn root() -> Redirect {
    Redirect::temporary("/dashboard/")
}

pub fn routes() -> Router<ServerState> {
    Router::new()
        .route("/", get(root))
        .route("/dashboard", get(root))
        .route("/dashboard/", get(index))
        .route("/dashboard/styles.css", get(styles))
        .route("/dashboard/app.js", get(script))
        .route("/dashboard/i18n.js", get(i18n_script))
        .route("/v1/dashboard/overview", get(overview))
        .route("/v1/dashboard/filters", get(filters))
        .route("/v1/dashboard/events", get(events))
        .route_layer(middleware::from_fn(local_only))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{init_db, router};
    use axum::{
        body::{to_bytes, Body},
        extract::connect_info::MockConnectInfo,
        http::Request,
    };
    use std::sync::{Arc, Mutex};
    use tower::ServiceExt;

    struct TestUsage<'a> {
        app_type: &'a str,
        input_tokens: i64,
        output_tokens: i64,
        cache_read_tokens: i64,
        cache_creation_tokens: i64,
        input_token_semantics: i64,
        status_code: i64,
    }

    fn insert_event(
        connection: &Connection,
        event_id: &str,
        created_at: i64,
        usage: TestUsage<'_>,
    ) {
        connection
            .execute(
                "INSERT INTO usage_events (
                    event_id,node_id,request_id,created_at,app_type,provider_id,model,
                    request_model,pricing_model,input_tokens,output_tokens,
                    cache_read_tokens,cache_creation_tokens,input_token_semantics,
                    total_cost_usd,latency_ms,status_code,is_streaming,data_source,received_at
                 ) VALUES (?1,'node-a',?1,?2,?3,'provider-a','model-a','','',
                           ?4,?5,?6,?7,?8,'0.25',120,?9,1,'proxy',?2)",
                rusqlite::params![
                    event_id,
                    created_at,
                    usage.app_type,
                    usage.input_tokens,
                    usage.output_tokens,
                    usage.cache_read_tokens,
                    usage.cache_creation_tokens,
                    usage.input_token_semantics,
                    usage.status_code
                ],
            )
            .unwrap();
    }

    #[test]
    fn auto_bucket_and_range_validation() {
        let query = resolve_query(DashboardQuery {
            from: Some(0),
            to: Some(60 * 60),
            ..Default::default()
        })
        .unwrap();
        assert!(matches!(query.bucket, Bucket::FiveMinutes));
        let invalid = resolve_query(DashboardQuery {
            from: Some(10),
            to: Some(10),
            ..Default::default()
        });
        assert!(matches!(invalid, Err(ApiError::BadRequest(_))));
    }

    #[test]
    fn percentages_handle_empty_denominators() {
        assert_eq!(percentage(1, 2), 50.0);
        assert_eq!(percentage(0, 0), 0.0);
        assert_eq!(ratio(1, 4), 0.25);
        assert_eq!(ratio(0, 0), 0.0);
    }

    #[test]
    fn token_sql_contains_cc_switch_semantics() {
        let sql = fresh_input_sql("l");
        assert!(sql.contains("'codex','gemini','grokbuild'"));
        assert!(sql.contains("input_token_semantics = 1"));
        assert!(sql.contains("input_token_semantics = 2"));
    }

    #[test]
    fn overview_normalizes_tokens_and_filters_detail_events() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("telemetry.db");
        let connection = init_db(&path).unwrap();
        insert_event(
            &connection,
            "claude",
            101,
            TestUsage {
                app_type: "claude",
                input_tokens: 100,
                output_tokens: 20,
                cache_read_tokens: 40,
                cache_creation_tokens: 10,
                input_token_semantics: 0,
                status_code: 200,
            },
        );
        insert_event(
            &connection,
            "total",
            102,
            TestUsage {
                app_type: "codex",
                input_tokens: 1000,
                output_tokens: 100,
                cache_read_tokens: 600,
                cache_creation_tokens: 100,
                input_token_semantics: 1,
                status_code: 200,
            },
        );
        insert_event(
            &connection,
            "legacy",
            103,
            TestUsage {
                app_type: "codex",
                input_tokens: 1000,
                output_tokens: 0,
                cache_read_tokens: 600,
                cache_creation_tokens: 100,
                input_token_semantics: 0,
                status_code: 512,
            },
        );
        insert_event(
            &connection,
            "fresh",
            104,
            TestUsage {
                app_type: "codex",
                input_tokens: 300,
                output_tokens: 100,
                cache_read_tokens: 600,
                cache_creation_tokens: 100,
                input_token_semantics: 2,
                status_code: 204,
            },
        );
        drop(connection);

        let query = resolve_query(DashboardQuery {
            from: Some(100),
            to: Some(105),
            bucket: Some("5m".to_owned()),
            ..Default::default()
        })
        .unwrap();
        let result = query_overview(&path, query).unwrap();
        assert_eq!(result.summary.total_requests, 4);
        assert_eq!(result.summary.successful_requests, 3);
        assert_eq!(result.summary.success_rate, 75.0);
        assert_eq!(result.summary.fresh_input_tokens, 1100);
        assert_eq!(result.summary.real_total_tokens, 3470);
        assert!((result.summary.cache_hit_rate - (1840.0 / 3250.0)).abs() < 1e-9);
        assert_eq!(result.coverage.first_event_at, Some(101));
        assert_eq!(result.coverage.last_event_at, Some(104));
        assert_eq!(result.breakdowns.nodes[0].key, "node-a");
    }

    #[test]
    fn event_cursor_is_stable_for_same_second() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("telemetry.db");
        let connection = init_db(&path).unwrap();
        for event_id in ["a", "b", "c"] {
            insert_event(
                &connection,
                event_id,
                100,
                TestUsage {
                    app_type: "claude",
                    input_tokens: 1,
                    output_tokens: 1,
                    cache_read_tokens: 0,
                    cache_creation_tokens: 0,
                    input_token_semantics: 0,
                    status_code: 200,
                },
            );
        }
        drop(connection);
        let query = resolve_query(DashboardQuery {
            from: Some(99),
            to: Some(101),
            ..Default::default()
        })
        .unwrap();
        let first = query_events(&path, query.clone(), 2, None).unwrap();
        assert_eq!(
            first
                .items
                .iter()
                .map(|item| item.event_id.as_str())
                .collect::<Vec<_>>(),
            vec!["c", "b"]
        );
        let cursor = first.next_cursor.unwrap();
        let second = query_events(
            &path,
            query,
            2,
            Some((cursor.before_created_at, cursor.before_event_id)),
        )
        .unwrap();
        assert_eq!(second.items[0].event_id, "a");
        assert!(second.next_cursor.is_none());
    }

    #[tokio::test]
    async fn dashboard_routes_are_loopback_only() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("telemetry.db");
        let connection = init_db(&path).unwrap();
        let state = ServerState {
            db: Arc::new(Mutex::new(connection)),
            db_path: path,
            auth_token: Some("secret".to_owned()),
        };
        let local = router(state.clone()).layer(MockConnectInfo(
            "127.0.0.1:12345".parse::<SocketAddr>().unwrap(),
        ));
        let response = local
            .oneshot(
                Request::builder()
                    .uri("/dashboard/")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "text/html; charset=utf-8"
        );

        let i18n = router(state.clone()).layer(MockConnectInfo(
            "127.0.0.1:12345".parse::<SocketAddr>().unwrap(),
        ));
        let response = i18n
            .oneshot(
                Request::builder()
                    .uri("/dashboard/i18n.js")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "text/javascript; charset=utf-8"
        );

        let remote = router(state.clone()).layer(MockConnectInfo(
            "192.0.2.1:12345".parse::<SocketAddr>().unwrap(),
        ));
        let response = remote
            .oneshot(
                Request::builder()
                    .uri("/dashboard/")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        let api = router(state.clone()).layer(MockConnectInfo(
            "127.0.0.1:12345".parse::<SocketAddr>().unwrap(),
        ));
        let response = api
            .oneshot(
                Request::builder()
                    .uri("/v1/dashboard/overview?from=1&to=2")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["dataScope"], "detailOnly");

        let events_api = router(state).layer(MockConnectInfo(
            "127.0.0.1:12345".parse::<SocketAddr>().unwrap(),
        ));
        let response = events_api
            .oneshot(
                Request::builder()
                    .uri("/v1/dashboard/events?from=1&to=2&limit=2")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
}

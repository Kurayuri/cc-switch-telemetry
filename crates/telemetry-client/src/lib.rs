use anyhow::Context;
use rusqlite::{Connection, Row};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};
use telemetry_core::{
    rollup_source_key, EventBatch, EventMutation, EventMutationBatch, MutationKind, ProviderEntry,
    ProviderMutationBatch, ProviderSnapshot, RollupMutation, RollupMutationBatch, SyncBeginRequest,
    SyncCommitRequest, SyncCommitResponse, UsageEvent, SCHEMA_VERSION,
};

pub mod quota;
pub mod usage_ledger;

const UPLOAD_MAX_ATTEMPTS: usize = 5;
const UPLOAD_INITIAL_RETRY_DELAY: Duration = Duration::from_millis(250);

#[derive(Debug, Clone)]
pub struct ClientConfig {
    pub cc_switch_db: PathBuf,
    pub server_url: String,
    pub auth_token: String,
    pub batch_size: usize,
    pub overlap_seconds: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct Cursor {
    pub created_at: i64,
    pub request_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileFingerprint {
    pub modified: SystemTime,
    pub len: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatabaseFingerprint {
    pub database: FileFingerprint,
    pub wal: Option<FileFingerprint>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncSummary {
    pub sent: usize,
    pub accepted: usize,
    pub duplicates: usize,
    pub rejected: usize,
    pub cursor_advanced: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirrorVerification {
    pub detail_rows: usize,
    pub rollup_rows: usize,
}

fn file_fingerprint(path: &Path) -> std::io::Result<FileFingerprint> {
    let metadata = std::fs::metadata(path)?;
    Ok(FileFingerprint {
        modified: metadata.modified()?,
        len: metadata.len(),
    })
}

fn wal_path(database: &Path) -> PathBuf {
    let mut path = OsString::from(database.as_os_str());
    path.push("-wal");
    PathBuf::from(path)
}

pub fn database_fingerprint(path: &Path) -> std::io::Result<DatabaseFingerprint> {
    let database = file_fingerprint(path)?;
    let wal = match file_fingerprint(&wal_path(path)) {
        Ok(fingerprint) => Some(fingerprint),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    Ok(DatabaseFingerprint { database, wal })
}

pub fn load_cursor(path: &std::path::Path) -> anyhow::Result<Cursor> {
    match std::fs::read_to_string(path) {
        Ok(contents) => Ok(serde_json::from_str(&contents)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Cursor::default()),
        Err(error) => Err(error.into()),
    }
}

pub fn save_cursor(path: &std::path::Path, cursor: &Cursor) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("tmp");
    std::fs::write(&temporary, serde_json::to_vec(cursor)?)?;
    std::fs::rename(temporary, path)?;
    Ok(())
}

fn event_from_row(row: &Row<'_>) -> rusqlite::Result<UsageEvent> {
    let request_id: String = row.get("request_id")?;
    Ok(UsageEvent {
        request_id,
        created_at: row.get("created_at")?,
        app_type: row.get("app_type")?,
        provider_id: row.get("provider_id")?,
        model: row.get("model")?,
        request_model: row.get("request_model")?,
        pricing_model: row.get("pricing_model")?,
        input_tokens: row.get("input_tokens")?,
        output_tokens: row.get("output_tokens")?,
        cache_read_tokens: row.get("cache_read_tokens")?,
        cache_creation_tokens: row.get("cache_creation_tokens")?,
        input_token_semantics: row.get("input_token_semantics")?,
        total_cost_usd: row.get("total_cost_usd")?,
        latency_ms: row.get("latency_ms")?,
        status_code: row.get("status_code")?,
        is_streaming: row.get::<_, i64>("is_streaming")? != 0,
        data_source: row.get("data_source")?,
    })
}

pub fn read_events(config: &ClientConfig, cursor: &Cursor) -> anyhow::Result<Vec<UsageEvent>> {
    let conn = Connection::open_with_flags(
        &config.cc_switch_db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .context("open cc-switch db read-only")?;
    conn.busy_timeout(Duration::from_secs(2))?;
    let has_input_semantics = conn
        .prepare("PRAGMA table_info(proxy_request_logs)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?
        .iter()
        .any(|column| column == "input_token_semantics");
    let semantics_column = if has_input_semantics {
        "input_token_semantics"
    } else {
        "0 AS input_token_semantics"
    };
    let sql = format!(
        "SELECT request_id, created_at, app_type, provider_id, model, request_model, \
         pricing_model, input_tokens, output_tokens, cache_read_tokens, \
         cache_creation_tokens, {semantics_column}, total_cost_usd, latency_ms, \
         status_code, is_streaming, data_source FROM proxy_request_logs \
         WHERE (created_at > ?1 OR (created_at = ?1 AND request_id > ?2)) \
         ORDER BY created_at, request_id LIMIT ?3"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(
        rusqlite::params![
            cursor.created_at,
            cursor.request_id,
            config.batch_size as i64
        ],
        event_from_row,
    )?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

pub fn read_provider_snapshot(config: &ClientConfig) -> anyhow::Result<ProviderSnapshot> {
    let conn = Connection::open_with_flags(
        &config.cc_switch_db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .context("open usage source database read-only")?;
    conn.busy_timeout(Duration::from_secs(2))?;
    let has_providers: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'providers')",
        [],
        |row| row.get(0),
    )?;
    if !has_providers {
        return Ok(usage_ledger::local_provider_snapshot());
    }
    let mut stmt = conn.prepare(
        "SELECT id, app_type, name FROM providers
         WHERE TRIM(name) <> ''
         ORDER BY app_type, id",
    )?;
    let providers = stmt
        .query_map([], |row| {
            Ok(ProviderEntry {
                provider_id: row.get("id")?,
                app_type: row.get("app_type")?,
                name: row.get("name")?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ProviderSnapshot {
        schema_version: SCHEMA_VERSION,
        providers,
    })
}

pub async fn upload(
    config: &ClientConfig,
    events: Vec<UsageEvent>,
) -> anyhow::Result<telemetry_core::BatchResponse> {
    let url = format!(
        "{}/v1/events/batch",
        config.server_url.trim_end_matches('/')
    );
    let batch = EventBatch {
        schema_version: SCHEMA_VERSION,
        events,
    };
    let response = post_json_with_retry(config, &url, &batch).await?;
    response
        .json()
        .await
        .with_context(|| format!("decode usage batch response from {url}"))
}

pub async fn sync_provider_catalog(config: &ClientConfig) -> anyhow::Result<usize> {
    let snapshot = read_provider_snapshot(config)?;
    let provider_count = snapshot.providers.len();
    let url = format!(
        "{}/v1/providers/snapshot",
        config.server_url.trim_end_matches('/')
    );
    post_json_with_retry(config, &url, &snapshot).await?;
    Ok(provider_count)
}

fn content_hash<T: Serialize>(value: &T) -> anyhow::Result<String> {
    let bytes = serde_json::to_vec(value)?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn mutation_name(operation: &MutationKind) -> &'static str {
    match operation {
        MutationKind::Upsert => "upsert",
        MutationKind::Delete => "delete",
    }
}

fn all_events(config: &ClientConfig) -> anyhow::Result<Vec<UsageEvent>> {
    if config.batch_size == 0 {
        anyhow::bail!("batch_size must be greater than zero");
    }
    let mut cursor = Cursor::default();
    let mut events = Vec::new();
    loop {
        let batch = read_events(config, &cursor)?;
        if batch.is_empty() {
            break;
        }
        let count = batch.len();
        let last = batch.last().expect("non-empty event batch");
        cursor = Cursor {
            created_at: last.created_at,
            request_id: last.request_id.clone(),
        };
        events.extend(batch);
        if count < config.batch_size {
            break;
        }
    }
    Ok(events)
}

fn hashed_events(events: Vec<UsageEvent>) -> anyhow::Result<BTreeMap<String, String>> {
    events
        .into_iter()
        .map(|event| Ok((event.request_id.clone(), content_hash(&event)?)))
        .collect()
}

fn hashed_rollups(
    rollups: Vec<telemetry_core::RollupSnapshot>,
) -> anyhow::Result<BTreeMap<String, String>> {
    rollups
        .into_iter()
        .map(|rollup| Ok((rollup_source_key(&rollup), content_hash(&rollup)?)))
        .collect()
}

fn verify_hash_map(
    entity: &str,
    source: &BTreeMap<String, String>,
    mirror: &BTreeMap<String, String>,
) -> anyhow::Result<()> {
    let missing = source
        .keys()
        .filter(|key| !mirror.contains_key(*key))
        .count();
    let extra = mirror
        .keys()
        .filter(|key| !source.contains_key(*key))
        .count();
    let changed = source
        .iter()
        .filter(|(key, hash)| mirror.get(*key).is_some_and(|value| value != *hash))
        .count();
    if missing + extra + changed > 0 {
        let first_key = source
            .keys()
            .chain(mirror.keys())
            .find(|key| source.get(*key) != mirror.get(*key))
            .map(String::as_str)
            .unwrap_or("unknown");
        anyhow::bail!(
            "{entity} mirror mismatch: missing={missing} extra={extra} changed={changed} first_key={first_key}"
        );
    }
    Ok(())
}

/// Compare an exact cc-switch source with the Client-owned mirror without
/// modifying either database. Only stable IDs and content hashes are reported.
pub fn verify_cc_switch_mirror(
    source_config: &ClientConfig,
    ledger_path: &Path,
) -> anyhow::Result<MirrorVerification> {
    let mut ledger_config = source_config.clone();
    ledger_config.cc_switch_db = ledger_path.to_owned();
    let source_events = hashed_events(all_events(source_config)?)?;
    let mirror_events = hashed_events(all_events(&ledger_config)?)?;
    verify_hash_map("detail", &source_events, &mirror_events)?;
    let source_rollups = hashed_rollups(usage_ledger::read_rollups(&source_config.cc_switch_db)?)?;
    let mirror_rollups = hashed_rollups(usage_ledger::read_rollups(ledger_path)?)?;
    verify_hash_map("rollup", &source_rollups, &mirror_rollups)?;
    Ok(MirrorVerification {
        detail_rows: source_events.len(),
        rollup_rows: source_rollups.len(),
    })
}

/// Upload ledger changes as an atomic protocol-v2 generation. The first sync
/// to a remote is a complete replacement; later syncs use a durable content-
/// hash baseline and send only upserts/deletes. The baseline advances only
/// after the server commits the generation.
pub async fn sync_snapshot_v2(
    ledger_config: &ClientConfig,
    provider_config: &ClientConfig,
    source_kind: &str,
) -> anyhow::Result<SyncCommitResponse> {
    sync_snapshot_v2_with_mode(ledger_config, provider_config, source_kind, false).await
}

pub async fn sync_snapshot_v2_with_mode(
    ledger_config: &ClientConfig,
    provider_config: &ClientConfig,
    source_kind: &str,
    force_replace_all: bool,
) -> anyhow::Result<SyncCommitResponse> {
    if !matches!(source_kind, "cc-switch" | "local" | "local-compact") {
        anyhow::bail!("unknown source {source_kind}; expected local, local-compact, or cc-switch");
    }
    let events = all_events(ledger_config)?;
    let rollups = usage_ledger::read_rollups(&ledger_config.cc_switch_db)?;
    let mut providers = read_provider_snapshot(provider_config)?.providers;
    providers.sort_by(|left, right| {
        (&left.app_type, &left.provider_id).cmp(&(&right.app_type, &right.provider_id))
    });
    let generation_id = uuid::Uuid::new_v4().to_string();
    let remote_key = content_hash(&serde_json::json!({
        "serverUrl": ledger_config.server_url.trim_end_matches('/'),
        "token": ledger_config.auth_token,
        "sourceKind": source_kind,
    }))?;
    let baseline =
        usage_ledger::load_upload_baseline(&ledger_config.cc_switch_db, &remote_key, source_kind)?;
    let replace_all = force_replace_all || !baseline.initialized;

    let mut current_event_hashes = BTreeMap::new();
    let mut event_mutations = Vec::new();
    for event in events {
        let event_hash = content_hash(&event)?;
        current_event_hashes.insert(event.request_id.clone(), event_hash.clone());
        if !replace_all && baseline.event_hashes.get(&event.request_id) == Some(&event_hash) {
            continue;
        }
        event_mutations.push(EventMutation {
            operation: MutationKind::Upsert,
            request_id: event.request_id.clone(),
            content_hash: event_hash,
            event: Some(event),
        });
    }
    if !replace_all {
        for (request_id, prior_hash) in &baseline.event_hashes {
            if !current_event_hashes.contains_key(request_id) {
                event_mutations.push(EventMutation {
                    operation: MutationKind::Delete,
                    request_id: request_id.clone(),
                    content_hash: prior_hash.clone(),
                    event: None,
                });
            }
        }
    }
    event_mutations.sort_by(|left, right| left.request_id.cmp(&right.request_id));

    let mut current_rollup_hashes = BTreeMap::new();
    let mut rollup_mutations = Vec::new();
    for snapshot in rollups {
        let snapshot_key = rollup_source_key(&snapshot);
        let snapshot_hash = content_hash(&snapshot)?;
        current_rollup_hashes.insert(snapshot_key.clone(), snapshot_hash.clone());
        if !replace_all && baseline.rollup_hashes.get(&snapshot_key) == Some(&snapshot_hash) {
            continue;
        }
        rollup_mutations.push(RollupMutation {
            operation: MutationKind::Upsert,
            snapshot_key,
            content_hash: snapshot_hash,
            snapshot: Some(snapshot),
        });
    }
    if !replace_all {
        for (snapshot_key, prior_hash) in &baseline.rollup_hashes {
            if !current_rollup_hashes.contains_key(snapshot_key) {
                rollup_mutations.push(RollupMutation {
                    operation: MutationKind::Delete,
                    snapshot_key: snapshot_key.clone(),
                    content_hash: prior_hash.clone(),
                    snapshot: None,
                });
            }
        }
    }
    rollup_mutations.sort_by(|left, right| left.snapshot_key.cmp(&right.snapshot_key));
    let event_manifest = event_mutations
        .iter()
        .map(|item| {
            (
                item.request_id.as_str(),
                mutation_name(&item.operation),
                item.content_hash.as_str(),
            )
        })
        .collect::<Vec<_>>();
    let rollup_manifest = rollup_mutations
        .iter()
        .map(|item| {
            (
                item.snapshot_key.as_str(),
                mutation_name(&item.operation),
                item.content_hash.as_str(),
            )
        })
        .collect::<Vec<_>>();
    let manifest_hash = content_hash(&(event_manifest, rollup_manifest, &providers))?;
    let base = ledger_config.server_url.trim_end_matches('/');
    let begin_url = format!("{base}/v2/sync/begin");
    post_json_with_retry(
        ledger_config,
        &begin_url,
        &SyncBeginRequest {
            schema_version: SCHEMA_VERSION,
            generation_id: generation_id.clone(),
            source_kind: source_kind.to_owned(),
            replace_all,
        },
    )
    .await?;

    for mutations in event_mutations.chunks(ledger_config.batch_size.clamp(1, 2_000)) {
        let url = format!("{base}/v2/sync/events");
        post_json_with_retry(
            ledger_config,
            &url,
            &EventMutationBatch {
                schema_version: SCHEMA_VERSION,
                generation_id: generation_id.clone(),
                mutations: mutations.to_vec(),
            },
        )
        .await?;
    }
    for mutations in rollup_mutations.chunks(ledger_config.batch_size.clamp(1, 2_000)) {
        let url = format!("{base}/v2/sync/rollups");
        post_json_with_retry(
            ledger_config,
            &url,
            &RollupMutationBatch {
                schema_version: SCHEMA_VERSION,
                generation_id: generation_id.clone(),
                mutations: mutations.to_vec(),
            },
        )
        .await?;
    }
    let providers_url = format!("{base}/v2/sync/providers");
    post_json_with_retry(
        ledger_config,
        &providers_url,
        &ProviderMutationBatch {
            schema_version: SCHEMA_VERSION,
            generation_id: generation_id.clone(),
            providers,
        },
    )
    .await?;

    let commit_url = format!("{base}/v2/sync/commit");
    let result = post_json_with_retry(
        ledger_config,
        &commit_url,
        &SyncCommitRequest {
            schema_version: SCHEMA_VERSION,
            generation_id,
            expected_event_mutations: event_mutations.len(),
            expected_rollup_mutations: rollup_mutations.len(),
            manifest_hash,
        },
    )
    .await?
    .json()
    .await
    .with_context(|| format!("decode protocol-v2 commit response from {commit_url}"))?;
    usage_ledger::save_upload_baseline(
        &ledger_config.cc_switch_db,
        &remote_key,
        source_kind,
        &current_event_hashes,
        &current_rollup_hashes,
    )?;
    Ok(result)
}

pub(crate) async fn post_json_with_retry<T: Serialize>(
    config: &ClientConfig,
    url: &str,
    payload: &T,
) -> anyhow::Result<reqwest::Response> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()?;
    for attempt in 0..UPLOAD_MAX_ATTEMPTS {
        let request = client
            .post(url)
            .json(payload)
            .bearer_auth(&config.auth_token);

        let response = match request.send().await {
            Ok(response) => response,
            Err(_) if attempt + 1 < UPLOAD_MAX_ATTEMPTS => {
                tokio::time::sleep(upload_retry_delay(attempt)).await;
                continue;
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("post telemetry payload to {url} after {UPLOAD_MAX_ATTEMPTS} attempts")
                })
            }
        };

        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }

        let body = response.text().await.unwrap_or_default();
        if !is_retryable_status(status) || attempt + 1 == UPLOAD_MAX_ATTEMPTS {
            let detail = if body.trim().is_empty() {
                status.to_string()
            } else {
                body.trim().to_owned()
            };
            anyhow::bail!("telemetry upload failed with HTTP {status} for {url}: {detail}");
        }

        tokio::time::sleep(upload_retry_delay(attempt)).await;
    }

    unreachable!("upload loop always returns or retries before exhausting attempts")
}

fn is_retryable_status(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 408 | 425 | 429 | 500 | 502 | 503 | 504)
}

fn upload_retry_delay(attempt: usize) -> Duration {
    let multiplier = 1u32 << attempt.min(3);
    UPLOAD_INITIAL_RETRY_DELAY.saturating_mul(multiplier)
}

fn overlap_start(cursor: &Cursor, overlap_seconds: i64) -> Cursor {
    Cursor {
        created_at: cursor.created_at.saturating_sub(overlap_seconds),
        request_id: String::new(),
    }
}

pub async fn sync_available(
    config: &ClientConfig,
    cursor: &mut Cursor,
) -> anyhow::Result<SyncSummary> {
    if config.batch_size == 0 {
        anyhow::bail!("batch_size must be greater than zero");
    }
    let original_cursor = cursor.clone();
    let mut scan_cursor = overlap_start(cursor, config.overlap_seconds);
    let mut summary = SyncSummary::default();
    loop {
        let events = read_events(config, &scan_cursor)?;
        if events.is_empty() {
            break;
        }
        let sent = events.len();
        let response = upload(config, events.clone()).await?;
        let acknowledged =
            response.accepted.len() + response.duplicates.len() + response.rejected.len();
        if acknowledged != sent {
            anyhow::bail!(
                "server acknowledgement mismatch: sent={sent} acknowledged={acknowledged}"
            );
        }
        summary.sent += sent;
        summary.accepted += response.accepted.len();
        summary.duplicates += response.duplicates.len();
        summary.rejected += response.rejected.len();
        if !response.rejected.is_empty() {
            anyhow::bail!(
                "server rejected {} of {sent} usage events",
                response.rejected.len()
            );
        }
        if let Some(last) = events.last() {
            scan_cursor = Cursor {
                created_at: last.created_at,
                request_id: last.request_id.clone(),
            };
            if scan_cursor > *cursor {
                *cursor = scan_cursor.clone();
            }
        }
        if sent < config.batch_size {
            break;
        }
    }
    summary.cursor_advanced = *cursor > original_cursor;
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        extract::State, http::StatusCode, response::IntoResponse, routing::post, Json, Router,
    };
    use std::{
        net::SocketAddr,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
    };
    use telemetry_core::BatchResponse;

    #[derive(Clone)]
    struct RetryState {
        attempts: Arc<AtomicUsize>,
    }

    async fn retry_then_accept(State(state): State<RetryState>) -> impl IntoResponse {
        let attempt = state.attempts.fetch_add(1, Ordering::SeqCst);
        if attempt < 2 {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(BatchResponse::default()),
            );
        }
        (
            StatusCode::OK,
            Json(BatchResponse {
                accepted: vec!["node-a:request-1".into()],
                ..Default::default()
            }),
        )
    }
    #[test]
    fn cursor_reads_same_second_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db.sqlite");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TABLE proxy_request_logs (request_id TEXT PRIMARY KEY, created_at INTEGER, app_type TEXT, provider_id TEXT, model TEXT, request_model TEXT, pricing_model TEXT, input_tokens INTEGER, output_tokens INTEGER, cache_read_tokens INTEGER, cache_creation_tokens INTEGER, total_cost_usd TEXT, latency_ms INTEGER, status_code INTEGER, is_streaming INTEGER, data_source TEXT); INSERT INTO proxy_request_logs VALUES ('a',10,'x','p','m','','',1,1,0,0,'0',1,200,0,'proxy'),('b',10,'x','p','m','','',1,1,0,0,'0',1,200,0,'proxy');").unwrap();
        let config = ClientConfig {
            cc_switch_db: path,
            server_url: "http://localhost".into(),
            auth_token: "test-token".into(),
            batch_size: 10,
            overlap_seconds: 0,
        };
        let events = read_events(&config, &Cursor::default()).unwrap();
        assert_eq!(events[0].input_token_semantics, 0);
        assert_eq!(
            events
                .iter()
                .map(|e| e.request_id.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b"]
        );
    }

    #[test]
    fn provider_snapshot_reads_cc_switch_provider_names() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db.sqlite");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE providers (
                 id TEXT NOT NULL,
                 app_type TEXT NOT NULL,
                 name TEXT NOT NULL,
                 PRIMARY KEY (id, app_type)
             );
             INSERT INTO providers (id, app_type, name)
             VALUES ('6bae9aab-8fb5-457f-8c70-1f91dd9e5c30', 'codex', 'DeepSeek');",
        )
        .unwrap();
        let config = ClientConfig {
            cc_switch_db: path,
            server_url: "http://localhost".into(),
            auth_token: "test-token".into(),
            batch_size: 10,
            overlap_seconds: 0,
        };
        let snapshot = read_provider_snapshot(&config).unwrap();
        assert_eq!(
            snapshot.providers,
            vec![ProviderEntry {
                app_type: "codex".into(),
                provider_id: "6bae9aab-8fb5-457f-8c70-1f91dd9e5c30".into(),
                name: "DeepSeek".into(),
            }]
        );
    }

    #[test]
    fn fingerprint_detects_wal_changes() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("cc-switch.db");
        std::fs::write(&database, b"db").unwrap();
        let before = database_fingerprint(&database).unwrap();
        assert!(before.wal.is_none());
        std::fs::write(wal_path(&database), b"wal-data").unwrap();
        let after_create = database_fingerprint(&database).unwrap();
        assert_ne!(before, after_create);
        std::fs::write(wal_path(&database), b"wal-data-extended").unwrap();
        let after_write = database_fingerprint(&database).unwrap();
        assert_ne!(after_create, after_write);
    }

    #[test]
    fn overlap_scan_start_never_moves_persistent_cursor() {
        let cursor = Cursor {
            created_at: 1_000,
            request_id: "request-z".into(),
        };
        let start = overlap_start(&cursor, 600);
        assert_eq!(start.created_at, 400);
        assert!(start.request_id.is_empty());
        assert_eq!(cursor.created_at, 1_000);
    }

    #[tokio::test]
    async fn upload_retries_service_unavailable_and_preserves_batch() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let state = RetryState {
            attempts: attempts.clone(),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route("/v1/events/batch", post(retry_then_accept))
                    .with_state(state),
            )
            .await
            .unwrap();
        });

        let config = ClientConfig {
            cc_switch_db: PathBuf::from("unused.db"),
            server_url: format!("http://{address}"),
            auth_token: "test-token".into(),
            batch_size: 1,
            overlap_seconds: 0,
        };
        let response = upload(
            &config,
            vec![UsageEvent {
                request_id: "request-1".into(),
                created_at: 1,
                app_type: "codex".into(),
                provider_id: "provider".into(),
                model: "model".into(),
                request_model: None,
                pricing_model: None,
                input_tokens: 1,
                output_tokens: 1,
                cache_read_tokens: 0,
                cache_creation_tokens: 0,
                input_token_semantics: 0,
                total_cost_usd: "0".into(),
                latency_ms: 1,
                status_code: 200,
                is_streaming: false,
                data_source: "proxy".into(),
            }],
        )
        .await
        .unwrap();

        assert_eq!(response.accepted, vec!["node-a:request-1"]);
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
        server.abort();
    }

    #[tokio::test]
    async fn sync_drains_backlog_and_recovers_late_events() {
        let directory = tempfile::tempdir().unwrap();
        let source_path = directory.path().join("cc-switch.db");
        let mut source = Connection::open(&source_path).unwrap();
        source
            .execute_batch(
                "CREATE TABLE proxy_request_logs (
                    request_id TEXT PRIMARY KEY,
                    created_at INTEGER,
                    app_type TEXT,
                    provider_id TEXT,
                    model TEXT,
                    request_model TEXT,
                    pricing_model TEXT,
                    input_tokens INTEGER,
                    output_tokens INTEGER,
                    cache_read_tokens INTEGER,
                    cache_creation_tokens INTEGER,
                    input_token_semantics INTEGER,
                    total_cost_usd TEXT,
                    latency_ms INTEGER,
                    status_code INTEGER,
                    is_streaming INTEGER,
                    data_source TEXT
                );",
            )
            .unwrap();
        let transaction = source.transaction().unwrap();
        for index in 1..=1_201 {
            transaction
                .execute(
                    "INSERT INTO proxy_request_logs VALUES (
                        ?1,?2,'codex','provider','model','','',1,1,0,0,1,
                        '0',0,200,1,'proxy'
                    )",
                    rusqlite::params![format!("request-{index:04}"), index],
                )
                .unwrap();
        }
        transaction.commit().unwrap();

        let server_path = directory.path().join("server.db");
        let server_connection = telemetry_server::init_db(&server_path).unwrap();
        let (_, token) = telemetry_server::nodes::create(&server_connection, "node-a").unwrap();
        let server_state =
            telemetry_server::ServerState::new(server_connection, server_path.clone(), None);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let legacy_test_router = axum::Router::new()
                .route(
                    "/v1/events/batch",
                    axum::routing::post(telemetry_server::ingest),
                )
                .with_state(server_state);
            axum::serve(
                listener,
                legacy_test_router.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });

        let config = ClientConfig {
            cc_switch_db: source_path,
            server_url: format!("http://{address}"),
            auth_token: token,
            batch_size: 512,
            overlap_seconds: 600,
        };
        let mut cursor = Cursor::default();
        let initial = sync_available(&config, &mut cursor).await.unwrap();
        assert_eq!(initial.sent, 1_201);
        assert_eq!(initial.accepted, 1_201);
        assert_eq!(initial.duplicates, 0);
        assert!(initial.cursor_advanced);
        assert_eq!(cursor.created_at, 1_201);

        source
            .execute(
                "INSERT INTO proxy_request_logs VALUES (
                    'late-event',1000,'codex','provider','model','','',1,1,0,0,1,
                    '0',0,200,1,'session_log'
                )",
                [],
            )
            .unwrap();
        let cursor_before_late_scan = cursor.clone();
        let late = sync_available(&config, &mut cursor).await.unwrap();
        assert_eq!(late.sent, 602);
        assert_eq!(late.accepted, 1);
        assert_eq!(late.duplicates, 601);
        assert!(!late.cursor_advanced);
        assert_eq!(cursor, cursor_before_late_scan);

        let collected: i64 = Connection::open(&server_path)
            .unwrap()
            .query_row("SELECT COUNT(*) FROM usage_events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(collected, 1_202);
        server.abort();
    }

    #[tokio::test]
    async fn sync_drains_a_ten_thousand_row_client_ledger() {
        let directory = tempfile::tempdir().unwrap();
        let source_path = directory.path().join("client-ledger.db");
        let mut source = Connection::open(&source_path).unwrap();
        source.execute_batch("CREATE TABLE proxy_request_logs (request_id TEXT PRIMARY KEY, created_at INTEGER, app_type TEXT, provider_id TEXT, model TEXT, request_model TEXT, pricing_model TEXT, input_tokens INTEGER, output_tokens INTEGER, cache_read_tokens INTEGER, cache_creation_tokens INTEGER, input_token_semantics INTEGER, total_cost_usd TEXT, latency_ms INTEGER, status_code INTEGER, is_streaming INTEGER, data_source TEXT);").unwrap();
        let transaction = source.transaction().unwrap();
        for index in 1..=10_001 {
            transaction.execute("INSERT INTO proxy_request_logs VALUES (?1,?2,'codex','provider','model','','',1,1,0,0,1,'0',0,200,1,'codex_session')", rusqlite::params![format!("request-{index:05}"), index]).unwrap();
        }
        transaction.commit().unwrap();

        let server_path = directory.path().join("server.db");
        let server_connection = telemetry_server::init_db(&server_path).unwrap();
        let (_, token) = telemetry_server::nodes::create(&server_connection, "node-a").unwrap();
        let state =
            telemetry_server::ServerState::new(server_connection, server_path.clone(), None);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let legacy_test_router = axum::Router::new()
                .route(
                    "/v1/events/batch",
                    axum::routing::post(telemetry_server::ingest),
                )
                .with_state(state);
            axum::serve(
                listener,
                legacy_test_router.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });
        let config = ClientConfig {
            cc_switch_db: source_path,
            server_url: format!("http://{address}"),
            auth_token: token,
            batch_size: 512,
            overlap_seconds: 0,
        };
        let mut cursor = Cursor::default();
        let summary = sync_available(&config, &mut cursor).await.unwrap();
        assert_eq!(summary.sent, 10_001);
        assert_eq!(summary.accepted, 10_001);
        assert_eq!(summary.duplicates, 0);
        assert_eq!(cursor.created_at, 10_001);
        server.abort();
    }

    #[tokio::test]
    async fn protocol_v2_retains_old_detail_until_client_rollup_replaces_day() {
        let directory = tempfile::tempdir().unwrap();
        let server_path = directory.path().join("server.db");
        let server_db = telemetry_server::init_db(&server_path).unwrap();
        let (node, token) = telemetry_server::nodes::create(&server_db, "node-v2").unwrap();
        let state = telemetry_server::ServerState::new(server_db, server_path.clone(), None);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                telemetry_server::router(state)
                    .into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .unwrap();
        });

        let ledger = directory.path().join("ledger.db");
        usage_ledger::init_local_ledger(&ledger).unwrap();
        let day = chrono::Local::now().date_naive() - chrono::Duration::days(40);
        let (day_start, day_end) =
            cc_switch_usage_core::local_day_utc_bounds(&chrono::Local, day).unwrap();
        let created_at = day_start + 60;
        let connection = Connection::open(&ledger).unwrap();
        connection
            .execute(
                "INSERT INTO proxy_request_logs (
                     request_id,provider_id,app_type,model,input_tokens,output_tokens,
                     cache_read_tokens,cache_creation_tokens,input_token_semantics,
                     total_cost_usd,latency_ms,status_code,is_streaming,created_at,data_source
                 ) VALUES ('old-row','provider','codex','gpt-5',100,20,10,0,1,
                           '0.1',30,200,1,?1,'proxy')",
                [created_at],
            )
            .unwrap();
        drop(connection);
        let config = ClientConfig {
            cc_switch_db: ledger.clone(),
            server_url: format!("http://{address}"),
            auth_token: token,
            batch_size: 16,
            overlap_seconds: 0,
        };

        let first = sync_snapshot_v2(&config, &config, "local-compact")
            .await
            .unwrap();
        assert_eq!(first.inserted, 1);
        let connection = Connection::open(&server_path).unwrap();
        let initial: (i64, i64) = connection
            .query_row(
                "SELECT COUNT(*),MAX(input_tokens) FROM usage_events WHERE node_id=?1",
                [&node.uuid],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(initial, (1, 100));
        drop(connection);

        Connection::open(&ledger)
            .unwrap()
            .execute(
                "UPDATE proxy_request_logs SET input_tokens=125 WHERE request_id='old-row'",
                [],
            )
            .unwrap();
        let amended = sync_snapshot_v2(&config, &config, "local-compact")
            .await
            .unwrap();
        assert_eq!(amended.updated, 1, "historical amendment is an upsert");
        assert_eq!(amended.inserted, 0);
        let amended_value: i64 = Connection::open(&server_path)
            .unwrap()
            .query_row(
                "SELECT input_tokens FROM usage_events WHERE node_id=?1 AND request_id='old-row'",
                [&node.uuid],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(amended_value, 125);

        let mut ledger_connection = Connection::open(&ledger).unwrap();
        let transaction = ledger_connection.transaction().unwrap();
        transaction
            .execute(
                "INSERT INTO usage_daily_rollups (
                     date,app_type,provider_id,model,request_model,pricing_model,
                     request_count,success_count,input_tokens,output_tokens,cache_read_tokens,
                     cache_creation_tokens,input_token_semantics,total_cost_usd,avg_latency_ms
                 ) VALUES (?1,'codex','provider','gpt-5','','',1,1,115,20,10,0,2,'0.1',30)",
                [day.format("%Y-%m-%d").to_string()],
            )
            .unwrap();
        transaction
            .execute(
                "DELETE FROM proxy_request_logs WHERE request_id='old-row'",
                [],
            )
            .unwrap();
        transaction.commit().unwrap();
        let rolled = sync_snapshot_v2(&config, &config, "local-compact")
            .await
            .unwrap();
        assert_eq!(rolled.rollups, 1);
        assert_eq!(rolled.deleted, 1);
        let connection = Connection::open(&server_path).unwrap();
        let detail_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM usage_events WHERE node_id=?1",
                [&node.uuid],
                |row| row.get(0),
            )
            .unwrap();
        let rollup_bounds: (i64, i64) = connection
            .query_row(
                "SELECT day_start_utc,day_end_utc FROM usage_daily_snapshots WHERE node_id=?1",
                [&node.uuid],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(detail_count, 0);
        assert_eq!(rollup_bounds, (day_start, day_end));

        let bad_generation = uuid::Uuid::new_v4().to_string();
        let http = reqwest::Client::new();
        let begin = http
            .post(format!("{}/v2/sync/begin", config.server_url))
            .bearer_auth(&config.auth_token)
            .json(&SyncBeginRequest {
                schema_version: SCHEMA_VERSION,
                generation_id: bad_generation.clone(),
                source_kind: "local-compact".into(),
                replace_all: false,
            })
            .send()
            .await
            .unwrap();
        assert_eq!(begin.status(), reqwest::StatusCode::CREATED);
        let providers = http
            .post(format!("{}/v2/sync/providers", config.server_url))
            .bearer_auth(&config.auth_token)
            .json(&ProviderMutationBatch {
                schema_version: SCHEMA_VERSION,
                generation_id: bad_generation.clone(),
                providers: Vec::new(),
            })
            .send()
            .await
            .unwrap();
        assert_eq!(providers.status(), reqwest::StatusCode::NO_CONTENT);
        let rejected = http
            .post(format!("{}/v2/sync/commit", config.server_url))
            .bearer_auth(&config.auth_token)
            .json(&SyncCommitRequest {
                schema_version: SCHEMA_VERSION,
                generation_id: bad_generation.clone(),
                expected_event_mutations: 0,
                expected_rollup_mutations: 0,
                manifest_hash: "not-the-staged-manifest".into(),
            })
            .send()
            .await
            .unwrap();
        assert_eq!(rejected.status(), reqwest::StatusCode::CONFLICT);
        let generation_status: String = Connection::open(&server_path)
            .unwrap()
            .query_row(
                "SELECT status FROM sync_generations WHERE generation_id=?1",
                [&bad_generation],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(generation_status, "open");
        server.abort();
    }
}

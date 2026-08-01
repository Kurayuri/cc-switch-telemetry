//! Local detail-only usage ledger backed by the shared session importers.
//!
//! `session-usage-core` owns raw-file semantics. This module owns only the
//! telemetry-specific SQLite schema, persistence, rebuild lifecycle, and the
//! synthetic provider catalog used when no cc-switch DB is available.

use crate::{read_events, ClientConfig, Cursor};
use anyhow::Context;
use rusqlite::{params, Connection};
use serde::Deserialize;
use session_usage_core::{ImportReport, SourceConfig, UsageRecord, IMPORTER_REVISION};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use telemetry_core::UsageEvent;

#[derive(Debug, Clone)]
pub struct LocalUsageConfig {
    pub database: PathBuf,
    pub claude_dir: PathBuf,
    pub codex_dir: PathBuf,
    pub gemini_dir: PathBuf,
    pub opencode_db: PathBuf,
    pub grok_dir: PathBuf,
}

impl LocalUsageConfig {
    pub fn from_env() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let path = |name: &str, default: PathBuf| {
            std::env::var_os(name).map(PathBuf::from).unwrap_or(default)
        };
        Self {
            database: path(
                "TELEMETRY_LOCAL_USAGE_DB",
                PathBuf::from("./data/local-usage.db"),
            ),
            claude_dir: path("TELEMETRY_CLAUDE_DIR", home.join(".claude")),
            codex_dir: path("TELEMETRY_CODEX_DIR", home.join(".codex")),
            gemini_dir: path("TELEMETRY_GEMINI_DIR", home.join(".gemini")),
            opencode_db: path(
                "TELEMETRY_OPENCODE_DB",
                home.join(".local/share/opencode/opencode.db"),
            ),
            grok_dir: path("TELEMETRY_GROK_DIR", home.join(".grok")),
        }
    }

    fn sources(&self) -> SourceConfig {
        SourceConfig {
            claude_dir: self.claude_dir.clone(),
            codex_dir: self.codex_dir.clone(),
            gemini_dir: self.gemini_dir.clone(),
            opencode_db: self.opencode_db.clone(),
            grok_dir: self.grok_dir.clone(),
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RebuildSummary {
    pub imported: u64,
    pub skipped: u64,
}

#[derive(Clone, Copy)]
struct Pricing {
    input: f64,
    output: f64,
    cache_read: f64,
    cache_creation: f64,
}

#[cfg(not(test))]
const MODELS_DEV_URL: &str = "https://models.dev/api.json";

#[derive(Debug, Deserialize)]
struct ModelsDevProvider {
    #[serde(default)]
    models: HashMap<String, ModelsDevModel>,
}

#[derive(Debug, Deserialize)]
struct ModelsDevModel {
    #[serde(default)]
    cost: Option<ModelsDevCost>,
}

#[derive(Debug, Deserialize)]
struct ModelsDevCost {
    input: Option<f64>,
    output: Option<f64>,
    cache_read: Option<f64>,
    cache_write: Option<f64>,
}

fn pricing_candidates(model: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut add = |s: String| {
        if !s.is_empty() && !out.contains(&s) {
            out.push(s);
        }
    };
    let base = model
        .rsplit_once('/')
        .map_or(model, |(_, v)| v)
        .split(':')
        .next()
        .unwrap_or(model)
        .trim()
        .replace('@', "-")
        .to_ascii_lowercase();
    add(base.clone());
    if let Some(pos) = base.rfind("claude-") {
        if pos > 0 {
            add(base[pos..].to_string());
        }
    }
    for prefix in ["openai.", "anthropic.", "google.", "bedrock.", "global."] {
        if let Some(v) = base.strip_prefix(prefix) {
            add(v.to_string());
        }
    }
    if let Some((head, suffix)) = base.rsplit_once('-') {
        if suffix.len() == 8 && suffix.chars().all(|c| c.is_ascii_digit()) {
            add(head.to_string());
        }
    }
    if let Some(v) = base.strip_suffix("-thinking") {
        add(v.to_string());
    }
    out
}

#[cfg(not(test))]
async fn load_pricing() -> anyhow::Result<HashMap<String, Pricing>> {
    let url =
        std::env::var("TELEMETRY_MODELS_DEV_URL").unwrap_or_else(|_| MODELS_DEV_URL.to_owned());
    let response = reqwest::Client::new()
        .get(&url)
        .send()
        .await
        .with_context(|| format!("fetch models.dev pricing from {url}"))?
        .error_for_status()
        .with_context(|| format!("models.dev pricing request failed: {url}"))?;
    let providers: HashMap<String, ModelsDevProvider> = response
        .json()
        .await
        .context("decode models.dev pricing JSON")?;
    parse_pricing(providers)
}

fn parse_pricing(
    providers: HashMap<String, ModelsDevProvider>,
) -> anyhow::Result<HashMap<String, Pricing>> {
    let mut map = HashMap::new();
    for provider in providers.values() {
        for (model, details) in &provider.models {
            let Some(cost) = details.cost.as_ref() else {
                continue;
            };
            if cost.input.is_none() && cost.output.is_none() {
                continue;
            }
            let pricing = Pricing {
                input: valid_price(cost.input),
                output: valid_price(cost.output),
                cache_read: valid_price(cost.cache_read),
                cache_creation: valid_price(cost.cache_write),
            };
            for candidate in pricing_candidates(model) {
                map.insert(candidate, pricing);
            }
        }
    }
    if map.is_empty() {
        anyhow::bail!("models.dev pricing response contains no usable models");
    }
    Ok(map)
}

#[cfg(test)]
async fn load_pricing() -> anyhow::Result<HashMap<String, Pricing>> {
    Ok(HashMap::new())
}

fn valid_price(value: Option<f64>) -> f64 {
    value
        .filter(|value| value.is_finite() && *value >= 0.0)
        .unwrap_or(0.0)
}

fn find_pricing(map: &HashMap<String, Pricing>, model: &str) -> Option<Pricing> {
    for candidate in pricing_candidates(model) {
        if let Some(p) = map.get(&candidate) {
            return Some(*p);
        }
        let mut matches = map
            .iter()
            .filter(|(key, _)| key.starts_with(&(candidate.clone() + "-")));
        if let Some((_, p)) = matches.next() {
            return Some(*p);
        }
    }
    None
}

fn is_placeholder(model: &str) -> bool {
    matches!(
        model.trim().to_ascii_lowercase().as_str(),
        "" | "unknown" | "null" | "none"
    )
}

fn calculate_cost(record: &UsageRecord, pricing: Option<Pricing>) -> ([String; 4], String) {
    let Some(p) = pricing else {
        return (["0".into(), "0".into(), "0".into(), "0".into()], "0".into());
    };
    let inclusive = matches!(record.app_type.as_str(), "codex" | "gemini" | "grokbuild");
    let fresh = if inclusive {
        record
            .input_tokens
            .saturating_sub(record.cache_read_tokens)
            .saturating_sub(record.cache_creation_tokens)
    } else {
        record.input_tokens
    };
    let vals = [
        fresh as f64 * p.input / 1e6,
        record.output_tokens as f64 * p.output / 1e6,
        record.cache_read_tokens as f64 * p.cache_read / 1e6,
        record.cache_creation_tokens as f64 * p.cache_creation / 1e6,
    ];
    let total = vals.iter().sum::<f64>();
    (vals.map(|v| v.to_string()), total.to_string())
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS proxy_request_logs (
 request_id TEXT PRIMARY KEY, provider_id TEXT NOT NULL, app_type TEXT NOT NULL, model TEXT NOT NULL,
 request_model TEXT, pricing_model TEXT, input_tokens INTEGER NOT NULL DEFAULT 0, output_tokens INTEGER NOT NULL DEFAULT 0,
 cache_read_tokens INTEGER NOT NULL DEFAULT 0, cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
 input_token_semantics INTEGER NOT NULL DEFAULT 0, input_cost_usd TEXT NOT NULL DEFAULT '0',
 output_cost_usd TEXT NOT NULL DEFAULT '0', cache_read_cost_usd TEXT NOT NULL DEFAULT '0',
 cache_creation_cost_usd TEXT NOT NULL DEFAULT '0', total_cost_usd TEXT NOT NULL DEFAULT '0',
 latency_ms INTEGER NOT NULL DEFAULT 0, first_token_ms INTEGER, duration_ms INTEGER, status_code INTEGER NOT NULL DEFAULT 200,
 error_message TEXT, session_id TEXT, provider_type TEXT, is_streaming INTEGER NOT NULL DEFAULT 1,
 cost_multiplier TEXT NOT NULL DEFAULT '1.0', created_at INTEGER NOT NULL, data_source TEXT NOT NULL DEFAULT 'proxy'
);
CREATE INDEX IF NOT EXISTS idx_request_logs_provider ON proxy_request_logs(provider_id, app_type);
CREATE INDEX IF NOT EXISTS idx_request_logs_created_at ON proxy_request_logs(created_at);
CREATE INDEX IF NOT EXISTS idx_request_logs_model ON proxy_request_logs(model);
CREATE INDEX IF NOT EXISTS idx_request_logs_session ON proxy_request_logs(session_id);
CREATE INDEX IF NOT EXISTS idx_request_logs_status ON proxy_request_logs(status_code);
CREATE INDEX IF NOT EXISTS idx_request_logs_app_created_at ON proxy_request_logs(app_type, created_at DESC);
CREATE TABLE IF NOT EXISTS session_log_sync (
 file_path TEXT PRIMARY KEY, last_modified INTEGER NOT NULL, last_line_offset INTEGER NOT NULL DEFAULT 0,
 last_synced_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS ledger_meta (
 key TEXT PRIMARY KEY, value TEXT NOT NULL
);"#;

pub fn init_local_ledger(path: &Path) -> anyhow::Result<()> {
    let existed = path.exists();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let conn = Connection::open(path)?;
    conn.execute_batch(SCHEMA)?;
    let had_detail = existed
        && conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name = 'proxy_request_logs')",
            [],
            |row| row.get::<_, bool>(0),
        )?;
    conn.execute(
        "INSERT INTO ledger_meta(key, value) VALUES ('importer_revision', ?1)
         ON CONFLICT(key) DO NOTHING",
        [if had_detail {
            "legacy"
        } else {
            IMPORTER_REVISION
        }],
    )?;
    Ok(())
}

fn verify_revision(conn: &Connection) -> anyhow::Result<()> {
    let revision: String = conn.query_row(
        "SELECT value FROM ledger_meta WHERE key = 'importer_revision'",
        [],
        |row| row.get(0),
    )?;
    if revision != IMPORTER_REVISION {
        anyhow::bail!(
            "local usage ledger importer revision is {revision}, expected {IMPORTER_REVISION}; run rebuild"
        );
    }
    Ok(())
}

pub async fn rebuild(config: &LocalUsageConfig) -> anyhow::Result<RebuildSummary> {
    if let Some(parent) = config.database.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp = config.database.with_extension("rebuild.sqlite");
    if temp.exists() {
        fs::remove_file(&temp)?;
    }
    init_local_ledger(&temp)?;
    let summary = sync_into(&temp, config).await?;
    let integrity: String = Connection::open(&temp)?
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .context("validate rebuilt local usage ledger")?;
    if integrity != "ok" {
        anyhow::bail!("rebuilt local usage ledger failed integrity_check: {integrity}");
    }
    fs::rename(&temp, &config.database)?;
    Ok(summary)
}

pub async fn sync_local(config: &LocalUsageConfig) -> anyhow::Result<RebuildSummary> {
    init_local_ledger(&config.database)?;
    sync_into(&config.database, config).await
}

/// Mirrors the cc-switch detail table into the Client-owned ledger. Uploading
/// is always resumed from the ledger with its own independent cursor.
pub fn sync_cc_switch(
    source_config: &ClientConfig,
    ledger_path: &Path,
) -> anyhow::Result<RebuildSummary> {
    init_local_ledger(ledger_path)?;
    let conn = Connection::open(ledger_path)?;
    // cc-switch can amend a previously imported session row without changing
    // its original created_at. Scan the source from the beginning on each
    // observed DB/WAL change so the Client ledger remains a complete mirror.
    let mut cursor = Cursor::default();
    let mut summary = RebuildSummary::default();
    loop {
        let events = read_events(source_config, &cursor)?;
        if events.is_empty() {
            break;
        }
        let transaction = conn.unchecked_transaction()?;
        for event in &events {
            if insert_event(&transaction, event)? {
                summary.imported += 1;
            } else {
                summary.skipped += 1;
            }
        }
        let last = events.last().expect("non-empty event batch");
        cursor = Cursor {
            created_at: last.created_at,
            request_id: last.request_id.clone(),
        };
        transaction.commit()?;
        if events.len() < source_config.batch_size {
            break;
        }
    }
    Ok(summary)
}

async fn sync_into(path: &Path, config: &LocalUsageConfig) -> anyhow::Result<RebuildSummary> {
    let mut conn = Connection::open(path)?;
    verify_revision(&conn)?;
    let mut sync_paths = std::collections::HashMap::<String, i64>::new();
    {
        let mut statement =
            conn.prepare("SELECT file_path, last_modified FROM session_log_sync")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        for row in rows.flatten() {
            sync_paths.insert(row.0, row.1);
        }
    }
    let pricing = load_pricing().await?;
    let report = session_usage_core::import_all_filtered(&config.sources(), |source| {
        let modified = fs::metadata(source)
            .ok()
            .and_then(|meta| meta.modified().ok())
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_nanos().min(i64::MAX as u128) as i64)
            .unwrap_or(0);
        modified
            > sync_paths
                .get(&source.to_string_lossy().to_string())
                .copied()
                .unwrap_or(0)
    })?;
    let mut summary = RebuildSummary {
        imported: 0,
        skipped: report.skipped,
    };
    let transaction = conn.transaction()?;
    for record in &report.records {
        if insert_record(&transaction, record, &pricing)? {
            summary.imported += 1;
        } else {
            summary.skipped += 1;
        }
    }
    transaction.commit()?;
    record_scan_metadata(&conn, report)?;
    Ok(summary)
}

fn insert_record(
    conn: &Connection,
    record: &UsageRecord,
    pricing: &HashMap<String, Pricing>,
) -> anyhow::Result<bool> {
    let model_for_pricing = record
        .pricing_model
        .as_deref()
        .filter(|m| !is_placeholder(m))
        .or_else(|| (!is_placeholder(&record.model)).then_some(record.model.as_str()))
        .or(Some(record.request_model.as_str()));
    let cost = calculate_cost(
        record,
        model_for_pricing.and_then(|m| find_pricing(pricing, m)),
    );
    let total = record
        .reported_total_cost_usd
        .as_deref()
        .filter(|v| v.parse::<f64>().ok().is_some_and(|n| n > 0.0))
        .unwrap_or(&cost.1);
    insert_values(
        conn,
        &record.request_id,
        &record.provider_id,
        &record.app_type,
        &record.model,
        Some(&record.request_model),
        record.pricing_model.as_deref(),
        record.input_tokens,
        record.output_tokens,
        record.cache_read_tokens,
        record.cache_creation_tokens,
        record.input_token_semantics,
        &cost.0,
        total,
        record.latency_ms,
        record.status_code,
        record.session_id.as_deref(),
        Some(&record.provider_type),
        record.is_streaming,
        record.created_at,
        &record.data_source,
    )
}

fn insert_event(conn: &Connection, event: &UsageEvent) -> anyhow::Result<bool> {
    insert_values(
        conn,
        &event.request_id,
        &event.provider_id,
        &event.app_type,
        &event.model,
        event.request_model.as_deref(),
        event.pricing_model.as_deref(),
        event.input_tokens,
        event.output_tokens,
        event.cache_read_tokens,
        event.cache_creation_tokens,
        event.input_token_semantics,
        &["0".into(), "0".into(), "0".into(), "0".into()],
        &event.total_cost_usd,
        event.latency_ms,
        event.status_code,
        None,
        None,
        event.is_streaming,
        event.created_at,
        &event.data_source,
    )
}

#[allow(clippy::too_many_arguments)]
fn insert_values(
    conn: &Connection,
    request_id: &str,
    provider_id: &str,
    app_type: &str,
    model: &str,
    request_model: Option<&str>,
    pricing_model: Option<&str>,
    input_tokens: i64,
    output_tokens: i64,
    cache_read_tokens: i64,
    cache_creation_tokens: i64,
    input_token_semantics: i64,
    component_costs: &[String; 4],
    total_cost_usd: &str,
    latency_ms: i64,
    status_code: i64,
    session_id: Option<&str>,
    provider_type: Option<&str>,
    is_streaming: bool,
    created_at: i64,
    data_source: &str,
) -> anyhow::Result<bool> {
    let changed = conn.execute(
        "INSERT INTO proxy_request_logs (
            request_id, provider_id, app_type, model, request_model, pricing_model,
            input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens,
            input_token_semantics, input_cost_usd, output_cost_usd, cache_read_cost_usd,
            cache_creation_cost_usd, total_cost_usd, latency_ms, first_token_ms, duration_ms,
            status_code, error_message, session_id, provider_type, is_streaming,
            cost_multiplier, created_at, data_source
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                   ?17, NULL, NULL, ?18, NULL, ?19, ?20, ?21, '1.0', ?22, ?23)
        ON CONFLICT(request_id) DO UPDATE SET
            provider_id = excluded.provider_id,
            app_type = excluded.app_type,
            model = excluded.model,
            request_model = excluded.request_model,
            pricing_model = excluded.pricing_model,
            input_tokens = excluded.input_tokens,
            output_tokens = excluded.output_tokens,
            cache_read_tokens = excluded.cache_read_tokens,
            cache_creation_tokens = excluded.cache_creation_tokens,
            input_token_semantics = excluded.input_token_semantics,
            input_cost_usd = excluded.input_cost_usd,
            output_cost_usd = excluded.output_cost_usd,
            cache_read_cost_usd = excluded.cache_read_cost_usd,
            cache_creation_cost_usd = excluded.cache_creation_cost_usd,
            total_cost_usd = excluded.total_cost_usd,
            latency_ms = excluded.latency_ms,
            status_code = excluded.status_code,
            is_streaming = excluded.is_streaming,
            data_source = excluded.data_source
        WHERE proxy_request_logs.data_source = excluded.data_source
          AND (proxy_request_logs.provider_id IS NOT excluded.provider_id
               OR proxy_request_logs.app_type IS NOT excluded.app_type
               OR proxy_request_logs.model IS NOT excluded.model
               OR proxy_request_logs.request_model IS NOT excluded.request_model
               OR proxy_request_logs.pricing_model IS NOT excluded.pricing_model
               OR proxy_request_logs.input_tokens IS NOT excluded.input_tokens
               OR proxy_request_logs.output_tokens IS NOT excluded.output_tokens
               OR proxy_request_logs.cache_read_tokens IS NOT excluded.cache_read_tokens
               OR proxy_request_logs.cache_creation_tokens IS NOT excluded.cache_creation_tokens
               OR proxy_request_logs.input_token_semantics IS NOT excluded.input_token_semantics
               OR proxy_request_logs.total_cost_usd IS NOT excluded.total_cost_usd
               OR proxy_request_logs.latency_ms IS NOT excluded.latency_ms
               OR proxy_request_logs.status_code IS NOT excluded.status_code
               OR proxy_request_logs.is_streaming IS NOT excluded.is_streaming)",
        params![
            request_id,
            provider_id,
            app_type,
            model,
            request_model,
            pricing_model,
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_creation_tokens,
            input_token_semantics,
            component_costs[0],
            component_costs[1],
            component_costs[2],
            component_costs[3],
            total_cost_usd,
            latency_ms,
            status_code,
            session_id,
            provider_type,
            is_streaming as i64,
            created_at,
            data_source,
        ],
    )?;
    Ok(changed > 0)
}

fn record_scan_metadata(conn: &Connection, report: ImportReport) -> anyhow::Result<()> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    let mut paths = report.scanned_paths;
    paths.sort();
    paths.dedup();
    for path in paths {
        if report
            .deferred_paths
            .iter()
            .any(|deferred| deferred == &path)
        {
            continue;
        }
        let modified = fs::metadata(&path)
            .ok()
            .and_then(|meta| meta.modified().ok())
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_nanos().min(i64::MAX as u128) as i64)
            .unwrap_or(0);
        conn.execute(
            "INSERT INTO session_log_sync(file_path,last_modified,last_line_offset,last_synced_at)
             VALUES (?1,?2,0,?3) ON CONFLICT(file_path) DO UPDATE SET last_modified=excluded.last_modified,last_synced_at=excluded.last_synced_at",
            params![path.to_string_lossy(), modified, now],
        )?;
    }
    Ok(())
}

pub fn local_provider_snapshot(node_id: String) -> telemetry_core::ProviderSnapshot {
    let providers = [
        ("claude", "_session", "Claude session"),
        ("codex", "_codex_session", "Codex session"),
        ("gemini", "_gemini_session", "Gemini session"),
        ("opencode", "_opencode_session", "OpenCode session"),
        ("grokbuild", "_grokbuild_session", "Grok Build session"),
    ]
    .into_iter()
    .map(
        |(app_type, provider_id, name)| telemetry_core::ProviderEntry {
            app_type: app_type.into(),
            provider_id: provider_id.into(),
            name: name.into(),
        },
    )
    .collect();
    telemetry_core::ProviderSnapshot {
        schema_version: telemetry_core::SCHEMA_VERSION,
        node_id,
        providers,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(dir: &Path) -> LocalUsageConfig {
        LocalUsageConfig {
            database: dir.join("usage.db"),
            claude_dir: dir.join("claude"),
            codex_dir: dir.join("codex"),
            gemini_dir: dir.join("gemini"),
            opencode_db: dir.join("opencode.db"),
            grok_dir: dir.join("grok"),
        }
    }

    #[test]
    fn models_dev_prices_are_flattened_and_normalized() {
        let providers: HashMap<String, ModelsDevProvider> = serde_json::from_str(r#"{
          "openai": {"models": {
            "Vendor/GPT-5@2025:free": {"cost": {"input": 2.5, "output": 15.0, "cache_read": 0.25, "cache_write": 3.125}},
            "free": {"cost": {}}
          }}
        }"#).unwrap();
        let prices = parse_pricing(providers).unwrap();
        assert_eq!(prices.get("gpt-5-2025").unwrap().output, 15.0);
        assert_eq!(prices.get("gpt-5-2025").unwrap().cache_creation, 3.125);
        assert!(!prices.contains_key("free"));
    }

    #[test]
    fn local_schema_has_detail_but_no_rollup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.db");
        init_local_ledger(&path).unwrap();
        let conn = Connection::open(path).unwrap();
        let detail: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='proxy_request_logs')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let rollup: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='usage_daily_rollups')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(detail);
        assert!(!rollup);
    }

    #[tokio::test]
    async fn rebuild_materializes_codex_usage_and_replaces_target() {
        let dir = tempfile::tempdir().unwrap();
        let sessions = dir.path().join("codex/sessions/2026/07/30");
        fs::create_dir_all(&sessions).unwrap();
        fs::write(sessions.join("rollout-019c6e27-e55b-73d1-87d8-4e01f1f75043.jsonl"), r#"{"type":"session_meta","timestamp":"2026-07-30T00:00:00Z","payload":{"id":"019c6e27-e55b-73d1-87d8-4e01f1f75043"}}
{"type":"turn_context","payload":{"model":"gpt-5"}}
{"type":"event_msg","timestamp":"2026-07-30T00:00:00Z","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":12,"output_tokens":3,"cached_input_tokens":2}}}}"#).unwrap();
        let local = config(dir.path());
        let summary = rebuild(&local).await.unwrap();
        assert_eq!(summary.imported, 1);
        let conn = Connection::open(local.database).unwrap();
        let row: (String, i64, i64, i64) = conn.query_row("SELECT app_type,input_tokens,output_tokens,cache_read_tokens FROM proxy_request_logs", [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))).unwrap();
        assert_eq!(row, ("codex".into(), 12, 3, 2));
    }

    #[tokio::test]
    async fn local_sync_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let sessions = dir.path().join("codex/sessions/2026/07/30");
        fs::create_dir_all(&sessions).unwrap();
        fs::write(sessions.join("rollout.jsonl"), r#"{"type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":1,"output_tokens":1}}}}"#).unwrap();
        let local = config(dir.path());
        rebuild(&local).await.unwrap();
        assert_eq!(sync_local(&local).await.unwrap().imported, 0);
    }

    #[test]
    fn local_provider_snapshot_is_available_without_cc_switch_tables() {
        let snapshot = local_provider_snapshot("node-a".into());
        assert_eq!(snapshot.providers.len(), 5);
        assert!(snapshot
            .providers
            .iter()
            .any(|entry| entry.provider_id == "_codex_session"));
    }

    #[test]
    fn cc_switch_rows_are_mirrored_into_the_client_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("cc-switch.db");
        let source_conn = Connection::open(&source).unwrap();
        source_conn
            .execute_batch(
                "CREATE TABLE proxy_request_logs (
                    request_id TEXT PRIMARY KEY, created_at INTEGER, app_type TEXT,
                    provider_id TEXT, model TEXT, request_model TEXT, pricing_model TEXT,
                    input_tokens INTEGER, output_tokens INTEGER, cache_read_tokens INTEGER,
                    cache_creation_tokens INTEGER, input_token_semantics INTEGER,
                    total_cost_usd TEXT, latency_ms INTEGER, status_code INTEGER,
                    is_streaming INTEGER, data_source TEXT
                );
                INSERT INTO proxy_request_logs VALUES
                    ('request-a', 10, 'codex', 'provider', 'model', '', '', 1, 2, 0, 0, 1, '0', 3, 200, 1, 'proxy'),
                    ('request-b', 11, 'codex', 'provider', 'model', '', '', 4, 5, 0, 0, 1, '0', 6, 200, 1, 'proxy');",
            )
            .unwrap();
        let local = config(dir.path());
        let source_config = ClientConfig {
            cc_switch_db: source,
            server_url: "http://localhost".into(),
            node_id: "node-a".into(),
            auth_token: None,
            batch_size: 1,
            overlap_seconds: 0,
        };

        let first = sync_cc_switch(&source_config, &local.database).unwrap();
        assert_eq!(first.imported, 2);
        assert_eq!(first.skipped, 0);
        let second = sync_cc_switch(&source_config, &local.database).unwrap();
        assert_eq!(second.imported, 0);
        let count: i64 = Connection::open(&local.database)
            .unwrap()
            .query_row("SELECT COUNT(*) FROM proxy_request_logs", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 2);
    }
}

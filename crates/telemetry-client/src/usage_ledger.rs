//! Client-owned usage ledger and source mirror.
//!
//! The unified ledger accepts cc-switch and local collectors. Explicit `local` mode uses
//! the vendored six-source `session-usage-core` adapters and retains every
//! imported detail row. `local-compact` applies the shared 30-day retention
//! policy after importing. This module also owns the durable v3 upload-hash
//! baseline and synthetic provider catalog.

use crate::{read_events, ClientConfig, Cursor};
use anyhow::Context;
use chrono::{Local, NaiveDate};
use rusqlite::{params, Connection, OptionalExtension};
use rust_decimal::Decimal;
use serde::Deserialize;
use session_usage_core::{ImportReport, SourceConfig, UsageRecord, IMPORTER_REVISION};
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::{Path, PathBuf},
    str::FromStr,
    time::{SystemTime, UNIX_EPOCH},
};
use telemetry_core::{ProviderEntry, RollupSnapshot, UsageEvent, SCHEMA_VERSION};

const LEDGER_SCHEMA_REVISION: &str = "unified-app-key-v3";

fn expected_revision() -> String {
    format!("{IMPORTER_REVISION}:{LEDGER_SCHEMA_REVISION}")
}

#[derive(Debug, Clone)]
pub struct LocalUsageConfig {
    pub database: PathBuf,
    pub claude_dir: PathBuf,
    pub codex_dir: PathBuf,
    pub gemini_dir: PathBuf,
    pub opencode_db: PathBuf,
    pub grok_dir: PathBuf,
    pub pi_dir: PathBuf,
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
            pi_dir: path("TELEMETRY_PI_SESSION_DIR", home.join(".pi/agent/sessions")),
        }
    }

    fn sources(&self) -> SourceConfig {
        SourceConfig {
            claude_dir: self.claude_dir.clone(),
            codex_dir: self.codex_dir.clone(),
            gemini_dir: self.gemini_dir.clone(),
            opencode_db: self.opencode_db.clone(),
            grok_dir: self.grok_dir.clone(),
            pi_dir: self.pi_dir.clone(),
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RebuildSummary {
    pub imported: u64,
    pub skipped: u64,
}

type Pricing = cc_switch_usage_core::ModelPricing;

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
    input: Option<serde_json::Number>,
    output: Option<serde_json::Number>,
    cache_read: Option<serde_json::Number>,
    cache_write: Option<serde_json::Number>,
}

use cc_switch_usage_core::pricing_candidates;

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
                input_cost_per_million: valid_price(cost.input.as_ref()),
                output_cost_per_million: valid_price(cost.output.as_ref()),
                cache_read_cost_per_million: valid_price(cost.cache_read.as_ref()),
                cache_creation_cost_per_million: valid_price(cost.cache_write.as_ref()),
            };
            for candidate in pricing_candidates(model) {
                map.insert(candidate, pricing.clone());
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

fn valid_price(value: Option<&serde_json::Number>) -> Decimal {
    value
        .and_then(|number| Decimal::from_str(&number.to_string()).ok())
        .filter(|value| *value >= Decimal::ZERO)
        .unwrap_or(Decimal::ZERO)
}

fn find_pricing<'a>(map: &'a HashMap<String, Pricing>, model: &str) -> Option<&'a Pricing> {
    for candidate in pricing_candidates(model) {
        if let Some(p) = map.get(&candidate) {
            return Some(p);
        }
        let mut matches = map
            .iter()
            .filter(|(key, _)| key.starts_with(&(candidate.clone() + "-")));
        if let Some((_, p)) = matches.next() {
            return Some(p);
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

fn calculate_cost(record: &UsageRecord, pricing: Option<&Pricing>) -> ([String; 4], String) {
    let Some(p) = pricing else {
        return (["0".into(), "0".into(), "0".into(), "0".into()], "0".into());
    };
    let mut cost = cc_switch_usage_core::calculate_cost(
        &record.app_type,
        cc_switch_usage_core::TokenCounts {
            input_tokens: record.input_tokens,
            output_tokens: record.output_tokens,
            cache_read_tokens: record.cache_read_tokens,
            cache_creation_tokens: record.cache_creation_tokens,
        },
        p,
        Decimal::ONE,
    );
    if let Some(factors) = cc_switch_usage_core::fast_pricing::factors(
        record.pricing_model.as_deref().unwrap_or(&record.model),
        record.service_tier.as_deref(),
        0,
    ) {
        cc_switch_usage_core::fast_pricing::apply(&mut cost, factors, Decimal::ONE);
    }
    (
        [
            cost.input_cost.to_string(),
            cost.output_cost.to_string(),
            cost.cache_read_cost.to_string(),
            cost.cache_creation_cost.to_string(),
        ],
        cost.total_cost.to_string(),
    )
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS proxy_request_logs (
 request_id TEXT NOT NULL, provider_id TEXT NOT NULL, app_type TEXT NOT NULL, model TEXT NOT NULL,
 request_model TEXT, pricing_model TEXT, input_tokens INTEGER NOT NULL DEFAULT 0, output_tokens INTEGER NOT NULL DEFAULT 0,
 cache_read_tokens INTEGER NOT NULL DEFAULT 0, cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
 input_token_semantics INTEGER NOT NULL DEFAULT 0, input_cost_usd TEXT NOT NULL DEFAULT '0',
 output_cost_usd TEXT NOT NULL DEFAULT '0', cache_read_cost_usd TEXT NOT NULL DEFAULT '0',
 cache_creation_cost_usd TEXT NOT NULL DEFAULT '0', total_cost_usd TEXT NOT NULL DEFAULT '0',
 latency_ms INTEGER NOT NULL DEFAULT 0, first_token_ms INTEGER, duration_ms INTEGER, status_code INTEGER NOT NULL DEFAULT 200,
 error_message TEXT, session_id TEXT, provider_type TEXT, is_streaming INTEGER NOT NULL DEFAULT 1,
 cost_multiplier TEXT NOT NULL DEFAULT '1.0', created_at INTEGER NOT NULL, data_source TEXT NOT NULL DEFAULT 'proxy',
 last_collector TEXT NOT NULL DEFAULT 'unknown',
 PRIMARY KEY (app_type, request_id)
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
CREATE TABLE IF NOT EXISTS session_usage_dedup (
 data_source TEXT NOT NULL, request_id TEXT NOT NULL, semantic_id TEXT NOT NULL,
 has_entry_id INTEGER NOT NULL DEFAULT 0,
 PRIMARY KEY (data_source, request_id)
);
CREATE INDEX IF NOT EXISTS idx_session_usage_dedup_semantic
 ON session_usage_dedup(data_source, semantic_id);
CREATE TABLE IF NOT EXISTS usage_daily_rollups (
 date TEXT NOT NULL, app_type TEXT NOT NULL, provider_id TEXT NOT NULL, model TEXT NOT NULL,
 request_model TEXT NOT NULL DEFAULT '', pricing_model TEXT NOT NULL DEFAULT '',
 request_count INTEGER NOT NULL DEFAULT 0, success_count INTEGER NOT NULL DEFAULT 0,
 input_tokens INTEGER NOT NULL DEFAULT 0, output_tokens INTEGER NOT NULL DEFAULT 0,
 cache_read_tokens INTEGER NOT NULL DEFAULT 0, cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
 input_token_semantics INTEGER NOT NULL DEFAULT 2, total_cost_usd TEXT NOT NULL DEFAULT '0',
 avg_latency_ms INTEGER NOT NULL DEFAULT 0, last_collector TEXT NOT NULL DEFAULT 'unknown',
 PRIMARY KEY (date, app_type, provider_id, model, request_model, pricing_model)
);
CREATE TABLE IF NOT EXISTS compaction_barriers (
 day_start_utc INTEGER NOT NULL, day_end_utc INTEGER NOT NULL,
 PRIMARY KEY (day_start_utc, day_end_utc)
);
CREATE TABLE IF NOT EXISTS client_provider_catalog (
 app_type TEXT NOT NULL, provider_id TEXT NOT NULL, name TEXT NOT NULL,
 last_collector TEXT NOT NULL,
 PRIMARY KEY (app_type, provider_id)
);
CREATE TABLE IF NOT EXISTS ledger_meta (
 key TEXT PRIMARY KEY, value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS upload_remotes (
 remote_key TEXT PRIMARY KEY, updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS upload_hashes (
 remote_key TEXT NOT NULL, entity_kind TEXT NOT NULL, entity_key TEXT NOT NULL,
 content_hash TEXT NOT NULL,
 PRIMARY KEY (remote_key, entity_kind, entity_key),
 FOREIGN KEY (remote_key) REFERENCES upload_remotes(remote_key) ON DELETE CASCADE
);"#;

#[derive(Debug, Default)]
pub(crate) struct UploadBaseline {
    pub initialized: bool,
    pub event_hashes: BTreeMap<String, String>,
    pub rollup_hashes: BTreeMap<String, String>,
}

pub fn init_local_ledger(path: &Path) -> anyhow::Result<()> {
    let existed = path.exists();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let conn = Connection::open(path)?;
    conn.execute_batch(SCHEMA)?;
    let columns = conn
        .prepare("PRAGMA table_info(proxy_request_logs)")?
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    for (name, kind) in [
        ("service_tier", "TEXT"),
        ("service_tier_source", "TEXT"),
        ("reasoning_effort", "TEXT"),
        ("service_tier_pricing_version", "INTEGER"),
    ] {
        if !columns.iter().any(|c| c == name) {
            conn.execute_batch(&format!(
                "ALTER TABLE proxy_request_logs ADD COLUMN {name} {kind}"
            ))?;
        }
    }
    let expected = expected_revision();
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
            expected.as_str()
        }],
    )?;
    let cursor_columns = conn
        .prepare("PRAGMA table_info(session_log_sync)")?
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    if !cursor_columns.iter().any(|c| c == "last_file_size") {
        conn.execute_batch("ALTER TABLE session_log_sync ADD COLUMN last_file_size INTEGER")?;
    }
    // Only this known predecessor is safe for an additive upgrade. Keep existing
    // rollups, upload hashes, source binding and compaction barriers.
    let previous: String = conn.query_row(
        "SELECT value FROM ledger_meta WHERE key='importer_revision'",
        [],
        |r| r.get(0),
    )?;
    if previous == "cc-switch-3217f725:session-usage-v3-six-source:unified-app-key-v3" {
        let tx = conn.unchecked_transaction()?;
        tx.execute(
            "INSERT OR REPLACE INTO ledger_meta(key,value) VALUES ('codex_fast_reconcile','1')",
            [],
        )?;
        tx.execute(
            "DELETE FROM session_log_sync WHERE file_path GLOB '*/rollout-*.jsonl'",
            [],
        )?;
        tx.execute(
            "UPDATE ledger_meta SET value=?1 WHERE key='importer_revision'",
            [expected_revision()],
        )?;
        tx.commit()?;
    }
    Ok(())
}

fn verify_revision(conn: &Connection) -> anyhow::Result<()> {
    let revision: String = conn.query_row(
        "SELECT value FROM ledger_meta WHERE key = 'importer_revision'",
        [],
        |row| row.get(0),
    )?;
    let expected = expected_revision();
    if revision != expected {
        anyhow::bail!(
            "local usage ledger revision is {revision}, expected {expected}; run rebuild"
        );
    }
    Ok(())
}

pub(crate) fn load_upload_baseline(
    path: &Path,
    remote_key: &str,
) -> anyhow::Result<UploadBaseline> {
    let connection = Connection::open(path)?;
    connection.execute_batch("PRAGMA foreign_keys=ON;")?;
    let initialized = connection
        .query_row(
            "SELECT 1 FROM upload_remotes WHERE remote_key=?1",
            [remote_key],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    let load_kind = |kind: &str| -> anyhow::Result<BTreeMap<String, String>> {
        let mut statement = connection.prepare(
            "SELECT entity_key,content_hash FROM upload_hashes
             WHERE remote_key=?1 AND entity_kind=?2 ORDER BY entity_key",
        )?;
        let rows = statement
            .query_map(params![remote_key, kind], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        Ok(rows)
    };
    Ok(UploadBaseline {
        initialized: initialized.is_some(),
        event_hashes: load_kind("event")?,
        rollup_hashes: load_kind("rollup")?,
    })
}

pub(crate) fn save_upload_baseline(
    path: &Path,
    remote_key: &str,
    event_hashes: &BTreeMap<String, String>,
    rollup_hashes: &BTreeMap<String, String>,
) -> anyhow::Result<()> {
    let mut connection = Connection::open(path)?;
    connection.execute_batch("PRAGMA foreign_keys=ON;")?;
    let transaction = connection.transaction()?;
    transaction.execute(
        "INSERT INTO upload_remotes(remote_key,updated_at) VALUES (?1,?2)
         ON CONFLICT(remote_key) DO UPDATE SET
           updated_at=excluded.updated_at",
        params![remote_key, chrono::Utc::now().timestamp()],
    )?;
    transaction.execute(
        "DELETE FROM upload_hashes WHERE remote_key=?1",
        [remote_key],
    )?;
    for (kind, hashes) in [("event", event_hashes), ("rollup", rollup_hashes)] {
        for (entity_key, content_hash) in hashes {
            transaction.execute(
                "INSERT INTO upload_hashes(remote_key,entity_kind,entity_key,content_hash)
                 VALUES (?1,?2,?3,?4)",
                params![remote_key, kind, entity_key, content_hash],
            )?;
        }
    }
    transaction.commit()?;
    Ok(())
}

async fn rebuild_with_compaction(
    config: &LocalUsageConfig,
    compact: bool,
) -> anyhow::Result<RebuildSummary> {
    if let Some(parent) = config.database.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp = config.database.with_extension("rebuild.sqlite");
    if temp.exists() {
        fs::remove_file(&temp)?;
    }
    init_local_ledger(&temp)?;
    let summary = sync_into(&temp, config).await?;
    if compact {
        rollup_local(&temp, 30)?;
    }
    let integrity: String = Connection::open(&temp)?
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .context("validate rebuilt local usage ledger")?;
    if integrity != "ok" {
        anyhow::bail!("rebuilt local usage ledger failed integrity_check: {integrity}");
    }
    fs::rename(&temp, &config.database)?;
    Ok(summary)
}

pub async fn rebuild(config: &LocalUsageConfig) -> anyhow::Result<RebuildSummary> {
    rebuild_with_compaction(config, false).await
}

pub async fn rebuild_compact(config: &LocalUsageConfig) -> anyhow::Result<RebuildSummary> {
    rebuild_with_compaction(config, true).await
}

pub fn rebuild_cc_switch(
    source_config: &ClientConfig,
    ledger_path: &Path,
) -> anyhow::Result<RebuildSummary> {
    if let Some(parent) = ledger_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = ledger_path.with_extension("rebuild.sqlite");
    if temporary.exists() {
        fs::remove_file(&temporary)?;
    }
    let summary = sync_cc_switch(source_config, &temporary)?;
    let integrity: String = Connection::open(&temporary)?
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .context("validate rebuilt cc-switch mirror ledger")?;
    if integrity != "ok" {
        anyhow::bail!("rebuilt cc-switch mirror failed integrity_check: {integrity}");
    }
    fs::rename(temporary, ledger_path)?;
    Ok(summary)
}

pub async fn sync_local(config: &LocalUsageConfig) -> anyhow::Result<RebuildSummary> {
    init_local_ledger(&config.database)?;
    let connection = Connection::open(&config.database)?;
    verify_revision(&connection)?;
    drop(connection);
    sync_into(&config.database, config).await
}

pub async fn sync_local_compact(config: &LocalUsageConfig) -> anyhow::Result<RebuildSummary> {
    let summary = sync_local(config).await?;
    rollup_local(&config.database, 30)?;
    Ok(summary)
}

/// Mirrors the cc-switch detail table into the Client-owned ledger. Uploading
/// is always resumed from the ledger with its own independent cursor.
pub fn sync_cc_switch(
    source_config: &ClientConfig,
    ledger_path: &Path,
) -> anyhow::Result<RebuildSummary> {
    init_local_ledger(ledger_path)?;
    let conn = Connection::open(ledger_path)?;
    verify_revision(&conn)?;
    conn.execute_batch(
        "CREATE TEMP TABLE IF NOT EXISTS sync_seen_requests (
             app_type TEXT NOT NULL,
             request_id TEXT NOT NULL,
             PRIMARY KEY (app_type, request_id)
         );
         DELETE FROM sync_seen_requests;",
    )?;
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
            if insert_event(&transaction, event, "cc-switch")? {
                summary.imported += 1;
            } else {
                summary.skipped += 1;
            }
            transaction.execute(
                "INSERT OR IGNORE INTO sync_seen_requests(app_type,request_id) VALUES (?1,?2)",
                params![event.app_type, event.request_id],
            )?;
        }
        let last = events.last().expect("non-empty event batch");
        cursor = Cursor {
            created_at: last.created_at,
            app_type: last.app_type.clone(),
            request_id: last.request_id.clone(),
        };
        transaction.commit()?;
        if events.len() < source_config.batch_size {
            break;
        }
    }
    conn.execute(
        "DELETE FROM proxy_request_logs
         WHERE last_collector='cc-switch'
           AND NOT EXISTS (
             SELECT 1 FROM sync_seen_requests seen
             WHERE seen.app_type=proxy_request_logs.app_type
               AND seen.request_id=proxy_request_logs.request_id
           )",
        [],
    )?;
    mirror_rollups(source_config, &conn)?;
    Ok(summary)
}

async fn sync_into(path: &Path, config: &LocalUsageConfig) -> anyhow::Result<RebuildSummary> {
    let mut conn = Connection::open(path)?;
    verify_revision(&conn)?;
    let mut sync_paths = std::collections::HashMap::<String, (i64, Option<u64>)>::new();
    {
        let mut statement =
            conn.prepare("SELECT file_path, last_modified, last_file_size FROM session_log_sync")?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                (row.get::<_, i64>(1)?, row.get::<_, Option<u64>>(2)?),
            ))
        })?;
        for row in rows {
            let row = row?;
            sync_paths.insert(row.0, row.1);
        }
    }
    let pricing = load_pricing().await?;
    let reconcile_codex: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM ledger_meta WHERE key='codex_fast_reconcile')",
        [],
        |r| r.get(0),
    )?;
    let scanned_stamps = std::cell::RefCell::new(HashMap::new());
    let mut report = session_usage_core::import_all_filtered(&config.sources(), |source| {
        let stamp = file_stamp(source);
        scanned_stamps.borrow_mut().insert(source.to_owned(), stamp);
        let previous = sync_paths
            .get(&source.to_string_lossy().to_string())
            .copied();
        (reconcile_codex
            && source
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("rollout-")))
            || previous != Some((stamp.0, Some(stamp.1)))
    })?;
    for path in &report.scanned_paths {
        if scanned_stamps
            .borrow()
            .get(path)
            .is_some_and(|stamp| file_stamp(path) != *stamp)
            && !report.deferred_paths.contains(path)
        {
            report.deferred_paths.push(path.clone());
        }
    }
    let mut summary = RebuildSummary {
        imported: 0,
        skipped: report.skipped,
    };
    let transaction = conn.transaction()?;
    for record in &report.records {
        if insert_record(&transaction, record, &pricing, "local")? {
            summary.imported += 1;
        } else {
            summary.skipped += 1;
        }
    }
    if reconcile_codex {
        reconcile_codex_upgrade(&transaction, &report)?;
    }
    record_scan_metadata(&transaction, report, &scanned_stamps.into_inner())?;
    transaction.commit()?;
    Ok(summary)
}

// A parser upgrade can remove formerly counted replay events. Reconcile only
// successfully resolved Codex files, never another collector or compacted days.
fn reconcile_codex_upgrade(conn: &Connection, report: &ImportReport) -> anyhow::Result<()> {
    conn.execute_batch("CREATE TEMP TABLE IF NOT EXISTS codex_seen(request_id TEXT PRIMARY KEY); DELETE FROM codex_seen;")?;
    for record in &report.records {
        if record.app_type == "codex" {
            conn.execute(
                "INSERT OR IGNORE INTO codex_seen VALUES (?1)",
                [&record.request_id],
            )?;
        }
    }
    for path in &report.scanned_paths {
        if report.deferred_paths.contains(path) {
            continue;
        }
        let Some(stem) = path
            .file_stem()
            .and_then(|s| s.to_str())
            .filter(|s| s.starts_with("rollout-"))
        else {
            continue;
        };
        let Some(id) = stem
            .get(stem.len().saturating_sub(36)..)
            .and_then(|s| uuid::Uuid::parse_str(s).ok())
        else {
            continue;
        };
        let prefix = format!("codex_session:thread-v1:{}:", id.hyphenated());
        conn.execute("DELETE FROM proxy_request_logs WHERE last_collector='local' AND app_type='codex' AND data_source='codex_session' AND substr(request_id,1,length(?1))=?1 AND NOT EXISTS(SELECT 1 FROM codex_seen s WHERE s.request_id=proxy_request_logs.request_id)", [prefix])?;
    }
    if report.deferred_paths.is_empty() {
        conn.execute(
            "DELETE FROM ledger_meta WHERE key='codex_fast_reconcile'",
            [],
        )?;
    }
    Ok(())
}

fn insert_record(
    conn: &Connection,
    record: &UsageRecord,
    pricing: &HashMap<String, Pricing>,
    collector: &str,
) -> anyhow::Result<bool> {
    if let Some(identity) = &record.identity {
        let request_seen: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM session_usage_dedup WHERE data_source=?1 AND request_id=?2)",
            params![record.data_source, record.request_id],
            |row| row.get(0),
        )?;
        let semantic_seen: bool = conn.query_row(
            if identity.has_entry_id {
                "SELECT EXISTS(SELECT 1 FROM session_usage_dedup WHERE data_source=?1 AND semantic_id=?2 AND has_entry_id=0)"
            } else {
                "SELECT EXISTS(SELECT 1 FROM session_usage_dedup WHERE data_source=?1 AND semantic_id=?2)"
            },
            params![record.data_source, identity.semantic_id],
            |row| row.get(0),
        )?;
        if request_seen || semantic_seen {
            return Ok(false);
        }
        conn.execute(
            "INSERT OR IGNORE INTO session_usage_dedup(data_source,request_id,semantic_id,has_entry_id)
             VALUES (?1,?2,?3,?4)",
            params![
                record.data_source,
                record.request_id,
                identity.semantic_id,
                i64::from(identity.has_entry_id)
            ],
        )?;
    }
    let model_for_pricing = record
        .pricing_model
        .as_deref()
        .filter(|m| !is_placeholder(m))
        .or_else(|| (!is_placeholder(&record.model)).then_some(record.model.as_str()))
        .or(Some(record.request_model.as_str()));
    let mut cost = calculate_cost(
        record,
        model_for_pricing.and_then(|m| find_pricing(pricing, m)),
    );
    // Metadata backfill must not reprice retained requests at today's rates.
    let prior = conn.query_row("SELECT input_cost_usd,output_cost_usd,cache_read_cost_usd,cache_creation_cost_usd,total_cost_usd,service_tier,service_tier_pricing_version FROM proxy_request_logs WHERE app_type=?1 AND request_id=?2 AND last_collector=?3 AND model=?4 AND COALESCE(pricing_model,'')=?5 AND input_tokens=?6 AND output_tokens=?7 AND cache_read_tokens=?8 AND cache_creation_tokens=?9 AND input_token_semantics=?10",
        params![record.app_type,record.request_id,collector,record.model,record.pricing_model.as_deref().unwrap_or(""),record.input_tokens,record.output_tokens,record.cache_read_tokens,record.cache_creation_tokens,record.input_token_semantics],
        |r| Ok(([r.get::<_,String>(0)?,r.get(1)?,r.get(2)?,r.get(3)?],r.get::<_,String>(4)?,r.get::<_,Option<String>>(5)?,r.get::<_,Option<i64>>(6)?))).optional()?;
    if let Some((components, total, tier, version)) = prior {
        let parsed = components
            .iter()
            .map(|v| v.parse::<Decimal>())
            .collect::<Result<Vec<_>, _>>()?;
        let original = total.parse::<Decimal>()?;
        let sum: Decimal = parsed.iter().sum();
        cost = (components, total);
        if (sum - original).abs() <= Decimal::new(1, 10) {
            let model = record.pricing_model.as_deref().unwrap_or(&record.model);
            let old = if version == Some(2) {
                cc_switch_usage_core::fast_pricing::factors(model, tier.as_deref(), 0)
                    .map(|f| f.0)
                    .unwrap_or(Decimal::ONE)
            } else {
                Decimal::ONE
            };
            let new = cc_switch_usage_core::fast_pricing::factors(
                model,
                record.service_tier.as_deref(),
                0,
            )
            .map(|f| f.0)
            .unwrap_or(Decimal::ONE);
            if old != new {
                let adjusted = parsed
                    .into_iter()
                    .map(|v| v * new / old)
                    .collect::<Vec<_>>();
                cost = (
                    std::array::from_fn(|i| adjusted[i].to_string()),
                    adjusted.iter().sum::<Decimal>().to_string(),
                );
            }
        }
    }
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
        record.service_tier.as_deref(),
        record.service_tier_source.as_deref(),
        record.reasoning_effort.as_deref(),
        record.service_tier_pricing_version,
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
        collector,
    )
}

fn insert_event(conn: &Connection, event: &UsageEvent, collector: &str) -> anyhow::Result<bool> {
    insert_values(
        conn,
        &event.request_id,
        &event.provider_id,
        &event.app_type,
        &event.model,
        event.request_model.as_deref(),
        event.pricing_model.as_deref(),
        event.service_tier.as_deref(),
        event.service_tier_source.as_deref(),
        event.reasoning_effort.as_deref(),
        event.service_tier_pricing_version,
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
        collector,
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
    service_tier: Option<&str>,
    service_tier_source: Option<&str>,
    reasoning_effort: Option<&str>,
    service_tier_pricing_version: Option<i64>,
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
    collector: &str,
) -> anyhow::Result<bool> {
    let compacted: bool = conn.query_row(
        "SELECT EXISTS(
           SELECT 1 FROM compaction_barriers
           WHERE ?1>=day_start_utc AND ?1<day_end_utc
         )",
        [created_at],
        |row| row.get(0),
    )?;
    if compacted {
        return Ok(false);
    }
    let changed = conn.execute(
        "INSERT INTO proxy_request_logs (
            request_id, provider_id, app_type, model, request_model, pricing_model,
            input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens,
            input_token_semantics, input_cost_usd, output_cost_usd, cache_read_cost_usd,
            cache_creation_cost_usd, total_cost_usd, latency_ms, first_token_ms, duration_ms,
            status_code, error_message, session_id, provider_type, is_streaming,
            cost_multiplier, created_at, data_source, last_collector, service_tier,service_tier_source,reasoning_effort,service_tier_pricing_version
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                   ?17, NULL, NULL, ?18, NULL, ?19, ?20, ?21, '1.0', ?22, ?23, ?24, ?25, ?26, ?27, ?28)
        ON CONFLICT(app_type,request_id) DO UPDATE SET
            service_tier = excluded.service_tier,
            service_tier_source = excluded.service_tier_source,
            reasoning_effort = excluded.reasoning_effort,
            service_tier_pricing_version = excluded.service_tier_pricing_version,
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
            created_at = excluded.created_at,
            data_source = excluded.data_source,
            last_collector = excluded.last_collector
        WHERE proxy_request_logs.service_tier IS NOT excluded.service_tier OR proxy_request_logs.service_tier_source IS NOT excluded.service_tier_source OR proxy_request_logs.reasoning_effort IS NOT excluded.reasoning_effort OR proxy_request_logs.service_tier_pricing_version IS NOT excluded.service_tier_pricing_version
               OR proxy_request_logs.provider_id IS NOT excluded.provider_id
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
               OR proxy_request_logs.is_streaming IS NOT excluded.is_streaming
               OR proxy_request_logs.data_source IS NOT excluded.data_source
               OR proxy_request_logs.last_collector IS NOT excluded.last_collector",
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
            collector,
            service_tier,
            service_tier_source,
            reasoning_effort,
            service_tier_pricing_version,
        ],
    )?;
    Ok(changed > 0)
}

fn mirror_rollups(source_config: &ClientConfig, ledger: &Connection) -> anyhow::Result<()> {
    let source = Connection::open_with_flags(
        &source_config.cc_switch_db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )?;
    let exists: bool = source.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='usage_daily_rollups')",
        [],
        |row| row.get(0),
    )?;
    let transaction = ledger.unchecked_transaction()?;
    transaction.execute_batch(
        "CREATE TEMP TABLE IF NOT EXISTS sync_seen_rollups (
           date TEXT NOT NULL, app_type TEXT NOT NULL, provider_id TEXT NOT NULL,
           model TEXT NOT NULL, request_model TEXT NOT NULL, pricing_model TEXT NOT NULL,
           PRIMARY KEY (date,app_type,provider_id,model,request_model,pricing_model)
         );
         DELETE FROM sync_seen_rollups;",
    )?;
    if exists {
        let has_semantics = source
            .prepare("PRAGMA table_info(usage_daily_rollups)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<Vec<_>, _>>()?
            .iter()
            .any(|column| column == "input_token_semantics");
        let semantics = if has_semantics {
            "input_token_semantics"
        } else {
            "2 AS input_token_semantics"
        };
        let sql = format!(
            "SELECT date,app_type,provider_id,model,request_model,pricing_model,
                    request_count,success_count,input_tokens,output_tokens,
                    cache_read_tokens,cache_creation_tokens,{semantics},total_cost_usd,
                    CAST(avg_latency_ms AS REAL) AS avg_latency_ms
             FROM usage_daily_rollups"
        );
        let mut statement = source.prepare(&sql)?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, i64>(8)?,
                row.get::<_, i64>(9)?,
                row.get::<_, i64>(10)?,
                row.get::<_, i64>(11)?,
                row.get::<_, i64>(12)?,
                row.get::<_, String>(13)?,
                row.get::<_, f64>(14)?,
            ))
        })?;
        for row in rows {
            let row = row?;
            transaction.execute(
                "INSERT INTO usage_daily_rollups
                 (date,app_type,provider_id,model,request_model,pricing_model,
                  request_count,success_count,input_tokens,output_tokens,cache_read_tokens,
                  cache_creation_tokens,input_token_semantics,total_cost_usd,avg_latency_ms,
                  last_collector)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,'cc-switch')
                 ON CONFLICT(date,app_type,provider_id,model,request_model,pricing_model)
                 DO UPDATE SET
                   request_count=excluded.request_count,
                   success_count=excluded.success_count,
                   input_tokens=excluded.input_tokens,
                   output_tokens=excluded.output_tokens,
                   cache_read_tokens=excluded.cache_read_tokens,
                   cache_creation_tokens=excluded.cache_creation_tokens,
                   input_token_semantics=excluded.input_token_semantics,
                   total_cost_usd=excluded.total_cost_usd,
                   avg_latency_ms=excluded.avg_latency_ms,
                   last_collector=excluded.last_collector",
                params![
                    row.0, row.1, row.2, row.3, row.4, row.5, row.6, row.7, row.8, row.9, row.10,
                    row.11, row.12, row.13, row.14
                ],
            )?;
            transaction.execute(
                "INSERT OR IGNORE INTO sync_seen_rollups
                 (date,app_type,provider_id,model,request_model,pricing_model)
                 VALUES (?1,?2,?3,?4,?5,?6)",
                params![row.0, row.1, row.2, row.3, row.4, row.5],
            )?;
        }
    }
    transaction.execute(
        "DELETE FROM usage_daily_rollups
         WHERE last_collector='cc-switch'
           AND NOT EXISTS (
             SELECT 1 FROM sync_seen_rollups seen
             WHERE seen.date=usage_daily_rollups.date
               AND seen.app_type=usage_daily_rollups.app_type
               AND seen.provider_id=usage_daily_rollups.provider_id
               AND seen.model=usage_daily_rollups.model
               AND seen.request_model=usage_daily_rollups.request_model
               AND seen.pricing_model=usage_daily_rollups.pricing_model
           )",
        [],
    )?;
    transaction.commit()?;
    rebuild_compaction_barriers(ledger)?;
    Ok(())
}

fn rebuild_compaction_barriers(connection: &Connection) -> anyhow::Result<()> {
    let dates = {
        let mut statement =
            connection.prepare("SELECT DISTINCT date FROM usage_daily_rollups ORDER BY date")?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        rows
    };
    let transaction = connection.unchecked_transaction()?;
    transaction.execute("DELETE FROM compaction_barriers", [])?;
    for date in dates {
        let day = NaiveDate::parse_from_str(&date, "%Y-%m-%d")?;
        let (day_start_utc, day_end_utc) = cc_switch_usage_core::local_day_utc_bounds(&Local, day)
            .context("resolve compaction barrier")?;
        transaction.execute(
            "INSERT OR IGNORE INTO compaction_barriers(day_start_utc,day_end_utc)
             VALUES (?1,?2)",
            params![day_start_utc, day_end_utc],
        )?;
    }
    transaction.commit()?;
    Ok(())
}

fn rollup_local(path: &Path, retain_days: i64) -> anyhow::Result<()> {
    let connection = Connection::open(path)?;
    let target = Local::now()
        .checked_sub_signed(chrono::Duration::days(retain_days))
        .context("local rollup cutoff overflow")?
        .date_naive();
    let cutoff_day = target.succ_opt().context("local rollup day overflow")?;
    let (cutoff, _) = cc_switch_usage_core::local_day_utc_bounds(&Local, cutoff_day)
        .context("resolve local rollup cutoff")?;
    let effective = cc_switch_usage_core::sql::effective_usage_log_filter("l", None);
    let fresh_detail = cc_switch_usage_core::sql::fresh_input("l");
    let fresh_old = cc_switch_usage_core::sql::fresh_input("old");
    let sql = format!(
        "INSERT OR REPLACE INTO usage_daily_rollups
         (date,app_type,provider_id,model,request_model,pricing_model,request_count,
          success_count,input_tokens,output_tokens,cache_read_tokens,cache_creation_tokens,
          input_token_semantics,total_cost_usd,avg_latency_ms,last_collector)
         SELECT d,a,p,m,rm,pm,
                COALESCE(old.request_count,0)+new_req,
                COALESCE(old.success_count,0)+new_succ,
                COALESCE({fresh_old},0)+new_in,
                COALESCE(old.output_tokens,0)+new_out,
                COALESCE(old.cache_read_tokens,0)+new_cr,
                COALESCE(old.cache_creation_tokens,0)+new_cc,
                2,
                CAST(COALESCE(CAST(old.total_cost_usd AS REAL),0)+new_cost AS TEXT),
                CASE WHEN COALESCE(old.request_count,0)+new_req>0
                     THEN (COALESCE(old.avg_latency_ms,0)*COALESCE(old.request_count,0)
                           +new_lat*new_req)/(COALESCE(old.request_count,0)+new_req)
                     ELSE 0.0 END,
                'local'
         FROM (
           SELECT date(l.created_at,'unixepoch','localtime') AS d,
                  l.app_type AS a,l.provider_id AS p,l.model AS m,
                  COALESCE(l.request_model,'') AS rm,COALESCE(l.pricing_model,'') AS pm,
                  COUNT(*) AS new_req,
                  SUM(CASE WHEN l.status_code>=200 AND l.status_code<300 THEN 1 ELSE 0 END) AS new_succ,
                  COALESCE(SUM({fresh_detail}),0) AS new_in,
                  COALESCE(SUM(l.output_tokens),0) AS new_out,
                  COALESCE(SUM(l.cache_read_tokens),0) AS new_cr,
                  COALESCE(SUM(l.cache_creation_tokens),0) AS new_cc,
                  COALESCE(SUM(CAST(l.total_cost_usd AS REAL)),0) AS new_cost,
                  COALESCE(AVG(l.latency_ms),0) AS new_lat
           FROM proxy_request_logs l WHERE l.created_at<?1 AND {effective}
           GROUP BY d,a,p,m,rm,pm
         ) agg
         LEFT JOIN usage_daily_rollups old
           ON old.date=agg.d AND old.app_type=agg.a AND old.provider_id=agg.p
          AND old.model=agg.m AND old.request_model=agg.rm AND old.pricing_model=agg.pm"
    );
    let transaction = connection.unchecked_transaction()?;
    transaction.execute(&sql, [cutoff])?;
    transaction.execute(
        "DELETE FROM proxy_request_logs WHERE created_at<?1",
        [cutoff],
    )?;
    transaction.commit()?;
    rebuild_compaction_barriers(&connection)?;
    Ok(())
}

fn collector_owner(collector: &str) -> &str {
    if collector == "local-compact" {
        "local"
    } else {
        collector
    }
}

pub fn merge_provider_snapshot(
    path: &Path,
    collector: &str,
    providers: Vec<ProviderEntry>,
) -> anyhow::Result<()> {
    let mut connection = Connection::open(path)?;
    verify_revision(&connection)?;
    let collector = collector_owner(collector);
    let transaction = connection.transaction()?;
    transaction.execute_batch(
        "CREATE TEMP TABLE IF NOT EXISTS sync_seen_providers (
           app_type TEXT NOT NULL, provider_id TEXT NOT NULL,
           PRIMARY KEY (app_type,provider_id)
         );
         DELETE FROM sync_seen_providers;",
    )?;
    for provider in providers {
        transaction.execute(
            "INSERT INTO client_provider_catalog(app_type,provider_id,name,last_collector)
             VALUES (?1,?2,?3,?4)
             ON CONFLICT(app_type,provider_id) DO UPDATE SET
               name=excluded.name,last_collector=excluded.last_collector",
            params![
                provider.app_type,
                provider.provider_id,
                provider.name,
                collector
            ],
        )?;
        transaction.execute(
            "INSERT OR IGNORE INTO sync_seen_providers(app_type,provider_id) VALUES (?1,?2)",
            params![provider.app_type, provider.provider_id],
        )?;
    }
    transaction.execute(
        "DELETE FROM client_provider_catalog
         WHERE last_collector=?1
           AND NOT EXISTS (
             SELECT 1 FROM sync_seen_providers seen
             WHERE seen.app_type=client_provider_catalog.app_type
               AND seen.provider_id=client_provider_catalog.provider_id
           )",
        [collector],
    )?;
    transaction.commit()?;
    Ok(())
}

pub fn read_provider_catalog(path: &Path) -> anyhow::Result<Vec<ProviderEntry>> {
    let connection = Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )?;
    let mut statement = connection.prepare(
        "SELECT app_type,provider_id,name FROM client_provider_catalog
         ORDER BY app_type,provider_id",
    )?;
    let providers = statement
        .query_map([], |row| {
            Ok(ProviderEntry {
                app_type: row.get(0)?,
                provider_id: row.get(1)?,
                name: row.get(2)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(providers)
}

pub fn read_rollups(path: &Path) -> anyhow::Result<Vec<RollupSnapshot>> {
    let connection = Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )?;
    let has_rollups: bool = connection.query_row(
        "SELECT EXISTS(
           SELECT 1 FROM sqlite_master WHERE type='table' AND name='usage_daily_rollups'
         )",
        [],
        |row| row.get(0),
    )?;
    if !has_rollups {
        return Ok(Vec::new());
    }
    let mut statement = connection.prepare(
        "SELECT date,app_type,provider_id,model,request_model,pricing_model,
                request_count,success_count,input_tokens,output_tokens,cache_read_tokens,
                cache_creation_tokens,input_token_semantics,total_cost_usd,
                CAST(avg_latency_ms AS REAL) AS avg_latency_ms
         FROM usage_daily_rollups ORDER BY date,app_type,provider_id,model,request_model,pricing_model",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, i64>(6)?,
            row.get::<_, i64>(7)?,
            row.get::<_, i64>(8)?,
            row.get::<_, i64>(9)?,
            row.get::<_, i64>(10)?,
            row.get::<_, i64>(11)?,
            row.get::<_, i64>(12)?,
            row.get::<_, String>(13)?,
            row.get::<_, f64>(14)?,
        ))
    })?;
    let mut snapshots = Vec::new();
    for row in rows {
        let row = row?;
        let day = NaiveDate::parse_from_str(&row.0, "%Y-%m-%d")?;
        let (day_start_utc, day_end_utc) = cc_switch_usage_core::local_day_utc_bounds(&Local, day)
            .context("resolve source-local rollup day")?;
        snapshots.push(RollupSnapshot {
            schema_version: SCHEMA_VERSION,
            date: row.0,
            app_type: row.1,
            provider_id: row.2,
            model: row.3,
            request_model: row.4,
            pricing_model: row.5,
            request_count: row.6,
            success_count: row.7,
            input_tokens: row.8,
            output_tokens: row.9,
            cache_read_tokens: row.10,
            cache_creation_tokens: row.11,
            input_token_semantics: row.12,
            total_cost_usd: row.13,
            avg_latency_ms: row.14,
            day_start_utc,
            day_end_utc,
        });
    }
    Ok(snapshots)
}

fn file_stamp(path: &Path) -> (i64, u64) {
    fs::metadata(path)
        .ok()
        .map(|m| {
            (
                m.modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_nanos().min(i64::MAX as u128) as i64)
                    .unwrap_or(0),
                m.len(),
            )
        })
        .unwrap_or_default()
}

fn record_scan_metadata(
    conn: &Connection,
    report: ImportReport,
    stamps: &HashMap<PathBuf, (i64, u64)>,
) -> anyhow::Result<()> {
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
        let Some(&(modified, size)) = stamps.get(&path) else {
            continue;
        };
        if file_stamp(&path) != (modified, size) {
            continue;
        }
        conn.execute(
            "INSERT INTO session_log_sync(file_path,last_modified,last_line_offset,last_synced_at,last_file_size)
             VALUES (?1,?2,0,?3,?4) ON CONFLICT(file_path) DO UPDATE SET last_modified=excluded.last_modified,last_synced_at=excluded.last_synced_at,last_file_size=excluded.last_file_size",
            params![path.to_string_lossy(), modified, now, size],
        )?;
    }
    Ok(())
}

pub fn local_provider_snapshot() -> telemetry_core::ProviderSnapshot {
    let providers = [
        ("claude", "_session", "Claude session"),
        ("codex", "_codex_session", "Codex session"),
        ("gemini", "_gemini_session", "Gemini session"),
        ("opencode", "_opencode_session", "OpenCode session"),
        ("grokbuild", "_grokbuild_session", "Grok Build session"),
        ("pi", "_pi_session", "Pi session"),
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
            pi_dir: dir.join("pi"),
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
        assert_eq!(
            prices.get("gpt-5-2025").unwrap().output_cost_per_million,
            Decimal::from_str("15.0").unwrap()
        );
        assert_eq!(
            prices
                .get("gpt-5-2025")
                .unwrap()
                .cache_creation_cost_per_million,
            Decimal::from_str("3.125").unwrap()
        );
        assert!(!prices.contains_key("free"));
    }

    #[test]
    fn local_schema_has_detail_and_rollup() {
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
        let upload_hashes: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='upload_hashes')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(detail);
        assert!(rollup);
        assert!(upload_hashes);
    }

    #[tokio::test]
    async fn rebuild_modes_keep_or_compact_old_codex_usage() {
        let dir = tempfile::tempdir().unwrap();
        let sessions = dir.path().join("codex/sessions/2026/07/30");
        fs::create_dir_all(&sessions).unwrap();
        let timestamp = (chrono::Utc::now() - chrono::Duration::days(40)).to_rfc3339();
        let fixture = r#"{"type":"session_meta","timestamp":"TIMESTAMP","payload":{"id":"019c6e27-e55b-73d1-87d8-4e01f1f75043"}}
{"type":"turn_context","payload":{"model":"gpt-5"}}
{"type":"event_msg","timestamp":"TIMESTAMP","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":12,"output_tokens":3,"cached_input_tokens":2}}}}"#
            .replace("TIMESTAMP", &timestamp);
        fs::write(
            sessions.join("rollout-019c6e27-e55b-73d1-87d8-4e01f1f75043.jsonl"),
            fixture,
        )
        .unwrap();
        let local = config(dir.path());
        let summary = rebuild(&local).await.unwrap();
        assert_eq!(summary.imported, 1);
        let conn = Connection::open(&local.database).unwrap();
        let row: (String, i64, i64, i64) = conn.query_row("SELECT app_type,input_tokens,output_tokens,cache_read_tokens FROM proxy_request_logs", [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))).unwrap();
        assert_eq!(row, ("codex".into(), 12, 3, 2));
        let rollups: i64 = conn
            .query_row("SELECT COUNT(*) FROM usage_daily_rollups", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(rollups, 0, "local rebuild must retain old detail");
        drop(conn);

        let compact = rebuild_compact(&local).await.unwrap();
        assert_eq!(compact.imported, 1);
        let conn = Connection::open(&local.database).unwrap();
        let counts: (i64, i64) = conn
            .query_row(
                "SELECT (SELECT COUNT(*) FROM proxy_request_logs),
                        (SELECT COUNT(*) FROM usage_daily_rollups)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(counts, (0, 1), "local-compact rebuild must roll old detail");
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

    #[tokio::test]
    async fn local_compaction_requires_the_explicit_compact_entrypoint() {
        let dir = tempfile::tempdir().unwrap();
        let local = config(dir.path());
        init_local_ledger(&local.database).unwrap();
        let old_timestamp = chrono::Utc::now().timestamp() - 40 * 86_400;
        Connection::open(&local.database)
            .unwrap()
            .execute(
                "INSERT INTO proxy_request_logs (
                   request_id,provider_id,app_type,model,input_tokens,output_tokens,
                   cache_read_tokens,cache_creation_tokens,input_token_semantics,
                   total_cost_usd,latency_ms,status_code,is_streaming,created_at,data_source
                 ) VALUES ('old-detail','provider','codex','gpt-5',100,50,10,2,1,
                           '0.25',200,200,1,?1,'session_log')",
                [old_timestamp],
            )
            .unwrap();

        sync_local(&local).await.unwrap();
        let after_local: (i64, i64) = Connection::open(&local.database)
            .unwrap()
            .query_row(
                "SELECT (SELECT COUNT(*) FROM proxy_request_logs),
                        (SELECT COUNT(*) FROM usage_daily_rollups)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(after_local, (1, 0));

        sync_local_compact(&local).await.unwrap();
        let after_compact: (i64, i64) = Connection::open(&local.database)
            .unwrap()
            .query_row(
                "SELECT (SELECT COUNT(*) FROM proxy_request_logs),
                        (SELECT COUNT(*) FROM usage_daily_rollups)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(after_compact, (0, 1));
    }

    #[test]
    fn local_rollup_merges_late_detail_with_existing_history() {
        use chrono::TimeZone;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.db");
        init_local_ledger(&path).unwrap();
        let old_timestamp = chrono::Utc::now().timestamp() - 40 * 86_400;
        let date = Local
            .timestamp_opt(old_timestamp, 0)
            .single()
            .unwrap()
            .format("%Y-%m-%d")
            .to_string();
        let connection = Connection::open(&path).unwrap();
        connection
            .execute(
                "INSERT INTO usage_daily_rollups (
                   date,app_type,provider_id,model,request_model,pricing_model,
                   request_count,success_count,input_tokens,output_tokens,cache_read_tokens,
                   cache_creation_tokens,input_token_semantics,total_cost_usd,avg_latency_ms
                 ) VALUES (?1,'codex','provider','gpt-5','','',10,9,1000,500,100,20,2,'1.0',100)",
                [&date],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO proxy_request_logs (
                   request_id,provider_id,app_type,model,input_tokens,output_tokens,
                   cache_read_tokens,cache_creation_tokens,input_token_semantics,
                   total_cost_usd,latency_ms,status_code,is_streaming,created_at,data_source
                 ) VALUES ('late','provider','codex','gpt-5',100,50,10,2,1,
                           '0.25',200,200,1,?1,'session_log')",
                [old_timestamp],
            )
            .unwrap();
        drop(connection);

        rollup_local(&path, 30).unwrap();
        let merged: (i64, i64, i64, i64, i64, f64, f64) = Connection::open(&path)
            .unwrap()
            .query_row(
                "SELECT request_count,success_count,input_tokens,output_tokens,
                        cache_read_tokens,CAST(total_cost_usd AS REAL),avg_latency_ms
                 FROM usage_daily_rollups",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(merged.0, 11);
        assert_eq!(merged.1, 10);
        assert_eq!(
            merged.2, 1088,
            "semantics=1 detail subtracts cache read and creation"
        );
        assert_eq!(merged.3, 550);
        assert_eq!(merged.4, 110);
        assert!((merged.5 - 1.25).abs() < 1e-9);
        assert!((merged.6 - (1200.0 / 11.0)).abs() < 1e-9);
    }

    #[test]
    fn local_provider_snapshot_is_available_without_cc_switch_tables() {
        let snapshot = local_provider_snapshot();
        assert_eq!(snapshot.providers.len(), 6);
        assert!(snapshot
            .providers
            .iter()
            .any(|entry| entry.provider_id == "_codex_session"));
        assert!(snapshot
            .providers
            .iter()
            .any(|entry| entry.provider_id == "_pi_session"));
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
                    ('request-b', 11, 'codex', 'provider', 'model', '', '', 4, 5, 0, 0, 1, '0', 6, 200, 1, 'proxy');
                CREATE TABLE usage_daily_rollups (
                    date TEXT, app_type TEXT, provider_id TEXT, model TEXT,
                    request_model TEXT, pricing_model TEXT, request_count INTEGER,
                    success_count INTEGER, input_tokens INTEGER, output_tokens INTEGER,
                    cache_read_tokens INTEGER, cache_creation_tokens INTEGER,
                    input_token_semantics INTEGER, total_cost_usd TEXT,
                    avg_latency_ms REAL
                );
                INSERT INTO usage_daily_rollups VALUES
                    ('2026-07-21', 'codex', 'provider', 'model', '', '',
                     3, 3, 30, 6, 0, 0, 2, '0.3', 4183.38333333333),
                    ('2026-07-22', 'codex', 'provider', 'model', '', '',
                     2, 2, 20, 4, 0, 0, 2, '0.2', 100);",
            )
            .unwrap();
        let source_latency_type: String = source_conn
            .query_row(
                "SELECT typeof(avg_latency_ms) FROM usage_daily_rollups
                 WHERE date='2026-07-21'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(source_latency_type, "real");
        let local = config(dir.path());
        let source_config = ClientConfig {
            cc_switch_db: source,
            server_url: "http://localhost".into(),
            auth_token: "test-token".into(),
            batch_size: 1,
        };

        let first = sync_cc_switch(&source_config, &local.database).unwrap();
        assert_eq!(first.imported, 2);
        assert_eq!(first.skipped, 0);
        let second = sync_cc_switch(&source_config, &local.database).unwrap();
        assert_eq!(second.imported, 0);
        let mut mirror_config = source_config.clone();
        mirror_config.cc_switch_db = local.database.clone();
        assert_eq!(
            crate::read_events(&source_config, &Cursor::default()).unwrap(),
            crate::read_events(&mirror_config, &Cursor::default()).unwrap(),
            "selected cc-switch detail fields must be copied without repricing or normalization"
        );
        assert_eq!(
            read_rollups(&source_config.cc_switch_db).unwrap(),
            read_rollups(&local.database).unwrap(),
            "cc-switch rollup accounting and fractional latency must round-trip exactly"
        );
        let count: i64 = Connection::open(&local.database)
            .unwrap()
            .query_row("SELECT COUNT(*) FROM proxy_request_logs", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 2);
        let mirrored_latency: (f64, String) = Connection::open(&local.database)
            .unwrap()
            .query_row(
                "SELECT avg_latency_ms,typeof(avg_latency_ms)
                 FROM usage_daily_rollups WHERE date='2026-07-21'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert!((mirrored_latency.0 - 4183.38333333333).abs() < f64::EPSILON);
        assert_eq!(mirrored_latency.1, "real");
        let verified = crate::verify_cc_switch_mirror(&source_config, &local.database).unwrap();
        assert_eq!(verified.detail_rows, 2);
        assert_eq!(verified.rollup_rows, 2);
        source_conn.execute_batch("ALTER TABLE proxy_request_logs ADD COLUMN service_tier TEXT; ALTER TABLE proxy_request_logs ADD COLUMN service_tier_source TEXT; ALTER TABLE proxy_request_logs ADD COLUMN reasoning_effort TEXT; ALTER TABLE proxy_request_logs ADD COLUMN service_tier_pricing_version INTEGER; PRAGMA user_version=21; UPDATE proxy_request_logs SET service_tier='fast',service_tier_source='response',reasoning_effort='high',service_tier_pricing_version=2,total_cost_usd='2.5' WHERE request_id='request-a';").unwrap();
        assert_eq!(
            sync_cc_switch(&source_config, &local.database)
                .unwrap()
                .imported,
            1
        );
        let enriched = crate::read_events(&mirror_config, &Cursor::default()).unwrap();
        assert_eq!(enriched[0].service_tier.as_deref(), Some("fast"));
        assert_eq!(enriched[0].total_cost_usd, "2.5");
        assert_eq!(enriched[0].created_at, 10);
        assert_eq!(
            sync_cc_switch(&source_config, &local.database)
                .unwrap()
                .imported,
            0
        );
        crate::verify_cc_switch_mirror(&source_config, &local.database).unwrap();
        source_conn
            .execute(
                "UPDATE proxy_request_logs SET input_tokens=99 WHERE request_id='request-a'",
                [],
            )
            .unwrap();
        assert!(crate::verify_cc_switch_mirror(&source_config, &local.database).is_err());
    }
    #[test]
    fn fast_backfill_preserves_historical_prices_reported_totals_and_compaction() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.db");
        init_local_ledger(&path).unwrap();
        let db = Connection::open(&path).unwrap();
        let mut record = UsageRecord {
            request_id: "fast-row".into(),
            app_type: "codex".into(),
            provider_id: "p".into(),
            provider_type: "codex_session".into(),
            data_source: "codex_session".into(),
            model: "gpt-5.5".into(),
            request_model: "gpt-5.5".into(),
            pricing_model: None,
            service_tier: None,
            service_tier_source: None,
            reasoning_effort: None,
            service_tier_pricing_version: None,
            input_tokens: 1_000_000,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            input_token_semantics: 1,
            created_at: 100,
            session_id: None,
            source_path: PathBuf::from("rollout.jsonl"),
            is_streaming: true,
            status_code: 200,
            latency_ms: 0,
            reported_total_cost_usd: None,
            identity: None,
        };
        let mut pricing = HashMap::from([(
            "gpt-5.5".into(),
            Pricing {
                input_cost_per_million: Decimal::ONE,
                output_cost_per_million: Decimal::ONE,
                cache_read_cost_per_million: Decimal::ONE,
                cache_creation_cost_per_million: Decimal::ONE,
            },
        )]);
        assert!(insert_record(&db, &record, &pricing, "local").unwrap());
        record.service_tier = Some("priority".into());
        record.service_tier_source = Some("request".into());
        record.reasoning_effort = Some("high".into());
        record.service_tier_pricing_version = Some(2);
        pricing.get_mut("gpt-5.5").unwrap().input_cost_per_million = Decimal::from(100);
        assert!(insert_record(&db, &record, &pricing, "local").unwrap());
        let total = || {
            db.query_row("SELECT CAST(total_cost_usd AS REAL) FROM proxy_request_logs WHERE request_id='fast-row'",[],|r| r.get::<_,f64>(0)).unwrap()
        };
        assert_eq!(total(), 2.5);
        assert!(!insert_record(&db, &record, &pricing, "local").unwrap());
        assert_eq!(total(), 2.5);
        db.execute("UPDATE proxy_request_logs SET total_cost_usd='7.0'", [])
            .unwrap();
        record.service_tier = Some("default".into());
        insert_record(&db, &record, &pricing, "local").unwrap();
        assert_eq!(total(), 7.0);
        db.execute("INSERT INTO compaction_barriers VALUES (200,300)", [])
            .unwrap();
        record.request_id = "pruned".into();
        record.created_at = 250;
        assert!(!insert_record(&db, &record, &pricing, "local").unwrap());
    }

    #[test]
    fn known_importer_upgrade_preserves_barriers_and_non_codex_cursors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.db");
        init_local_ledger(&path).unwrap();
        let db = Connection::open(&path).unwrap();
        db.execute("UPDATE ledger_meta SET value='cc-switch-3217f725:session-usage-v3-six-source:unified-app-key-v3' WHERE key='importer_revision'",[]).unwrap();
        db.execute_batch("INSERT INTO compaction_barriers VALUES (1,2); INSERT INTO session_log_sync(file_path,last_modified,last_line_offset,last_synced_at) VALUES ('/codex/rollout-123.jsonl',1,1,1),('/claude/session.jsonl',1,1,1);").unwrap();
        init_local_ledger(&path).unwrap();
        verify_revision(&db).unwrap();
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM compaction_barriers", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            db.query_row("SELECT file_path FROM session_log_sync", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "/claude/session.jsonl"
        );
        init_local_ledger(&path).unwrap();
        verify_revision(&db).unwrap();
    }
    #[test]
    fn upgrade_reconciliation_and_scan_cursors_are_atomic_and_scoped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.db");
        init_local_ledger(&path).unwrap();
        let mut db = Connection::open(&path).unwrap();
        let id = "00000000-0000-4000-8000-000000000001";
        let deferred_id = "00000000-0000-4000-8000-000000000002";
        let file = dir.path().join(format!("rollout-{id}.jsonl"));
        fs::write(&file, "").unwrap();
        let deferred = dir.path().join(format!("rollout-{deferred_id}.jsonl"));
        fs::write(&deferred, "").unwrap();
        for (thread, index, collector) in [
            (id, 1, "local"),
            (id, 2, "cc-switch"),
            (deferred_id, 1, "local"),
        ] {
            db.execute("INSERT INTO proxy_request_logs(request_id,app_type,provider_id,model,created_at,last_collector,data_source) VALUES (?1,'codex','p','gpt-5.5',100,?2,'codex_session')",params![format!("codex_session:thread-v1:{thread}:{index}"),collector]).unwrap();
        }
        db.execute(
            "INSERT INTO ledger_meta VALUES ('codex_fast_reconcile','1')",
            [],
        )
        .unwrap();
        let stamps = HashMap::from([
            (file.clone(), file_stamp(&file)),
            (deferred.clone(), file_stamp(&deferred)),
        ]);
        let report = || ImportReport {
            scanned_paths: vec![file.clone(), deferred.clone()],
            deferred_paths: vec![deferred.clone()],
            ..Default::default()
        };
        {
            let tx = db.transaction().unwrap();
            reconcile_codex_upgrade(&tx, &report()).unwrap();
            record_scan_metadata(&tx, report(), &stamps).unwrap();
            // Simulate interruption before commit: neither cursors nor data advance.
        }
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM proxy_request_logs", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            3
        );
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM session_log_sync", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        let tx = db.transaction().unwrap();
        reconcile_codex_upgrade(&tx, &report()).unwrap();
        record_scan_metadata(&tx, report(), &stamps).unwrap();
        tx.commit().unwrap();
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM proxy_request_logs", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            2
        );
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM session_log_sync", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert!(db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM ledger_meta WHERE key='codex_fast_reconcile')",
                [],
                |r| r.get::<_, bool>(0)
            )
            .unwrap());
    }
}

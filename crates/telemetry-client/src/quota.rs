use crate::{post_json_with_retry, ClientConfig};
use anyhow::Context;
use chrono::DateTime;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    env,
    path::{Path, PathBuf},
    time::Duration,
};
use telemetry_core::{
    QuotaBatchResponse, QuotaMetric, QuotaMetricKind, QuotaObservation, QuotaProviderState,
    QuotaProviderStatus, QuotaTargetKind, QuotaUploadBatch, SCHEMA_VERSION,
};
use tokio::process::Command;
use uuid::Uuid;

pub const DEFAULT_INTERVAL_SECONDS: u64 = 60;
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_COMMAND_OUTPUT_BYTES: usize = 128 * 1024;
const DEFAULT_UPLOAD_BATCH_SIZE: usize = 512;

#[derive(Debug, Clone)]
pub struct QuotaConfig {
    pub cc_switch_db: PathBuf,
    pub quota_db: PathBuf,
    pub cli_override: Option<PathBuf>,
    pub interval: Duration,
    pub command_timeout: Duration,
    pub upload_batch_size: usize,
}

impl QuotaConfig {
    pub fn from_env(cc_switch_db: PathBuf) -> anyhow::Result<Self> {
        let interval_seconds = env::var("TELEMETRY_QUOTA_INTERVAL_SECONDS")
            .ok()
            .map(|value| {
                value
                    .parse::<u64>()
                    .with_context(|| format!("invalid TELEMETRY_QUOTA_INTERVAL_SECONDS={value:?}"))
            })
            .transpose()?
            .unwrap_or(DEFAULT_INTERVAL_SECONDS);
        Ok(Self {
            cc_switch_db,
            quota_db: env::var_os("TELEMETRY_QUOTA_DB")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("./data/quota-history.db")),
            cli_override: env::var_os("CC_SWITCH_CLI")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from),
            interval: Duration::from_secs(interval_seconds),
            command_timeout: COMMAND_TIMEOUT,
            upload_batch_size: DEFAULT_UPLOAD_BATCH_SIZE,
        })
    }

    pub fn enabled(&self) -> bool {
        !self.interval.is_zero()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexProvider {
    pub id: String,
    pub name: String,
}

#[derive(Debug)]
pub struct CollectedQuota {
    pub state: QuotaProviderState,
    pub observation: Option<QuotaObservation>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CliQuotaOutput {
    app: String,
    provider_id: String,
    target: Option<CliTarget>,
    status: String,
    available: bool,
    queried_at: i64,
    result: Option<CliQuotaResult>,
}

#[derive(Debug, Deserialize)]
struct CliTarget {
    kind: CliTargetKind,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
enum CliTargetKind {
    SubscriptionTool,
    CodexOAuth,
    UsageScript,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type", content = "quota")]
enum CliQuotaResult {
    Subscription(CliSubscriptionQuota),
    Script(CliUsageResult),
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CliSubscriptionQuota {
    success: bool,
    #[serde(default)]
    tiers: Vec<CliQuotaTier>,
    extra_usage: Option<CliExtraUsage>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CliQuotaTier {
    name: String,
    utilization: f64,
    resets_at: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CliExtraUsage {
    is_enabled: bool,
    monthly_limit: Option<f64>,
    used_credits: Option<f64>,
    utilization: Option<f64>,
    currency: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CliUsageResult {
    success: bool,
    data: Option<Vec<CliUsageData>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CliUsageData {
    plan_name: Option<String>,
    is_valid: Option<bool>,
    total: Option<f64>,
    used: Option<f64>,
    remaining: Option<f64>,
    unit: Option<String>,
}

fn public_label(value: &str, fallback: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.contains('@') || trimmed.chars().any(char::is_control) {
        return fallback.to_owned();
    }
    trimmed.chars().take(96).collect()
}

fn metric_key(prefix: &str, label: &str, index: usize) -> String {
    let slug = label
        .chars()
        .filter_map(|character| {
            if character.is_ascii_alphanumeric() {
                Some(character.to_ascii_lowercase())
            } else if matches!(character, '-' | '_' | ' ') {
                Some('-')
            } else {
                None
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_owned();
    if slug.is_empty() {
        let digest = Sha256::digest(label.as_bytes());
        format!("{prefix}:metric-{}-{index}", &format!("{digest:x}")[..12])
    } else {
        format!("{prefix}:{slug}")
    }
}

fn unique_key(
    prefix: &str,
    label: &str,
    index: usize,
    occurrences: &mut BTreeMap<String, usize>,
) -> String {
    let base = metric_key(prefix, label, index);
    let occurrence = occurrences.entry(base.clone()).or_default();
    *occurrence += 1;
    if *occurrence == 1 {
        base
    } else {
        format!("{base}-{}", *occurrence)
    }
}

fn normalized_seconds(milliseconds: i64) -> Option<i64> {
    (milliseconds > 0).then_some(milliseconds / 1_000)
}

fn reset_timestamp(value: Option<&str>) -> Option<i64> {
    value
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.timestamp())
}

fn target_kind(target: Option<&CliTarget>) -> Option<QuotaTargetKind> {
    target.map(|target| match target.kind {
        CliTargetKind::SubscriptionTool => QuotaTargetKind::SubscriptionTool,
        CliTargetKind::CodexOAuth => QuotaTargetKind::CodexOAuth,
        CliTargetKind::UsageScript => QuotaTargetKind::UsageScript,
    })
}

fn upstream_status(value: &str) -> Option<QuotaProviderStatus> {
    match value {
        "ok" => Some(QuotaProviderStatus::Ok),
        "not_available" => Some(QuotaProviderStatus::NotAvailable),
        "credential_parse_failed" => Some(QuotaProviderStatus::CredentialParseFailed),
        "login_expired" => Some(QuotaProviderStatus::LoginExpired),
        "query_failed" => Some(QuotaProviderStatus::QueryFailed),
        _ => None,
    }
}

fn finite(values: &[Option<f64>]) -> bool {
    values.iter().all(|value| value.is_none_or(f64::is_finite))
}

fn utilization_percent(
    explicit: Option<f64>,
    used: Option<f64>,
    remaining: Option<f64>,
    total: Option<f64>,
) -> Option<f64> {
    explicit.or_else(|| {
        let total = total.filter(|value| *value > 0.0)?;
        used.map(|value| value / total * 100.0)
            .or_else(|| remaining.map(|value| (total - value) / total * 100.0))
            .map(|value| value.clamp(0.0, 100.0))
    })
}

fn subscription_metrics(quota: CliSubscriptionQuota) -> Option<Vec<QuotaMetric>> {
    if !quota.success || quota.tiers.is_empty() {
        return None;
    }
    let mut keys = BTreeMap::new();
    let mut metrics = Vec::new();
    for (index, tier) in quota.tiers.into_iter().enumerate() {
        if !tier.utilization.is_finite() || !(0.0..=100.0).contains(&tier.utilization) {
            return None;
        }
        let label = public_label(&tier.name, &format!("Tier {}", index + 1));
        metrics.push(QuotaMetric {
            key: unique_key("subscription", &label, index, &mut keys),
            label,
            kind: QuotaMetricKind::UtilizationPercent,
            utilization_percent: Some(tier.utilization),
            used: None,
            remaining: None,
            total: None,
            unit: Some("%".to_owned()),
            resets_at: reset_timestamp(tier.resets_at.as_deref()),
        });
    }
    if let Some(extra) = quota.extra_usage.filter(|extra| extra.is_enabled) {
        if !finite(&[extra.monthly_limit, extra.used_credits, extra.utilization])
            || extra
                .utilization
                .is_some_and(|value| !(0.0..=100.0).contains(&value))
        {
            return None;
        }
        let remaining = match (extra.monthly_limit, extra.used_credits) {
            (Some(total), Some(used)) => Some((total - used).max(0.0)),
            _ => None,
        };
        let utilization_percent = utilization_percent(
            extra.utilization,
            extra.used_credits,
            remaining,
            extra.monthly_limit,
        );
        metrics.push(QuotaMetric {
            key: "subscription:extra-usage".to_owned(),
            label: "Extra usage".to_owned(),
            kind: QuotaMetricKind::Balance,
            utilization_percent,
            used: extra.used_credits,
            remaining,
            total: extra.monthly_limit,
            unit: extra
                .currency
                .as_deref()
                .map(|value| public_label(value, "credits")),
            resets_at: None,
        });
    }
    Some(metrics)
}

fn script_metrics(result: CliUsageResult) -> Option<Vec<QuotaMetric>> {
    if !result.success {
        return None;
    }
    let mut keys = BTreeMap::new();
    let metrics = result
        .data?
        .into_iter()
        .enumerate()
        .filter(|(_, item)| item.is_valid != Some(false))
        .map(|(index, item)| {
            if !finite(&[item.total, item.used, item.remaining]) {
                return None;
            }
            let raw_label = item
                .plan_name
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or("Plan");
            let label = public_label(raw_label, &format!("Plan {}", index + 1));
            let utilization_percent =
                utilization_percent(None, item.used, item.remaining, item.total);
            Some(QuotaMetric {
                key: unique_key("script", &label, index, &mut keys),
                label,
                kind: QuotaMetricKind::Balance,
                utilization_percent,
                used: item.used,
                remaining: item.remaining,
                total: item.total,
                unit: item
                    .unit
                    .as_deref()
                    .map(|value| public_label(value, "units")),
                resets_at: None,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    (!metrics.is_empty()).then_some(metrics)
}

fn state_only(
    provider: &CodexProvider,
    status: QuotaProviderStatus,
    target_kind: Option<QuotaTargetKind>,
    checked_at: i64,
) -> CollectedQuota {
    CollectedQuota {
        state: QuotaProviderState {
            app_type: "codex".to_owned(),
            provider_id: provider.id.clone(),
            provider_name: public_label(&provider.name, "Codex provider"),
            status,
            target_kind,
            checked_at,
        },
        observation: None,
    }
}

fn normalize_output(provider: &CodexProvider, output: CliQuotaOutput) -> CollectedQuota {
    let fallback_time = chrono::Utc::now().timestamp();
    let checked_at = normalized_seconds(output.queried_at).unwrap_or(fallback_time);
    let target = target_kind(output.target.as_ref());
    if output.app != "codex" || output.provider_id != provider.id {
        return state_only(
            provider,
            QuotaProviderStatus::InvalidOutput,
            target,
            checked_at,
        );
    }
    let Some(status) = upstream_status(&output.status) else {
        return state_only(
            provider,
            QuotaProviderStatus::InvalidOutput,
            target,
            checked_at,
        );
    };
    if status != QuotaProviderStatus::Ok {
        return state_only(provider, status, target, checked_at);
    }
    if !output.available {
        return state_only(
            provider,
            QuotaProviderStatus::InvalidOutput,
            target,
            checked_at,
        );
    }
    let metrics = match output.result {
        Some(CliQuotaResult::Subscription(quota)) => subscription_metrics(quota),
        Some(CliQuotaResult::Script(result)) => script_metrics(result),
        None => None,
    };
    let Some(metrics) = metrics else {
        return state_only(
            provider,
            QuotaProviderStatus::InvalidOutput,
            target,
            checked_at,
        );
    };
    CollectedQuota {
        state: QuotaProviderState {
            app_type: "codex".to_owned(),
            provider_id: provider.id.clone(),
            provider_name: public_label(&provider.name, "Codex provider"),
            status: QuotaProviderStatus::Ok,
            target_kind: target,
            checked_at,
        },
        observation: Some(QuotaObservation {
            observation_id: Uuid::new_v4().to_string(),
            app_type: "codex".to_owned(),
            provider_id: provider.id.clone(),
            sampled_at: checked_at,
            metrics,
        }),
    }
}

pub fn read_codex_providers(path: &Path) -> anyhow::Result<Vec<CodexProvider>> {
    let connection = Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .with_context(|| format!("open cc-switch database read-only: {}", path.display()))?;
    connection.busy_timeout(Duration::from_secs(2))?;
    let mut statement = connection.prepare(
        "SELECT id,name FROM providers
         WHERE app_type='codex' AND TRIM(id)<>''
         ORDER BY id",
    )?;
    let providers = statement
        .query_map([], |row| {
            Ok(CodexProvider {
                id: row.get(0)?,
                name: row.get(1)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(providers)
}

fn executable_file(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

pub fn resolve_cli(config: &QuotaConfig) -> anyhow::Result<PathBuf> {
    if let Some(path) = &config.cli_override {
        if executable_file(path) {
            return Ok(path.clone());
        }
        anyhow::bail!(
            "CC_SWITCH_CLI is not an executable file: {}",
            path.display()
        );
    }
    if let Some(path) = env::var_os("PATH").and_then(|path| {
        env::split_paths(&path)
            .map(|directory| directory.join("cc-switch-cli"))
            .find(|candidate| executable_file(candidate))
    }) {
        return Ok(path);
    }
    if let Some(path) = dirs::home_dir()
        .map(|home| home.join(".local/bin/cc-switch-cli"))
        .filter(|candidate| executable_file(candidate))
    {
        return Ok(path);
    }
    anyhow::bail!("cc-switch-cli was not found in PATH or $HOME/.local/bin")
}

async fn query_provider_with_timeout(
    executable: &Path,
    provider: &CodexProvider,
    timeout: Duration,
) -> CollectedQuota {
    let checked_at = chrono::Utc::now().timestamp();
    let output = tokio::time::timeout(
        timeout,
        Command::new(executable)
            .args([
                "--app",
                "codex",
                "provider",
                "quota",
                &provider.id,
                "--json",
            ])
            .kill_on_drop(true)
            .output(),
    )
    .await;
    match output {
        Err(_) => state_only(provider, QuotaProviderStatus::TimedOut, None, checked_at),
        Ok(Err(_)) => state_only(
            provider,
            QuotaProviderStatus::CommandFailed,
            None,
            checked_at,
        ),
        Ok(Ok(output)) if !output.status.success() => state_only(
            provider,
            QuotaProviderStatus::CommandFailed,
            None,
            checked_at,
        ),
        Ok(Ok(output)) if output.stdout.len() > MAX_COMMAND_OUTPUT_BYTES => state_only(
            provider,
            QuotaProviderStatus::InvalidOutput,
            None,
            checked_at,
        ),
        Ok(Ok(output)) => match serde_json::from_slice::<CliQuotaOutput>(&output.stdout) {
            Ok(output) => normalize_output(provider, output),
            Err(_) => state_only(
                provider,
                QuotaProviderStatus::InvalidOutput,
                None,
                checked_at,
            ),
        },
    }
}

pub async fn collect_cycle(config: &QuotaConfig) -> anyhow::Result<Vec<CollectedQuota>> {
    let providers = read_codex_providers(&config.cc_switch_db)?;
    let executable = resolve_cli(config);
    let mut collected = Vec::with_capacity(providers.len());
    for provider in providers {
        let item = match &executable {
            Ok(executable) => {
                query_provider_with_timeout(executable, &provider, config.command_timeout).await
            }
            Err(_) => state_only(
                &provider,
                QuotaProviderStatus::CommandFailed,
                None,
                chrono::Utc::now().timestamp(),
            ),
        };
        collected.push(item);
    }
    Ok(collected)
}

pub fn init_db(path: &Path) -> anyhow::Result<Connection> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    let connection = Connection::open(path)?;
    connection.busy_timeout(Duration::from_secs(5))?;
    connection.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA foreign_keys=ON;
         CREATE TABLE IF NOT EXISTS quota_provider_states (
             app_type TEXT NOT NULL,
             provider_id TEXT NOT NULL,
             provider_name TEXT NOT NULL,
             status TEXT NOT NULL,
             target_kind TEXT,
             checked_at INTEGER NOT NULL,
             PRIMARY KEY (app_type,provider_id)
         );
         CREATE TABLE IF NOT EXISTS quota_observations (
             sequence INTEGER PRIMARY KEY AUTOINCREMENT,
             observation_id TEXT NOT NULL UNIQUE,
             app_type TEXT NOT NULL,
             provider_id TEXT NOT NULL,
             sampled_at INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_local_quota_series_time
             ON quota_observations(provider_id,sampled_at);
         CREATE TABLE IF NOT EXISTS quota_metrics (
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
             PRIMARY KEY (observation_id,metric_key),
             FOREIGN KEY (observation_id) REFERENCES quota_observations(observation_id)
                 ON DELETE CASCADE
         );
         CREATE TABLE IF NOT EXISTS quota_upload_cursors (
             remote_key TEXT PRIMARY KEY,
             sequence INTEGER NOT NULL
         );",
    )?;
    Ok(connection)
}

fn enum_text<T: Serialize>(value: &T) -> anyhow::Result<String> {
    serde_json::to_value(value)?
        .as_str()
        .map(str::to_owned)
        .context("quota enum must serialize as a string")
}

fn enum_value<T: DeserializeOwned>(value: String) -> anyhow::Result<T> {
    Ok(serde_json::from_value(serde_json::Value::String(value))?)
}

pub fn persist_cycle(path: &Path, collected: &[CollectedQuota]) -> anyhow::Result<usize> {
    let mut connection = init_db(path)?;
    let transaction = connection.transaction()?;
    let mut observations = 0;
    for item in collected {
        transaction.execute(
            "INSERT INTO quota_provider_states (
                 app_type,provider_id,provider_name,status,target_kind,checked_at
             ) VALUES (?1,?2,?3,?4,?5,?6)
             ON CONFLICT(app_type,provider_id) DO UPDATE SET
                 provider_name=excluded.provider_name,
                 status=excluded.status,
                 target_kind=excluded.target_kind,
                 checked_at=excluded.checked_at
             WHERE excluded.checked_at >= quota_provider_states.checked_at",
            params![
                item.state.app_type,
                item.state.provider_id,
                item.state.provider_name,
                enum_text(&item.state.status)?,
                item.state.target_kind.as_ref().map(enum_text).transpose()?,
                item.state.checked_at,
            ],
        )?;
        let Some(observation) = &item.observation else {
            continue;
        };
        transaction.execute(
            "INSERT INTO quota_observations (
                 observation_id,app_type,provider_id,sampled_at
             ) VALUES (?1,?2,?3,?4)",
            params![
                observation.observation_id,
                observation.app_type,
                observation.provider_id,
                observation.sampled_at,
            ],
        )?;
        for metric in &observation.metrics {
            transaction.execute(
                "INSERT INTO quota_metrics (
                     observation_id,metric_key,metric_label,metric_kind,
                     utilization_percent,used,remaining,total,unit,resets_at
                 ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                params![
                    observation.observation_id,
                    metric.key,
                    metric.label,
                    enum_text(&metric.kind)?,
                    metric.utilization_percent,
                    metric.used,
                    metric.remaining,
                    metric.total,
                    metric.unit,
                    metric.resets_at,
                ],
            )?;
        }
        observations += 1;
    }
    transaction.commit()?;
    Ok(observations)
}

fn remote_key(config: &ClientConfig) -> String {
    let digest = Sha256::digest(
        format!(
            "{}\0{}",
            config.server_url.trim_end_matches('/'),
            config.auth_token
        )
        .as_bytes(),
    );
    format!("{digest:x}")
}

fn load_states(connection: &Connection) -> anyhow::Result<Vec<QuotaProviderState>> {
    let mut statement = connection.prepare(
        "SELECT app_type,provider_id,provider_name,status,target_kind,checked_at
         FROM quota_provider_states ORDER BY app_type,provider_id",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(
            |(app_type, provider_id, provider_name, status, target_kind, checked_at)| {
                Ok(QuotaProviderState {
                    app_type,
                    provider_id,
                    provider_name,
                    status: enum_value(status)?,
                    target_kind: target_kind.map(enum_value).transpose()?,
                    checked_at,
                })
            },
        )
        .collect()
}

fn load_metrics(connection: &Connection, observation_id: &str) -> anyhow::Result<Vec<QuotaMetric>> {
    let mut statement = connection.prepare(
        "SELECT metric_key,metric_label,metric_kind,utilization_percent,used,remaining,
                total,unit,resets_at
         FROM quota_metrics WHERE observation_id=?1 ORDER BY metric_key",
    )?;
    let rows = statement
        .query_map([observation_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<f64>>(3)?,
                row.get::<_, Option<f64>>(4)?,
                row.get::<_, Option<f64>>(5)?,
                row.get::<_, Option<f64>>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<i64>>(8)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(
            |(key, label, kind, utilization_percent, used, remaining, total, unit, resets_at)| {
                Ok(QuotaMetric {
                    key,
                    label,
                    kind: enum_value(kind)?,
                    utilization_percent,
                    used,
                    remaining,
                    total,
                    unit,
                    resets_at,
                })
            },
        )
        .collect()
}

struct PendingBatch {
    payload: QuotaUploadBatch,
    last_sequence: Option<i64>,
}

fn load_pending(path: &Path, remote: &str, limit: usize) -> anyhow::Result<PendingBatch> {
    let connection = init_db(path)?;
    let cursor = connection
        .query_row(
            "SELECT sequence FROM quota_upload_cursors WHERE remote_key=?1",
            [remote],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .unwrap_or(0);
    let raw = {
        let mut statement = connection.prepare(
            "SELECT sequence,observation_id,app_type,provider_id,sampled_at
             FROM quota_observations WHERE sequence>?1 ORDER BY sequence LIMIT ?2",
        )?;
        let rows = statement
            .query_map(params![cursor, limit.max(1) as i64], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows
    };
    let mut observations = Vec::with_capacity(raw.len());
    let mut last_sequence = None;
    for (sequence, observation_id, app_type, provider_id, sampled_at) in raw {
        observations.push(QuotaObservation {
            metrics: load_metrics(&connection, &observation_id)?,
            observation_id,
            app_type,
            provider_id,
            sampled_at,
        });
        last_sequence = Some(sequence);
    }
    Ok(PendingBatch {
        payload: QuotaUploadBatch {
            schema_version: SCHEMA_VERSION,
            provider_states: load_states(&connection)?,
            observations,
        },
        last_sequence,
    })
}

fn save_cursor(path: &Path, remote: &str, sequence: i64) -> anyhow::Result<()> {
    let connection = init_db(path)?;
    connection.execute(
        "INSERT INTO quota_upload_cursors(remote_key,sequence) VALUES (?1,?2)
         ON CONFLICT(remote_key) DO UPDATE SET sequence=MAX(sequence,excluded.sequence)",
        params![remote, sequence],
    )?;
    Ok(())
}

fn reset_cursor(path: &Path, remote: &str) -> anyhow::Result<()> {
    let connection = init_db(path)?;
    connection.execute(
        "INSERT INTO quota_upload_cursors(remote_key,sequence) VALUES (?1,0)
         ON CONFLICT(remote_key) DO UPDATE SET sequence=0",
        [remote],
    )?;
    Ok(())
}

pub async fn upload_pending(
    quota: &QuotaConfig,
    client: &ClientConfig,
    replay: bool,
) -> anyhow::Result<(usize, usize)> {
    let remote = remote_key(client);
    if replay {
        reset_cursor(&quota.quota_db, &remote)?;
    }
    let url = format!(
        "{}/v3/quota/observations",
        client.server_url.trim_end_matches('/')
    );
    let mut accepted = 0;
    let mut duplicates = 0;
    let mut first = true;
    loop {
        let batch = load_pending(&quota.quota_db, &remote, quota.upload_batch_size)?;
        if batch.payload.observations.is_empty()
            && (!first || batch.payload.provider_states.is_empty())
        {
            break;
        }
        let expected = batch.payload.observations.len();
        let response = post_json_with_retry(client, &url, &batch.payload)
            .await?
            .json::<QuotaBatchResponse>()
            .await
            .with_context(|| format!("decode quota upload response from {url}"))?;
        if response.accepted.len() + response.duplicates.len() != expected
            || response.provider_states != batch.payload.provider_states.len()
        {
            anyhow::bail!(
                "quota acknowledgement mismatch: sent={expected} accepted={} duplicates={} states={}/{}",
                response.accepted.len(),
                response.duplicates.len(),
                response.provider_states,
                batch.payload.provider_states.len()
            );
        }
        accepted += response.accepted.len();
        duplicates += response.duplicates.len();
        if let Some(sequence) = batch.last_sequence {
            save_cursor(&quota.quota_db, &remote, sequence)?;
        } else {
            break;
        }
        first = false;
    }
    Ok((accepted, duplicates))
}

pub async fn collect_store_upload(
    quota: &QuotaConfig,
    client: &ClientConfig,
) -> anyhow::Result<(usize, usize, usize)> {
    let collected = collect_cycle(quota).await?;
    let observations = persist_cycle(&quota.quota_db, &collected)?;
    let (accepted, duplicates) = upload_pending(quota, client, false).await?;
    Ok((observations, accepted, duplicates))
}

pub async fn run(quota: QuotaConfig, client: ClientConfig) {
    if !quota.enabled() {
        return;
    }
    let mut interval = tokio::time::interval(quota.interval);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        interval.tick().await;
        match collect_store_upload(&quota, &client).await {
            Ok((stored, accepted, duplicates)) => {
                eprintln!("quota sync: stored={stored} accepted={accepted} duplicates={duplicates}")
            }
            Err(error) => eprintln!("quota sync error: {error}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider() -> CodexProvider {
        CodexProvider {
            id: "provider-a".to_owned(),
            name: "Provider A".to_owned(),
        }
    }

    fn subscription_json(status: &str, available: bool) -> String {
        serde_json::json!({
            "app": "codex",
            "providerId": "provider-a",
            "providerName": "ignored@example.com",
            "target": {
                "appType": "codex",
                "providerId": "provider-a",
                "providerName": "ignored@example.com",
                "kind": { "kind": "codexOAuth", "accountId": "must-not-escape" }
            },
            "status": status,
            "available": available,
            "queriedAt": 1_800_000_000_000_i64,
            "result": {
                "type": "subscription",
                "quota": {
                    "tool": "codex_oauth",
                    "credentialStatus": "valid",
                    "credentialMessage": "must-not-escape",
                    "success": true,
                    "tiers": [{
                        "name": "five_hour",
                        "utilization": 42.5,
                        "resetsAt": "2027-01-15T08:00:00Z"
                    }],
                    "extraUsage": null,
                    "error": null,
                    "queriedAt": 1_800_000_000_000_i64
                }
            },
            "error": "must-not-escape"
        })
        .to_string()
    }

    #[test]
    fn successful_subscription_is_allowlist_normalized() {
        let raw: CliQuotaOutput = serde_json::from_str(&subscription_json("ok", true)).unwrap();
        let collected = normalize_output(&provider(), raw);
        assert_eq!(collected.state.status, QuotaProviderStatus::Ok);
        assert_eq!(
            collected.state.target_kind,
            Some(QuotaTargetKind::CodexOAuth)
        );
        let observation = collected.observation.unwrap();
        assert_eq!(observation.sampled_at, 1_800_000_000);
        assert_eq!(observation.metrics[0].utilization_percent, Some(42.5));
        let serialized = serde_json::to_string(&observation).unwrap();
        assert!(!serialized.contains("accountId"));
        assert!(!serialized.contains("must-not-escape"));
    }

    #[test]
    fn balances_derive_bounded_utilization_when_total_is_available() {
        let metrics = script_metrics(CliUsageResult {
            success: true,
            data: Some(vec![
                CliUsageData {
                    plan_name: Some("used".to_owned()),
                    is_valid: Some(true),
                    total: Some(20.0),
                    used: Some(2.0),
                    remaining: None,
                    unit: Some("USD".to_owned()),
                },
                CliUsageData {
                    plan_name: Some("remaining".to_owned()),
                    is_valid: Some(true),
                    total: Some(20.0),
                    used: None,
                    remaining: Some(18.0),
                    unit: Some("USD".to_owned()),
                },
                CliUsageData {
                    plan_name: Some("capped".to_owned()),
                    is_valid: Some(true),
                    total: Some(20.0),
                    used: Some(30.0),
                    remaining: None,
                    unit: Some("USD".to_owned()),
                },
                CliUsageData {
                    plan_name: Some("amount-only".to_owned()),
                    is_valid: Some(true),
                    total: None,
                    used: None,
                    remaining: Some(18.0),
                    unit: Some("USD".to_owned()),
                },
            ]),
        })
        .unwrap();
        assert_eq!(metrics[0].utilization_percent, Some(10.0));
        assert_eq!(metrics[1].utilization_percent, Some(10.0));
        assert_eq!(metrics[2].utilization_percent, Some(100.0));
        assert_eq!(metrics[3].utilization_percent, None);

        assert_eq!(
            utilization_percent(None, Some(2.0), None, Some(20.0)),
            Some(10.0)
        );
        assert_eq!(
            utilization_percent(None, None, Some(18.0), Some(20.0)),
            Some(10.0)
        );
    }

    #[test]
    fn status_and_available_are_not_inferred_from_exit_success() {
        let raw: CliQuotaOutput =
            serde_json::from_str(&subscription_json("login_expired", false)).unwrap();
        let collected = normalize_output(&provider(), raw);
        assert_eq!(collected.state.status, QuotaProviderStatus::LoginExpired);
        assert!(collected.observation.is_none());

        let raw: CliQuotaOutput = serde_json::from_str(&subscription_json("ok", false)).unwrap();
        let collected = normalize_output(&provider(), raw);
        assert_eq!(collected.state.status, QuotaProviderStatus::InvalidOutput);
        assert!(collected.observation.is_none());
    }

    #[test]
    fn cadence_timeout_dynamic_tiers_and_provider_identity_are_stable() {
        assert_eq!(DEFAULT_INTERVAL_SECONDS, 60);
        assert_eq!(COMMAND_TIMEOUT, Duration::from_secs(20));

        let mut value =
            serde_json::from_str::<serde_json::Value>(&subscription_json("ok", true)).unwrap();
        value["result"]["quota"]["tiers"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "name": "seven_day",
                "utilization": 17.0,
                "resetsAt": null
            }));
        let collected = normalize_output(&provider(), serde_json::from_value(value).unwrap());
        let metrics = &collected.observation.as_ref().unwrap().metrics;
        assert_eq!(metrics.len(), 2);
        assert_ne!(metrics[0].key, metrics[1].key);

        let provider_b = CodexProvider {
            id: "provider-b".to_owned(),
            name: "Provider B".to_owned(),
        };
        let mut value =
            serde_json::from_str::<serde_json::Value>(&subscription_json("ok", true)).unwrap();
        value["providerId"] = serde_json::Value::String(provider_b.id.clone());
        let collected_b = normalize_output(&provider_b, serde_json::from_value(value).unwrap());
        assert_eq!(collected_b.observation.unwrap().provider_id, "provider-b");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn mock_executable_receives_exact_argv() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("cc-switch-cli");
        let argv = directory.path().join("argv");
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nprintf '%s\\n' '{}'\n",
            argv.display(),
            subscription_json("ok", true).replace('\'', "'\\''")
        );
        std::fs::write(&executable, script).unwrap();
        let mut permissions = executable.metadata().unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&executable, permissions).unwrap();

        let collected =
            query_provider_with_timeout(&executable, &provider(), Duration::from_secs(2)).await;
        assert_eq!(collected.state.status, QuotaProviderStatus::Ok);
        assert_eq!(
            std::fs::read_to_string(argv).unwrap(),
            "--app\ncodex\nprovider\nquota\nprovider-a\n--json\n"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn subprocess_nonzero_invalid_json_and_timeout_are_states_only() {
        use std::os::unix::fs::PermissionsExt;

        async fn run_script(body: &str, timeout: Duration) -> CollectedQuota {
            let directory = tempfile::tempdir().unwrap();
            let executable = directory.path().join("cc-switch-cli");
            std::fs::write(&executable, format!("#!/bin/sh\n{body}\n")).unwrap();
            let mut permissions = executable.metadata().unwrap().permissions();
            permissions.set_mode(0o700);
            std::fs::set_permissions(&executable, permissions).unwrap();
            query_provider_with_timeout(&executable, &provider(), timeout).await
        }

        let nonzero = run_script("exit 7", Duration::from_secs(1)).await;
        assert_eq!(nonzero.state.status, QuotaProviderStatus::CommandFailed);
        assert!(nonzero.observation.is_none());

        let invalid = run_script("printf 'not-json\\n'", Duration::from_secs(1)).await;
        assert_eq!(invalid.state.status, QuotaProviderStatus::InvalidOutput);
        assert!(invalid.observation.is_none());

        let timed_out = run_script("sleep 1", Duration::from_millis(10)).await;
        assert_eq!(timed_out.state.status, QuotaProviderStatus::TimedOut);
        assert!(timed_out.observation.is_none());
    }

    #[test]
    fn durable_ledger_keeps_observations_and_cursor_is_remote_scoped() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("quota.db");
        let raw: CliQuotaOutput = serde_json::from_str(&subscription_json("ok", true)).unwrap();
        let collected = normalize_output(&provider(), raw);
        let observation_id = collected
            .observation
            .as_ref()
            .unwrap()
            .observation_id
            .clone();
        assert_eq!(persist_cycle(&path, &[collected]).unwrap(), 1);
        let pending = load_pending(&path, "remote-a", 512).unwrap();
        assert_eq!(
            pending.payload.observations[0].observation_id,
            observation_id
        );
        save_cursor(&path, "remote-a", pending.last_sequence.unwrap()).unwrap();
        assert!(load_pending(&path, "remote-a", 512)
            .unwrap()
            .payload
            .observations
            .is_empty());
        assert_eq!(
            load_pending(&path, "remote-b", 512)
                .unwrap()
                .payload
                .observations
                .len(),
            1
        );
        reset_cursor(&path, "remote-a").unwrap();
        assert_eq!(
            load_pending(&path, "remote-a", 512)
                .unwrap()
                .payload
                .observations
                .len(),
            1
        );
        let connection = init_db(&path).unwrap();
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM quota_observations", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn replay_is_idempotent_against_the_server() {
        let directory = tempfile::tempdir().unwrap();
        let quota_path = directory.path().join("quota.db");
        let telemetry_path = directory.path().join("telemetry.db");
        let raw: CliQuotaOutput = serde_json::from_str(&subscription_json("ok", true)).unwrap();
        persist_cycle(&quota_path, &[normalize_output(&provider(), raw)]).unwrap();

        let connection = telemetry_server::init_db(&telemetry_path).unwrap();
        let (_node, token) = telemetry_server::nodes::create(&connection, "node-a").unwrap();
        let state = telemetry_server::ServerState::new(connection, telemetry_path.clone(), None);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, telemetry_server::router(state))
                .await
                .unwrap();
        });
        let client = ClientConfig {
            cc_switch_db: directory.path().join("unused.db"),
            server_url: format!("http://{address}"),
            auth_token: token,
            batch_size: 512,
        };
        let quota = QuotaConfig {
            cc_switch_db: directory.path().join("unused-cc-switch.db"),
            quota_db: quota_path,
            cli_override: None,
            interval: Duration::from_secs(60),
            command_timeout: COMMAND_TIMEOUT,
            upload_batch_size: 512,
        };

        assert_eq!(
            upload_pending(&quota, &client, false).await.unwrap(),
            (1, 0)
        );
        assert_eq!(
            upload_pending(&quota, &client, false).await.unwrap(),
            (0, 0)
        );
        assert_eq!(upload_pending(&quota, &client, true).await.unwrap(), (0, 1));
        let central = Connection::open(telemetry_path).unwrap();
        assert_eq!(
            central
                .query_row("SELECT COUNT(*) FROM quota_observations", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        server.abort();
    }
}

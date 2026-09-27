//! Neutral raw-session importers shared by telemetry and cc-switch adapters.
//!
//! The importers deliberately do not know about Tauri, cc-switch's `Database`,
//! pricing DAO, or the destination schema. They emit stable usage records and
//! leave persistence, deduplication, and pricing to the caller.

mod codex;
pub mod metadata;
mod pi;
#[cfg(test)]
mod provenance;

use anyhow::Context;
use chrono::DateTime;
use rusqlite::Connection;
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

pub const IMPORTER_SOURCE_COMMIT: &str = "87d966b7f887adfe0e9856ee0f7e93cc8efc874f";
pub const IMPORTER_REVISION: &str = "cc-switch-87d966b7:session-usage-v4-fast";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageIdentity {
    pub semantic_id: String,
    pub has_entry_id: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageRecord {
    pub request_id: String,
    pub app_type: String,
    pub provider_id: String,
    pub provider_type: String,
    pub data_source: String,
    pub model: String,
    pub request_model: String,
    pub pricing_model: Option<String>,
    pub service_tier: Option<String>,
    pub service_tier_source: Option<String>,
    pub reasoning_effort: Option<String>,
    pub service_tier_pricing_version: Option<i64>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
    pub input_token_semantics: i64,
    pub created_at: i64,
    pub session_id: Option<String>,
    pub source_path: PathBuf,
    pub is_streaming: bool,
    pub status_code: i64,
    pub latency_ms: i64,
    pub reported_total_cost_usd: Option<String>,
    pub identity: Option<UsageIdentity>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ImportReport {
    pub records: Vec<UsageRecord>,
    pub files_scanned: u64,
    pub skipped: u64,
    pub scanned_paths: Vec<PathBuf>,
    pub deferred_paths: Vec<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct SourceConfig {
    pub claude_dir: PathBuf,
    pub codex_dir: PathBuf,
    pub gemini_dir: PathBuf,
    pub opencode_db: PathBuf,
    pub grok_dir: PathBuf,
    pub pi_dir: PathBuf,
}

pub fn import_all(config: &SourceConfig) -> anyhow::Result<ImportReport> {
    import_all_filtered(config, |_| true)
}

pub fn import_all_filtered<F>(config: &SourceConfig, should_scan: F) -> anyhow::Result<ImportReport>
where
    F: Fn(&Path) -> bool,
{
    let mut report = ImportReport::default();
    import_claude_files(
        &config.claude_dir.join("projects"),
        &mut report,
        &should_scan,
    )?;
    codex::import(&config.codex_dir, &mut report, &should_scan)?;
    import_gemini_files(&config.gemini_dir.join("tmp"), &mut report, &should_scan)?;
    import_opencode(&config.opencode_db, &mut report, &should_scan)?;
    import_grok_tree(&config.grok_dir, &mut report, &should_scan)?;
    pi::import_pi_files(&config.pi_dir, &mut report, &should_scan)?;
    Ok(report)
}

fn import_claude_files(
    root: &Path,
    report: &mut ImportReport,
    should_scan: &impl Fn(&Path) -> bool,
) -> anyhow::Result<()> {
    let mut files = Vec::new();
    let Ok(projects) = fs::read_dir(root) else {
        return Ok(());
    };
    for project in projects.flatten().filter(|entry| entry.path().is_dir()) {
        let project_path = project.path();
        let Ok(entries) = fs::read_dir(&project_path) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) == Some("jsonl") {
                files.push(path);
            } else if path.is_dir() {
                let subagents = path.join("subagents");
                collect_direct_jsonl(&subagents, &mut files);
                let workflows = subagents.join("workflows");
                if let Ok(workflow_entries) = fs::read_dir(workflows) {
                    for workflow in workflow_entries
                        .flatten()
                        .filter(|entry| entry.path().is_dir())
                    {
                        collect_direct_jsonl(&workflow.path(), &mut files);
                    }
                }
            }
        }
    }
    files.sort();
    for path in files {
        if !should_scan(&path) {
            continue;
        }
        report.files_scanned += 1;
        report.scanned_paths.push(path.clone());
        report.records.extend(parse_claude_file(&path, "")?);
    }
    Ok(())
}

fn collect_direct_jsonl(root: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    files.extend(
        entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("jsonl")),
    );
}

fn collect_named_files(root: &Path, name: &str, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_named_files(&path, name, files);
        } else if path.file_name().and_then(|value| value.to_str()) == Some(name) {
            files.push(path);
        }
    }
}

fn import_gemini_files(
    root: &Path,
    report: &mut ImportReport,
    should_scan: &impl Fn(&Path) -> bool,
) -> anyhow::Result<()> {
    let Ok(projects) = fs::read_dir(root) else {
        return Ok(());
    };
    let mut files = Vec::new();
    for project in projects.flatten() {
        let chats = project.path().join("chats");
        let Ok(entries) = fs::read_dir(chats) else {
            continue;
        };
        files.extend(entries.flatten().map(|entry| entry.path()).filter(|path| {
            path.file_name()
                .and_then(|value| value.to_str())
                .is_some_and(|name| name.starts_with("session-") && name.ends_with(".json"))
        }));
    }
    files.sort();
    for path in files {
        if !should_scan(&path) {
            continue;
        }
        report.files_scanned += 1;
        report.scanned_paths.push(path.clone());
        import_gemini_file(&path, report)?;
    }
    Ok(())
}

fn import_gemini_file(path: &Path, report: &mut ImportReport) -> anyhow::Result<()> {
    let value: Value = serde_json::from_str(&fs::read_to_string(path)?)?;
    let session_id = value
        .get("sessionId")
        .and_then(Value::as_str)
        .map(str::to_owned);
    for (index, item) in value
        .get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        if item.get("type").and_then(Value::as_str) != Some("gemini") {
            continue;
        }
        let Some(tokens) = item.get("tokens") else {
            continue;
        };
        let input = number(tokens, &["input"]);
        let output = number(tokens, &["output"]) + number(tokens, &["thoughts"]);
        let cached = number(tokens, &["cached"]);
        if input + output + cached == 0 {
            continue;
        }
        let message_id = string(item, &["id", "message_id"], &format!("idx{index}"));
        let model = string(item, &["model"], "unknown");
        report.records.push(UsageRecord {
            request_id: format!(
                "gemini_session:{}:{message_id}",
                session_id.as_deref().unwrap_or("unknown")
            ),
            app_type: "gemini".into(),
            provider_id: "_gemini_session".into(),
            provider_type: "gemini_session".into(),
            data_source: "gemini_session".into(),
            model: model.clone(),
            request_model: model,
            pricing_model: None,
            service_tier: None,
            service_tier_source: None,
            reasoning_effort: None,
            service_tier_pricing_version: None,
            input_tokens: input,
            output_tokens: output,
            cache_read_tokens: cached,
            cache_creation_tokens: 0,
            input_token_semantics: 0,
            created_at: timestamp(item.get("timestamp")),
            session_id: session_id.clone(),
            source_path: path.to_owned(),
            is_streaming: true,
            status_code: 200,
            latency_ms: 0,
            reported_total_cost_usd: None,
            identity: None,
        });
    }
    Ok(())
}

fn timestamp(value: Option<&Value>) -> i64 {
    value
        .and_then(|v| {
            v.as_str()
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        })
        .map(|date| date.timestamp())
        .or_else(|| {
            value.and_then(|value| {
                value.as_i64().or_else(|| {
                    value.as_u64().map(|value| {
                        if value > 100_000_000_000 {
                            (value / 1000) as i64
                        } else {
                            value as i64
                        }
                    })
                })
            })
        })
        .unwrap_or_else(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0)
        })
}

fn event_timestamp(value: Option<&Value>) -> Option<i64> {
    let value = value?;
    if let Some(number) = value.as_i64() {
        return Some(if number > 100_000_000_000 {
            number / 1000
        } else {
            number
        });
    }
    value
        .as_str()
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.timestamp())
}

fn reported_cost(value: &Value) -> Option<String> {
    let ticks = value
        .get("costUsdTicks")
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)?;
    let whole = ticks / 10_000_000_000;
    let fraction = ticks % 10_000_000_000;
    Some(format!("{whole}.{fraction:010}"))
}

fn string(value: &Value, keys: &[&str], default: &str) -> String {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
        .filter(|value| !value.is_empty())
        .unwrap_or(default)
        .to_owned()
}

fn number(value: &Value, keys: &[&str]) -> i64 {
    keys.iter()
        .find_map(|key| {
            value.get(*key).and_then(|value| {
                value
                    .as_i64()
                    .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
            })
        })
        .unwrap_or(0)
}

fn usage(value: &Value) -> Option<(i64, i64, i64, i64)> {
    let usage = value
        .get("usage")
        .or_else(|| {
            value
                .get("message")
                .and_then(|message| message.get("usage"))
        })
        .or_else(|| {
            value
                .get("payload")
                .and_then(|payload| payload.get("usage"))
        })
        .or_else(|| value.get("tokens"))?;
    let input = number(
        usage,
        &["input_tokens", "input", "prompt_tokens", "promptTokenCount"],
    );
    let output = number(
        usage,
        &[
            "output_tokens",
            "output",
            "completion_tokens",
            "candidatesTokenCount",
        ],
    );
    let cached = number(
        usage,
        &[
            "cached_input_tokens",
            "cached",
            "cache_read_tokens",
            "cachedContentTokenCount",
        ],
    );
    let written = number(usage, &["cache_creation_tokens", "cache_write_tokens"]);
    (input + output + cached + written > 0).then_some((input, output, cached, written))
}

fn parse_claude_file(path: &Path, _thread_id: &str) -> anyhow::Result<Vec<UsageRecord>> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("read Claude session {}", path.display()))?;
    let mut records = Vec::new();
    let mut session_id = None;
    let mut selected = std::collections::HashMap::<String, (Value, bool, i64)>::new();
    for line in content.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        session_id = session_id.or_else(|| {
            value
                .get("sessionId")
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
        if value.get("type").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let Some(message) = value.get("message") else {
            continue;
        };
        let Some(message_id) = message.get("id").and_then(Value::as_str) else {
            continue;
        };
        let Some(usage) = message.get("usage") else {
            continue;
        };
        let candidate = (
            value.clone(),
            message.get("stop_reason").is_some(),
            number(usage, &["output_tokens", "output"]),
        );
        let replace = selected
            .get(message_id)
            .is_none_or(|(_, old_stop, old_output)| {
                (candidate.1 && !*old_stop)
                    || (candidate.1 == *old_stop && candidate.2 > *old_output)
            });
        if replace {
            selected.insert(message_id.to_owned(), candidate);
        }
    }
    for (message_id, (value, _, _)) in selected {
        let message = value.get("message").expect("selected assistant message");
        let Some((input, output, cached, written)) = usage(&value) else {
            continue;
        };
        let metadata = metadata::UsageMetadata::default().with_response(message);
        records.push(UsageRecord {
            request_id: format!("session:{message_id}"),
            app_type: "claude".into(),
            provider_id: "_session".into(),
            provider_type: "session_log".into(),
            data_source: "session_log".into(),
            model: string(message, &["model"], "unknown"),
            request_model: string(message, &["model"], "unknown"),
            pricing_model: None,
            service_tier: metadata.service_tier,
            service_tier_source: metadata.service_tier_source,
            reasoning_effort: metadata.reasoning_effort,
            service_tier_pricing_version: Some(2),
            input_tokens: input,
            output_tokens: output,
            cache_read_tokens: cached,
            cache_creation_tokens: written,
            input_token_semantics: 0,
            created_at: timestamp(value.get("timestamp")),
            session_id: session_id.clone(),
            source_path: path.into(),
            is_streaming: true,
            status_code: 200,
            latency_ms: 0,
            reported_total_cost_usd: None,
            identity: None,
        });
    }
    Ok(records)
}

fn import_opencode(
    path: &Path,
    report: &mut ImportReport,
    should_scan: &impl Fn(&Path) -> bool,
) -> anyhow::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    if !should_scan(path) {
        return Ok(());
    }
    report.files_scanned += 1;
    report.scanned_paths.push(path.to_owned());
    let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("open OpenCode database {}", path.display()))?;
    let mut statement = match conn.prepare("SELECT id, session_id, data FROM message") {
        Ok(statement) => statement,
        Err(_) => return Ok(()),
    };
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    for row in rows.flatten() {
        let (id, session, raw) = row;
        let Ok(value) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        if value.get("role").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let Some(tokens) = value.get("tokens") else {
            continue;
        };
        let input = number(tokens, &["input"]);
        if value.pointer("/time/completed").is_none() {
            continue;
        }
        let output = number(tokens, &["output"]) + number(tokens, &["reasoning"]);
        let cached = tokens
            .get("cache")
            .map(|cache| number(cache, &["read"]))
            .unwrap_or(0);
        let written = tokens
            .get("cache")
            .map(|cache| number(cache, &["write"]))
            .unwrap_or(0);
        if input + output + cached + written == 0 {
            continue;
        }
        let model = string(&value, &["modelID", "model_id", "model"], "unknown");
        let created_at = value
            .pointer("/time/created")
            .and_then(Value::as_i64)
            .map(|value| value / 1000)
            .unwrap_or_else(|| timestamp(value.get("timestamp")));
        report.records.push(UsageRecord {
            request_id: format!("opencode_session:{session}:{id}"),
            app_type: "opencode".into(),
            provider_id: "_opencode_session".into(),
            provider_type: "opencode_session".into(),
            data_source: "opencode_session".into(),
            model: model.clone(),
            request_model: model,
            pricing_model: None,
            service_tier: None,
            service_tier_source: None,
            reasoning_effort: None,
            service_tier_pricing_version: None,
            input_tokens: input,
            output_tokens: output,
            cache_read_tokens: cached,
            cache_creation_tokens: written,
            input_token_semantics: 0,
            created_at,
            session_id: Some(session),
            source_path: path.to_owned(),
            is_streaming: true,
            status_code: 200,
            latency_ms: 0,
            reported_total_cost_usd: value
                .get("cost")
                .and_then(Value::as_f64)
                .filter(|cost| *cost > 0.0)
                .map(|cost| cost.to_string()),
            identity: None,
        });
    }
    Ok(())
}

fn import_grok_tree(
    root: &Path,
    report: &mut ImportReport,
    should_scan: &impl Fn(&Path) -> bool,
) -> anyhow::Result<()> {
    let mut files = Vec::new();
    collect_named_files(root, "updates.jsonl", &mut files);
    files.sort();
    for path in files {
        if !should_scan(&path) {
            continue;
        }
        report.files_scanned += 1;
        report.scanned_paths.push(path.to_owned());
        let content = fs::read_to_string(&path)?;
        let session_id = path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|s| s.to_str())
            .unwrap_or("unknown");
        for (index, line) in content.lines().enumerate() {
            let Ok(value) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if value.get("method").and_then(Value::as_str) != Some("_x.ai/session/update") {
                continue;
            };
            let Some(update) = value.pointer("/params/update") else {
                continue;
            };
            if update
                .get("sessionUpdate")
                .and_then(Value::as_str)
                .is_some_and(|kind| kind != "turn_completed")
            {
                continue;
            };
            let Some(usage) = update.get("usage") else {
                continue;
            };
            let Some(created_at) = event_timestamp(value.get("timestamp")) else {
                report.skipped += 1;
                continue;
            };
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_secs() as i64)
                .unwrap_or(created_at);
            if now.saturating_sub(created_at) < 60 {
                report.deferred_paths.push(path.clone());
                continue;
            }
            let prompt_id = string(update, &["prompt_id"], &format!("idx{index}"));
            let per_model = usage.get("modelUsage").and_then(Value::as_object);
            let models: Vec<(String, &Value)> = per_model
                .map(|map| {
                    map.iter()
                        .map(|(model, value)| (model.clone(), value))
                        .collect()
                })
                .unwrap_or_else(|| vec![("unknown".into(), usage)]);
            for (model, counters) in models {
                let input = number(counters, &["inputTokens"]);
                let output = number(counters, &["outputTokens"]);
                let cached = number(counters, &["cachedReadTokens"]);
                if input + output + cached == 0 {
                    continue;
                };
                report.records.push(UsageRecord {
                    request_id: format!("grok_session:{session_id}:{prompt_id}:{model}"),
                    app_type: "grokbuild".into(),
                    provider_id: "_grokbuild_session".into(),
                    provider_type: "grok_session".into(),
                    data_source: "grok_session".into(),
                    model: model.clone(),
                    request_model: model,
                    pricing_model: None,
                    service_tier: None,
                    service_tier_source: None,
                    reasoning_effort: None,
                    service_tier_pricing_version: None,
                    input_tokens: input,
                    output_tokens: output,
                    cache_read_tokens: cached,
                    cache_creation_tokens: 0,
                    input_token_semantics: 1,
                    created_at,
                    session_id: Some(session_id.into()),
                    source_path: path.clone(),
                    is_streaming: true,
                    status_code: 200,
                    latency_ms: number(counters, &["apiDurationMs"]),
                    reported_total_cost_usd: reported_cost(counters),
                    identity: None,
                });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_cumulative_usage_becomes_deltas() {
        let dir = tempfile::tempdir().unwrap();
        let thread = "019c6e27-e55b-73d1-87d8-4e01f1f75043";
        let sessions = dir.path().join("sessions");
        fs::create_dir(&sessions).unwrap();
        let path = sessions.join(format!("rollout-{thread}.jsonl"));
        fs::write(&path, format!(
            "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{thread}\"}}}}\n{{\"type\":\"event_msg\",\"timestamp\":\"2026-07-30T00:00:00Z\",\"payload\":{{\"type\":\"token_count\",\"info\":{{\"total_token_usage\":{{\"input_tokens\":10,\"cached_input_tokens\":2,\"output_tokens\":3}}}}}}}}\n{{\"type\":\"event_msg\",\"timestamp\":\"2026-07-30T00:00:01Z\",\"payload\":{{\"type\":\"token_count\",\"info\":{{\"total_token_usage\":{{\"input_tokens\":15,\"cached_input_tokens\":4,\"output_tokens\":5}}}}}}}}"
        )).unwrap();
        let mut report = ImportReport::default();
        codex::import(dir.path(), &mut report, &|_| true).unwrap();
        let records = report.records;
        assert_eq!(
            records
                .iter()
                .map(|record| record.input_tokens)
                .collect::<Vec<_>>(),
            vec![10, 5]
        );
        assert_eq!(records[1].cache_read_tokens, 2);
    }

    #[test]
    fn codex_prefers_last_usage_over_reset_cumulative_totals() {
        let dir = tempfile::tempdir().unwrap();
        let thread = "019c6e27-e55b-73d1-87d8-4e01f1f75043";
        let sessions = dir.path().join("sessions");
        fs::create_dir(&sessions).unwrap();
        let path = sessions.join(format!("rollout-{thread}.jsonl"));
        fs::write(&path, format!(
            "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{thread}\"}}}}\n{{\"type\":\"event_msg\",\"timestamp\":\"2026-07-30T00:00:00Z\",\"payload\":{{\"type\":\"token_count\",\"info\":{{\"last_token_usage\":{{\"input_tokens\":10,\"cached_input_tokens\":2,\"output_tokens\":3}},\"total_token_usage\":{{\"input_tokens\":5000000,\"cached_input_tokens\":4900000,\"output_tokens\":30000}}}}}}}}\n{{\"type\":\"event_msg\",\"timestamp\":\"2026-07-31T00:00:00Z\",\"payload\":{{\"type\":\"token_count\",\"info\":{{\"last_token_usage\":{{\"input_tokens\":25,\"cached_input_tokens\":4,\"output_tokens\":5}},\"total_token_usage\":{{\"input_tokens\":25,\"cached_input_tokens\":4,\"output_tokens\":5}}}}}}}}"
        )).unwrap();
        let mut report = ImportReport::default();
        codex::import(dir.path(), &mut report, &|_| true).unwrap();
        let records = report.records;
        assert_eq!(
            records
                .iter()
                .map(|record| (
                    record.input_tokens,
                    record.cache_read_tokens,
                    record.output_tokens
                ))
                .collect::<Vec<_>>(),
            vec![(10, 2, 3), (25, 4, 5)]
        );
    }

    #[test]
    fn codex_cumulative_fallback_matches_upstream_high_water() {
        let dir = tempfile::tempdir().unwrap();
        let thread = "019c6e27-e55b-73d1-87d8-4e01f1f75043";
        let sessions = dir.path().join("sessions");
        fs::create_dir(&sessions).unwrap();
        let path = sessions.join(format!("rollout-{thread}.jsonl"));
        fs::write(&path, format!(
            "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{thread}\"}}}}\n{{\"type\":\"event_msg\",\"timestamp\":\"2026-07-30T00:00:00Z\",\"payload\":{{\"type\":\"token_count\",\"info\":{{\"total_token_usage\":{{\"input_tokens\":100,\"cached_input_tokens\":80,\"output_tokens\":10}}}}}}}}\n{{\"type\":\"event_msg\",\"timestamp\":\"2026-07-31T00:00:00Z\",\"payload\":{{\"type\":\"token_count\",\"info\":{{\"total_token_usage\":{{\"input_tokens\":25,\"cached_input_tokens\":4,\"output_tokens\":5}}}}}}}}"
        )).unwrap();
        let mut report = ImportReport::default();
        codex::import(dir.path(), &mut report, &|_| true).unwrap();
        let records = report.records;
        assert_eq!(
            records
                .iter()
                .map(|record| (
                    record.input_tokens,
                    record.cache_read_tokens,
                    record.output_tokens
                ))
                .collect::<Vec<_>>(),
            vec![(100, 80, 10)]
        );
    }

    #[test]
    fn import_all_executes_all_six_vendored_adapters() {
        let temp = tempfile::tempdir().unwrap();
        let claude = temp.path().join("claude");
        let codex = temp.path().join("codex");
        let gemini = temp.path().join("gemini");
        let grok = temp.path().join("grok/session-1");
        let pi = temp.path().join("pi");
        let opencode = temp.path().join("opencode.db");

        fs::create_dir_all(claude.join("projects/project")).unwrap();
        fs::write(
            claude.join("projects/project/session.jsonl"),
            r#"{"type":"assistant","sessionId":"claude-session","timestamp":"2020-01-01T00:00:00Z","message":{"id":"claude-message","model":"claude-model","stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":2}}}
"#,
        )
        .unwrap();

        let thread = "019c6e27-e55b-73d1-87d8-4e01f1f75043";
        let codex_sessions = codex.join("sessions/2020/01/01");
        fs::create_dir_all(&codex_sessions).unwrap();
        fs::write(
            codex_sessions.join(format!("rollout-2020-01-01T00-00-00-{thread}.jsonl")),
            format!(
                "{{\"type\":\"session_meta\",\"timestamp\":\"2020-01-01T00:00:00Z\",\"payload\":{{\"id\":\"{thread}\"}}}}\n{{\"type\":\"turn_context\",\"payload\":{{\"model\":\"gpt-fixture\"}}}}\n{{\"type\":\"event_msg\",\"timestamp\":\"2020-01-01T00:00:01Z\",\"payload\":{{\"type\":\"token_count\",\"info\":{{\"total_token_usage\":{{\"input_tokens\":3,\"output_tokens\":4}}}}}}}}\n"
            ),
        )
        .unwrap();

        let gemini_chats = gemini.join("tmp/project/chats");
        fs::create_dir_all(&gemini_chats).unwrap();
        fs::write(
            gemini_chats.join("session-fixture.json"),
            r#"{"sessionId":"gemini-session","messages":[{"id":"gemini-message","type":"gemini","model":"gemini-model","timestamp":"2020-01-01T00:00:02Z","tokens":{"input":5,"output":6}}]}"#,
        )
        .unwrap();

        let opencode_conn = Connection::open(&opencode).unwrap();
        opencode_conn
            .execute_batch("CREATE TABLE message(id TEXT, session_id TEXT, data TEXT);")
            .unwrap();
        opencode_conn
            .execute(
                "INSERT INTO message VALUES (?1,?2,?3)",
                rusqlite::params![
                    "opencode-message",
                    "opencode-session",
                    r#"{"role":"assistant","modelID":"opencode-model","tokens":{"input":7,"output":8,"cache":{"read":0,"write":0}},"time":{"created":1577836803000,"completed":1577836804000}}"#
                ],
            )
            .unwrap();
        drop(opencode_conn);

        fs::create_dir_all(&grok).unwrap();
        fs::write(
            grok.join("updates.jsonl"),
            r#"{"method":"_x.ai/session/update","timestamp":"2020-01-01T00:00:04Z","params":{"update":{"sessionUpdate":"turn_completed","prompt_id":"grok-prompt","usage":{"modelUsage":{"grok-model":{"inputTokens":9,"outputTokens":10,"cachedReadTokens":0}}}}}}
"#,
        )
        .unwrap();

        fs::create_dir_all(&pi).unwrap();
        fs::write(
            pi.join("session.jsonl"),
            r#"{"type":"session","id":"pi-session","timestamp":"2020-01-01T00:00:05Z"}
{"type":"message","id":"pi-message","timestamp":"2020-01-01T00:00:06Z","message":{"role":"assistant","provider":"pi-provider","model":"pi-model","usage":{"input":11,"output":12,"cacheRead":0,"cacheWrite":0},"stopReason":"stop"}}
"#,
        )
        .unwrap();

        let report = import_all(&SourceConfig {
            claude_dir: claude,
            codex_dir: codex,
            gemini_dir: gemini,
            opencode_db: opencode,
            grok_dir: temp.path().join("grok"),
            pi_dir: pi,
        })
        .unwrap();
        let sources = report
            .records
            .iter()
            .map(|record| record.data_source.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            sources,
            std::collections::BTreeSet::from([
                "session_log",
                "codex_session",
                "gemini_session",
                "opencode_session",
                "grok_session",
                "pi_session",
            ])
        );
        assert_eq!(report.files_scanned, 6);
        assert_eq!(report.scanned_paths.len(), 6);
    }
    #[test]
    fn claude_fast_metadata_uses_the_provider_response() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        fs::write(&path,serde_json::json!({"type":"assistant","timestamp":"2026-09-26T10:00:00Z","message":{"id":"m","model":"claude-opus-5","usage":{"input_tokens":100,"output_tokens":10,"speed":"fast"}}}).to_string()).unwrap();
        let records = parse_claude_file(&path, "thread").unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].service_tier.as_deref(), Some("fast"));
        assert_eq!(records[0].service_tier_source.as_deref(), Some("response"));
    }
}

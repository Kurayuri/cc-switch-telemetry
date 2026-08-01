//! Neutral raw-session importers shared by telemetry and cc-switch adapters.
//!
//! The importers deliberately do not know about Tauri, cc-switch's `Database`,
//! pricing DAO, or the destination schema. They emit stable usage records and
//! leave persistence, deduplication, and pricing to the caller.

use anyhow::Context;
use chrono::DateTime;
use rusqlite::Connection;
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

pub const IMPORTER_REVISION: &str = "cc-switch-ff3bc242:session-usage-v1";

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
    import_codex_files(&config.codex_dir, &mut report, &should_scan)?;
    import_gemini_files(&config.gemini_dir.join("tmp"), &mut report, &should_scan)?;
    import_opencode(&config.opencode_db, &mut report, &should_scan)?;
    import_grok_tree(&config.grok_dir, &mut report, &should_scan)?;
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

fn import_codex_files(
    root: &Path,
    report: &mut ImportReport,
    should_scan: &impl Fn(&Path) -> bool,
) -> anyhow::Result<()> {
    let mut files = Vec::new();
    collect_codex_sessions(&root.join("sessions"), &mut files, 0);
    if let Ok(entries) = fs::read_dir(root.join("archived_sessions")) {
        files.extend(
            entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("jsonl")),
        );
    }
    files.retain(|path| {
        path.file_name()
            .and_then(|value| value.to_str())
            .is_some_and(is_rollout_filename)
    });
    files.sort();
    let mut parsed = Vec::new();
    for path in files {
        let Some(thread_id) = filename_uuid(&path) else {
            continue;
        };
        let records = parse_codex_file(&path, &thread_id)?;
        let info = codex_file_info(&path, &thread_id)?;
        parsed.push((path, thread_id, records, info));
    }
    for index in 0..parsed.len() {
        let (path, thread_id, records, info) = &parsed[index];
        if !should_scan(path) {
            continue;
        }
        report.files_scanned += 1;
        report.scanned_paths.push(path.clone());
        if !info.meta_seen || info.meta_id.as_deref() != Some(thread_id) {
            report.skipped += 1;
            continue;
        }
        let replay_events = if let Some(parent) = info.parent_id.as_deref() {
            let parent_index = parsed.iter().position(|(_, id, _, _)| id == parent);
            let Some(parent_index) = parent_index else {
                report.skipped += 1;
                continue;
            };
            let Some(cutoff) = info.root_timestamp else {
                report.skipped += 1;
                continue;
            };
            let parent_signatures = parsed[parent_index]
                .3
                .events
                .iter()
                .filter(|event| event.timestamp.is_none_or(|timestamp| timestamp <= cutoff))
                .map(|event| event.signature.as_str())
                .collect::<Vec<_>>();
            matching_codex_prefix(&info.events, &parent_signatures)
        } else {
            0
        };
        let skip_records = info
            .events
            .iter()
            .take(replay_events)
            .filter(|event| event.billable)
            .count();
        if skip_records >= records.len() {
            report.skipped += records.len() as u64;
        } else {
            report
                .records
                .extend(records.iter().skip(skip_records).cloned());
        }
    }
    Ok(())
}

#[derive(Debug)]
struct CodexFileInfo {
    meta_seen: bool,
    meta_id: Option<String>,
    parent_id: Option<String>,
    root_timestamp: Option<i64>,
    events: Vec<CodexEventSignature>,
}

#[derive(Debug)]
struct CodexEventSignature {
    signature: String,
    timestamp: Option<i64>,
    billable: bool,
}

fn codex_file_info(path: &Path, _thread_id: &str) -> anyhow::Result<CodexFileInfo> {
    let content = fs::read_to_string(path)?;
    let mut info = CodexFileInfo {
        meta_seen: false,
        meta_id: None,
        parent_id: None,
        root_timestamp: None,
        events: Vec::new(),
    };
    for line in content.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match value.get("type").and_then(Value::as_str) {
            Some("session_meta") if !info.meta_seen => {
                info.meta_seen = true;
                info.root_timestamp = value
                    .get("timestamp")
                    .and_then(|value| event_timestamp(Some(value)));
                let payload = value.get("payload").unwrap_or(&Value::Null);
                info.meta_id = payload
                    .get("id")
                    .or_else(|| payload.get("thread_id"))
                    .or_else(|| payload.get("threadId"))
                    .and_then(Value::as_str)
                    .and_then(|value| uuid::Uuid::parse_str(value).ok())
                    .map(|value| value.hyphenated().to_string());
                let fork = payload.get("forked_from_id").and_then(Value::as_str);
                let spawn = payload
                    .pointer("/source/subagent/thread_spawn/parent_thread_id")
                    .and_then(Value::as_str);
                info.parent_id = match (fork, spawn) {
                    (Some(fork), Some(spawn)) if fork != spawn => None,
                    (Some(value), _) | (_, Some(value)) => uuid::Uuid::parse_str(value)
                        .ok()
                        .map(|value| value.hyphenated().to_string()),
                    _ => None,
                };
            }
            Some("event_msg")
                if value.pointer("/payload/type").and_then(Value::as_str)
                    == Some("token_count") =>
            {
                let Some(counters) = value
                    .pointer("/payload/info/total_token_usage")
                    .or_else(|| value.pointer("/payload/info/last_token_usage"))
                else {
                    continue;
                };
                let input = number(counters, &["input_tokens", "input"]);
                let cached = number(counters, &["cached_input_tokens", "cached_input", "cached"]);
                let output = number(counters, &["output_tokens", "output"]);
                info.events.push(CodexEventSignature {
                    signature: format!("{input}:{cached}:{output}"),
                    timestamp: event_timestamp(value.get("timestamp")),
                    billable: input + cached + output > 0,
                });
            }
            _ => {}
        }
    }
    Ok(info)
}

fn matching_codex_prefix(child: &[CodexEventSignature], parent: &[&str]) -> usize {
    let mut parent_index = 0;
    let mut matched = 0;
    for event in child {
        let Some(relative) = parent[parent_index..]
            .iter()
            .position(|signature| *signature == event.signature)
        else {
            break;
        };
        parent_index += relative + 1;
        matched += 1;
    }
    matched
}

fn collect_codex_sessions(root: &Path, files: &mut Vec<PathBuf>, depth: usize) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && depth < 3 {
            collect_codex_sessions(&path, files, depth + 1);
        } else if path.extension().and_then(|value| value.to_str()) == Some("jsonl") {
            files.push(path);
        }
    }
}

fn is_rollout_filename(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".jsonl") else {
        return false;
    };
    stem.starts_with("rollout-") && filename_uuid_from_stem(stem).is_some()
}

fn filename_uuid(path: &Path) -> Option<String> {
    filename_uuid_from_stem(path.file_stem()?.to_str()?)
}

fn filename_uuid_from_stem(stem: &str) -> Option<String> {
    let start = stem.len().checked_sub(36)?;
    let suffix = stem.get(start..)?;
    uuid::Uuid::parse_str(suffix)
        .ok()
        .map(|id| id.hyphenated().to_string())
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
        records.push(UsageRecord {
            request_id: format!("session:{message_id}"),
            app_type: "claude".into(),
            provider_id: "_session".into(),
            provider_type: "session_log".into(),
            data_source: "session_log".into(),
            model: string(message, &["model"], "unknown"),
            request_model: string(message, &["model"], "unknown"),
            pricing_model: None,
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
        });
    }
    Ok(records)
}

#[derive(Default, Clone, Copy)]
struct Counters {
    input: i64,
    cached: i64,
    output: i64,
}

fn parse_codex_file(path: &Path, thread_id: &str) -> anyhow::Result<Vec<UsageRecord>> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("read Codex session {}", path.display()))?;
    let mut model = "unknown".to_owned();
    let mut previous = Counters::default();
    let mut has_previous = false;
    let mut index = 0_u64;
    let mut records = Vec::new();
    let mut session_meta_seen = false;
    for line in content.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match value.get("type").and_then(Value::as_str) {
            Some("session_meta") if !session_meta_seen => {
                session_meta_seen = true;
                let payload = value.get("payload").unwrap_or(&Value::Null);
                if payload
                    .get("id")
                    .or_else(|| payload.get("thread_id"))
                    .or_else(|| payload.get("threadId"))
                    .and_then(Value::as_str)
                    .and_then(|id| uuid::Uuid::parse_str(id).ok())
                    .is_none_or(|id| id.hyphenated().to_string() != thread_id)
                {
                    return Ok(Vec::new());
                }
            }
            Some("turn_context") => {
                model = string(
                    value.get("payload").unwrap_or(&Value::Null),
                    &["model"],
                    &model,
                );
            }
            Some("event_msg")
                if value.pointer("/payload/type").and_then(Value::as_str)
                    == Some("token_count") =>
            {
                let Some(info) = value.pointer("/payload/info") else {
                    continue;
                };
                if let Some(next) = info
                    .get("model")
                    .or_else(|| info.get("model_name"))
                    .and_then(Value::as_str)
                {
                    model = normalize_codex_model(next);
                }
                let (current, cumulative) = if let Some(total) = info.get("total_token_usage") {
                    (total, true)
                } else if let Some(last) = info.get("last_token_usage") {
                    (last, false)
                } else {
                    continue;
                };
                let counters = Counters {
                    input: number(current, &["input_tokens", "input"]),
                    cached: number(current, &["cached_input_tokens", "cached_input", "cached"]),
                    output: number(current, &["output_tokens", "output"]),
                };
                let delta = if cumulative {
                    let value = if has_previous {
                        Counters {
                            input: counters.input.saturating_sub(previous.input),
                            cached: counters.cached.saturating_sub(previous.cached),
                            output: counters.output.saturating_sub(previous.output),
                        }
                    } else {
                        counters
                    };
                    previous = counters;
                    has_previous = true;
                    value
                } else {
                    counters
                };
                if delta.input + delta.cached + delta.output == 0 {
                    continue;
                }
                index += 1;
                records.push(UsageRecord {
                    request_id: format!("codex_session:thread-v1:{thread_id}:{index}"),
                    app_type: "codex".into(),
                    provider_id: "_codex_session".into(),
                    provider_type: "codex_session".into(),
                    data_source: "codex_session".into(),
                    model: model.clone(),
                    request_model: model.clone(),
                    pricing_model: None,
                    input_tokens: delta.input,
                    output_tokens: delta.output,
                    cache_read_tokens: delta.cached.min(delta.input),
                    cache_creation_tokens: 0,
                    input_token_semantics: 0,
                    created_at: timestamp(value.get("timestamp")),
                    session_id: Some(thread_id.to_owned()),
                    source_path: path.into(),
                    is_streaming: true,
                    status_code: 200,
                    latency_ms: 0,
                    reported_total_cost_usd: None,
                });
            }
            _ => {}
        }
    }
    if !session_meta_seen {
        return Ok(Vec::new());
    }
    Ok(records)
}

fn normalize_codex_model(raw: &str) -> String {
    let mut model = raw.to_ascii_lowercase();
    if let Some(index) = model.rfind('/') {
        model = model[index + 1..].to_owned();
    }
    if model.len() >= 11 {
        let suffix = &model[model.len() - 11..];
        if suffix.as_bytes().first() == Some(&b'-')
            && suffix[1..5].chars().all(|c| c.is_ascii_digit())
            && suffix.as_bytes().get(5) == Some(&b'-')
            && suffix[6..8].chars().all(|c| c.is_ascii_digit())
            && suffix.as_bytes().get(8) == Some(&b'-')
            && suffix[9..11].chars().all(|c| c.is_ascii_digit())
        {
            model.truncate(model.len() - 11);
        }
    }
    if model.len() >= 9 {
        let (base, suffix) = model.split_at(model.len() - 8);
        if base.ends_with('-') && suffix.chars().all(|c| c.is_ascii_digit()) {
            model.truncate(model.len() - 9);
        }
    }
    model
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
        let path = dir.path().join("rollout.jsonl");
        let thread = "019c6e27-e55b-73d1-87d8-4e01f1f75043";
        fs::write(&path, format!(
            "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{thread}\"}}}}\n{{\"type\":\"event_msg\",\"timestamp\":\"2026-07-30T00:00:00Z\",\"payload\":{{\"type\":\"token_count\",\"info\":{{\"total_token_usage\":{{\"input_tokens\":10,\"cached_input_tokens\":2,\"output_tokens\":3}}}}}}}}\n{{\"type\":\"event_msg\",\"timestamp\":\"2026-07-30T00:00:01Z\",\"payload\":{{\"type\":\"token_count\",\"info\":{{\"total_token_usage\":{{\"input_tokens\":15,\"cached_input_tokens\":4,\"output_tokens\":5}}}}}}}}"
        )).unwrap();
        let records = parse_codex_file(&path, thread).unwrap();
        assert_eq!(
            records
                .iter()
                .map(|record| record.input_tokens)
                .collect::<Vec<_>>(),
            vec![10, 5]
        );
        assert_eq!(records[1].cache_read_tokens, 2);
    }
}

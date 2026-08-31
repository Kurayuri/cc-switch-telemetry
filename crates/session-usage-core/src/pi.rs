//! Tauri-free Pi session adapter derived from the vendored cc-switch parser.

use crate::{ImportReport, UsageIdentity, UsageRecord};
use chrono::DateTime;
use rust_decimal::Decimal;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    str::FromStr,
    time::UNIX_EPOCH,
};

const MAX_SESSION_BYTES: u64 = 128 * 1024 * 1024;
const MAX_TREE_ID_BYTES: usize = 256;
const MAX_USAGE_LABEL_BYTES: usize = 512;
const MIN_SQLITE_UNIX_MILLIS: i64 = -62_167_219_200_000;
const MAX_SQLITE_UNIX_MILLIS: i64 = 253_402_300_799_999;
const PROVIDER_PLACEHOLDER: &str = "_pi_session";
const UNKNOWN_MODEL: &str = "unknown";

#[derive(Debug)]
struct PiRequestIdentity {
    request_id: String,
    semantic_id: String,
    has_entry_id: bool,
}

pub(super) fn import_pi_files(
    root: &Path,
    report: &mut ImportReport,
    should_scan: &impl Fn(&Path) -> bool,
) -> anyhow::Result<()> {
    let mut files = Vec::new();
    collect_jsonl_files(root, &mut files, 0);
    files.sort();
    for path in files {
        if !should_scan(&path) {
            continue;
        }
        report.files_scanned = report.files_scanned.saturating_add(1);
        report.scanned_paths.push(path.clone());
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.file_type().is_file() || metadata.len() > MAX_SESSION_BYTES {
            report.skipped = report.skipped.saturating_add(1);
            continue;
        }
        parse_pi_file(&path, report)?;
    }
    Ok(())
}

fn collect_jsonl_files(root: &Path, files: &mut Vec<PathBuf>, depth: usize) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_file() && path.extension().and_then(|value| value.to_str()) == Some("jsonl")
        {
            files.push(path);
        } else if file_type.is_dir() && depth == 0 {
            collect_jsonl_files(&path, files, depth + 1);
        }
    }
}

fn parse_pi_file(path: &Path, report: &mut ImportReport) -> anyhow::Result<()> {
    let content = fs::read_to_string(path)?;
    let file_timestamp = fs::metadata(path)
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|timestamp| timestamp.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or(0);
    let mut session_id = None;
    let mut session_timestamp = None;

    for raw_line in content.split_inclusive('\n') {
        let has_newline = raw_line.ends_with('\n');
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let value = match serde_json::from_str::<Value>(line) {
            Ok(value) => value,
            Err(_) if !has_newline => {
                report.deferred_paths.push(path.to_owned());
                break;
            }
            Err(_) => continue,
        };

        if session_id.is_none() {
            if value.get("type").and_then(Value::as_str) != Some("session") {
                anyhow::bail!(
                    "Pi session {} first valid JSON is not a session header",
                    path.display()
                );
            }
            let id = value
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| is_valid_tree_id(id))
                .ok_or_else(|| anyhow::anyhow!("Pi session {} has no valid id", path.display()))?;
            session_id = Some(id.to_owned());
            session_timestamp = value
                .get("timestamp")
                .and_then(parse_timestamp_millis)
                .map(|timestamp| timestamp / 1000);
            continue;
        }

        if let Some(record) = parse_usage_record(
            &value,
            session_id.as_deref().unwrap_or_default(),
            session_timestamp,
            file_timestamp,
            path,
        ) {
            report.records.push(record);
        }
    }

    if session_id.is_none() && !report.deferred_paths.iter().any(|item| item == path) {
        anyhow::bail!("Pi session {} has no valid header", path.display());
    }
    Ok(())
}

fn parse_usage_record(
    entry: &Value,
    session_id: &str,
    session_timestamp: Option<i64>,
    file_timestamp: i64,
    source_path: &Path,
) -> Option<UsageRecord> {
    let entry_type = entry.get("type").and_then(Value::as_str)?;
    let (kind, usage, message) = match entry_type {
        "message" => {
            let message = entry.get("message")?;
            match message.get("role").and_then(Value::as_str) {
                Some("assistant") => ("assistant", message.get("usage")?, Some(message)),
                Some("toolResult") => ("tool_result", message.get("usage")?, Some(message)),
                _ => return None,
            }
        }
        "compaction" => ("compaction", entry.get("usage")?, None),
        "branch_summary" => ("branch_summary", entry.get("usage")?, None),
        _ => return None,
    };

    let input_tokens = token_count(usage, "input");
    let output_tokens = token_count(usage, "output");
    let cache_read_tokens = token_count(usage, "cacheRead");
    let cache_write_tokens = token_count(usage, "cacheWrite");
    let reported_total_cost_usd = reported_cost(usage.get("cost"));
    let stop_reason = (kind == "assistant")
        .then(|| message.and_then(|value| nonempty_string(value.get("stopReason"))))
        .flatten();
    let failed = matches!(stop_reason, Some("error" | "aborted"));
    if input_tokens == 0
        && output_tokens == 0
        && cache_read_tokens == 0
        && cache_write_tokens == 0
        && reported_total_cost_usd.is_none()
        && !failed
    {
        return None;
    }

    let (provider_id, model, request_model) = if kind == "assistant" {
        let message = message?;
        let provider = bounded_label(message.get("provider"), PROVIDER_PLACEHOLDER);
        let requested = bounded_label(message.get("model"), UNKNOWN_MODEL);
        let actual = nonempty_string(message.get("responseModel"))
            .map(truncate_usage_label)
            .unwrap_or(&requested)
            .to_owned();
        (provider, actual, requested)
    } else {
        (
            PROVIDER_PLACEHOLDER.to_owned(),
            UNKNOWN_MODEL.to_owned(),
            UNKNOWN_MODEL.to_owned(),
        )
    };
    let created_at = entry
        .get("timestamp")
        .and_then(parse_timestamp_millis)
        .or_else(|| {
            message
                .and_then(|value| value.get("timestamp"))
                .and_then(parse_timestamp_millis)
        })
        .map(|timestamp| timestamp / 1000)
        .or(session_timestamp)
        .unwrap_or(file_timestamp)
        .clamp(MIN_SQLITE_UNIX_MILLIS / 1000, MAX_SQLITE_UNIX_MILLIS / 1000);
    let status_code = match stop_reason {
        Some("aborted") => 499,
        Some("error") => 500,
        _ => 200,
    };
    let identity = pi_request_identity(entry, kind, usage, message);

    Some(UsageRecord {
        request_id: identity.request_id,
        app_type: "pi".to_owned(),
        provider_id,
        provider_type: "pi_session".to_owned(),
        data_source: "pi_session".to_owned(),
        model: model.clone(),
        request_model,
        pricing_model: Some(model),
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_creation_tokens: cache_write_tokens,
        input_token_semantics: 0,
        created_at,
        session_id: Some(session_id.to_owned()),
        source_path: source_path.to_owned(),
        is_streaming: true,
        status_code,
        latency_ms: 0,
        reported_total_cost_usd,
        identity: Some(UsageIdentity {
            semantic_id: identity.semantic_id,
            has_entry_id: identity.has_entry_id,
        }),
    })
}

fn is_valid_tree_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= MAX_TREE_ID_BYTES
        && bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn token_count(usage: &Value, key: &str) -> i64 {
    usage
        .get(key)
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(i64::MAX as u64) as i64
}

fn nonempty_string(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn bounded_label(value: Option<&Value>, fallback: &str) -> String {
    truncate_usage_label(nonempty_string(value).unwrap_or(fallback)).to_owned()
}

fn truncate_usage_label(value: &str) -> &str {
    if value.len() <= MAX_USAGE_LABEL_BYTES {
        return value;
    }
    let mut end = MAX_USAGE_LABEL_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

fn reported_cost(value: Option<&Value>) -> Option<String> {
    let decimal = |key| {
        value
            .and_then(|cost| cost.get(key))
            .and_then(parse_decimal)
            .unwrap_or(Decimal::ZERO)
            .max(Decimal::ZERO)
    };
    let components =
        decimal("input") + decimal("output") + decimal("cacheRead") + decimal("cacheWrite");
    let total = decimal("total");
    let effective = if total > Decimal::ZERO {
        total
    } else {
        components
    };
    (effective > Decimal::ZERO).then(|| effective.to_string())
}

fn parse_decimal(value: &Value) -> Option<Decimal> {
    let raw = match value {
        Value::Number(number) => number.to_string(),
        Value::String(value) => value.clone(),
        _ => return None,
    };
    Decimal::from_str(&raw)
        .or_else(|_| Decimal::from_scientific(&raw))
        .ok()
}

fn parse_timestamp_millis(value: &Value) -> Option<i64> {
    let timestamp = if let Some(timestamp) = value.as_i64() {
        if !(-100_000_000_000..=100_000_000_000).contains(&timestamp) {
            timestamp
        } else {
            timestamp.saturating_mul(1000)
        }
    } else {
        value
            .as_str()
            .and_then(|timestamp| DateTime::parse_from_rfc3339(timestamp).ok())?
            .timestamp_millis()
    };
    (MIN_SQLITE_UNIX_MILLIS..=MAX_SQLITE_UNIX_MILLIS)
        .contains(&timestamp)
        .then_some(timestamp)
}

fn pi_request_identity(
    entry: &Value,
    kind: &str,
    usage: &Value,
    message: Option<&Value>,
) -> PiRequestIdentity {
    let mut hasher = Sha256::new();
    hash_field(&mut hasher, b"pi-session-semantic-v1");
    hash_field(&mut hasher, kind.as_bytes());
    for (label, value) in [
        (b"entry_timestamp".as_slice(), entry.get("timestamp")),
        (
            b"message_timestamp".as_slice(),
            message.and_then(|value| value.get("timestamp")),
        ),
    ] {
        if let Some(value) = value {
            hash_field(&mut hasher, label);
            hash_json(&mut hasher, value);
        }
    }
    if let Some(message) = message {
        for key in [
            "provider",
            "model",
            "responseModel",
            "responseId",
            "api",
            "toolCallId",
            "toolName",
            "stopReason",
            "errorMessage",
        ] {
            if let Some(value) = message.get(key) {
                hash_field(&mut hasher, key.as_bytes());
                hash_json(&mut hasher, value);
            }
        }
        if let Some(content) = message.get("content") {
            hash_field(&mut hasher, b"content");
            hash_json(&mut hasher, content);
        }
    } else if let Some(summary) = entry.get("summary") {
        hash_field(&mut hasher, b"summary");
        hash_json(&mut hasher, summary);
    }
    hash_field(&mut hasher, b"usage");
    hash_json(&mut hasher, usage);
    let semantic_id = format!("pi_session_semantic:{:x}", hasher.finalize());
    let entry_id = nonempty_string(entry.get("id"));
    let request_id = if let Some(entry_id) = entry_id {
        let mut request_hasher = Sha256::new();
        hash_field(&mut request_hasher, b"pi-session-request-v3");
        hash_field(&mut request_hasher, kind.as_bytes());
        hash_field(&mut request_hasher, entry_id.as_bytes());
        if let Some(timestamp) = entry.get("timestamp") {
            hash_json(&mut request_hasher, timestamp);
        }
        format!("pi_session:{:x}", request_hasher.finalize())
    } else {
        semantic_id.clone()
    };
    PiRequestIdentity {
        request_id,
        semantic_id,
        has_entry_id: entry_id.is_some(),
    }
}

fn hash_json(hasher: &mut Sha256, value: &Value) {
    match value {
        Value::Null => hash_field(hasher, b"null"),
        Value::Bool(value) => {
            hash_field(hasher, b"bool");
            hash_field(hasher, if *value { b"true" } else { b"false" });
        }
        Value::Number(value) => {
            hash_field(hasher, b"number");
            hash_field(hasher, value.to_string().as_bytes());
        }
        Value::String(value) => {
            hash_field(hasher, b"string");
            hash_field(hasher, value.as_bytes());
        }
        Value::Array(values) => {
            hash_field(hasher, b"array");
            hash_field(hasher, &(values.len() as u64).to_be_bytes());
            for value in values {
                hash_json(hasher, value);
            }
        }
        Value::Object(values) => {
            hash_field(hasher, b"object");
            hash_field(hasher, &(values.len() as u64).to_be_bytes());
            let mut keys: Vec<_> = values.keys().collect();
            keys.sort_unstable();
            for key in keys {
                hash_field(hasher, key.as_bytes());
                hash_json(hasher, &values[key]);
            }
        }
    }
}

fn hash_field(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn assistant(id: &str, stop_reason: &str) -> String {
        format!(
            r#"{{"type":"message","id":"{id}","timestamp":"2026-08-30T01:00:01Z","message":{{"role":"assistant","provider":"fixture-provider","model":"requested-model","responseModel":"actual-model","content":[{{"type":"text","text":"ok"}}],"usage":{{"input":10,"output":2,"cacheRead":5,"cacheWrite":1,"cost":{{"total":"0.25"}}}},"stopReason":"{stop_reason}"}}}}"#
        )
    }

    #[test]
    fn imports_assistant_tool_and_summary_usage() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("sessions");
        let project = root.join("project");
        fs::create_dir_all(&project).unwrap();
        let path = project.join("session.jsonl");
        let mut file = fs::File::create(&path).unwrap();
        writeln!(
            file,
            r#"{{"type":"session","id":"session-1","timestamp":"2026-08-30T01:00:00Z"}}"#
        )
        .unwrap();
        writeln!(file, "{}", assistant("entry-1", "stop")).unwrap();
        writeln!(file, r#"{{"type":"message","id":"tool-1","timestamp":"2026-08-30T01:00:02Z","message":{{"role":"toolResult","toolCallId":"call-1","usage":{{"input":1,"output":0,"cacheRead":0,"cacheWrite":0}}}}}}"#).unwrap();
        writeln!(file, r#"{{"type":"compaction","id":"compact-1","timestamp":"2026-08-30T01:00:03Z","usage":{{"input":3,"output":1,"cacheRead":0,"cacheWrite":0}}}}"#).unwrap();
        let mut report = ImportReport::default();
        import_pi_files(&root, &mut report, &|_| true).unwrap();
        assert_eq!(report.records.len(), 3);
        assert_eq!(report.records[0].data_source, "pi_session");
        assert_eq!(report.records[0].model, "actual-model");
        assert_eq!(
            report.records[0].reported_total_cost_usd.as_deref(),
            Some("0.25")
        );
        assert!(report
            .records
            .iter()
            .all(|record| record.identity.is_some()));
    }

    #[test]
    fn maps_aborted_status_and_defers_partial_json_tail() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("session.jsonl");
        fs::write(
            &path,
            format!(
                "{{\"type\":\"session\",\"id\":\"session-1\"}}\n{}\n{{\"type\":\"message\"",
                assistant("entry-1", "aborted")
            ),
        )
        .unwrap();
        let mut report = ImportReport::default();
        import_pi_files(temp.path(), &mut report, &|_| true).unwrap();
        assert_eq!(report.records.len(), 1);
        assert_eq!(report.records[0].status_code, 499);
        assert_eq!(report.deferred_paths, vec![path]);
    }
}

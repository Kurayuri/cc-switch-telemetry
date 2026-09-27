//! Codex parsing and paginated-parent reconciliation adapted from cc-switch 87d966b7.
//! Persistence belongs to the caller; deferred files never advance a durable cursor.
use super::{ImportReport, UsageRecord};
use crate::metadata::UsageMetadata;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::{
    collections::HashMap,
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};
const CODEX_JSONL_BUFFER_CAPACITY: usize = 256 * 1024;
fn metadata_modified_nanos(metadata: &fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}
/// 累计 token 用量（跟踪 total_token_usage 字段）
#[derive(Debug, Clone, Default)]
struct CumulativeTokens {
    input: u64,
    cached_input: u64,
    output: u64,
}

/// 单次 API 调用的 token 增量
#[derive(Debug)]
struct DeltaTokens {
    input: u32,
    cached_input: u32,
    output: u32,
}

impl DeltaTokens {
    fn is_zero(&self) -> bool {
        self.input == 0 && self.cached_input == 0 && self.output == 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct TokenCountersSignature {
    input: Option<u64>,
    cached_input: Option<u64>,
    output: Option<u64>,
    reasoning_output: Option<u64>,
    total: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct TokenUsageSignature {
    total: Option<TokenCountersSignature>,
    last: Option<TokenCountersSignature>,
}

#[derive(Debug, Clone)]
struct TimestampedTokenSignature {
    timestamp: DateTime<Utc>,
    signature: TokenUsageSignature,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct ParentFileStamp {
    modified_nanos: i64,
    size: u64,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ParentDependency {
    path: String,
    stamp: Option<ParentFileStamp>,
}

impl ParentDependency {
    fn snapshot(path: &Path) -> Self {
        let stamp = fs::File::open(path)
            .ok()
            .and_then(|file| ParentFileStamp::from_file(&file));
        Self {
            path: path.to_string_lossy().to_string(),
            stamp,
        }
    }
}

impl ParentFileStamp {
    fn from_file(file: &fs::File) -> Option<Self> {
        let metadata = file.metadata().ok()?;
        Some(Self {
            modified_nanos: metadata_modified_nanos(&metadata),
            size: metadata.len(),
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
            #[cfg(windows)]
            volume_serial,
            #[cfg(windows)]
            file_id,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ParentLifecycleEvent {
    timestamp: Option<DateTime<Utc>>,
    terminal: bool,
}

#[derive(Debug, Default)]
struct ParentTokenTimeline {
    lifecycle_events: Vec<ParentLifecycleEvent>,
    events: Vec<TimestampedTokenSignature>,
    max_finalized_timestamp: Option<DateTime<Utc>>,
    ends_at_finalized_turn_boundary: bool,
    unfinalized_activity_min_timestamp: Option<DateTime<Utc>>,
    has_unfinalized_activity_without_timestamp: bool,
    has_token_without_timestamp: bool,
}

impl ParentTokenTimeline {
    fn record_lifecycle(&mut self, event: ParentLifecycleEvent) {
        self.lifecycle_events.push(event);
        if event.terminal {
            self.ends_at_finalized_turn_boundary = event.timestamp.is_some();
            if let Some(timestamp) = event.timestamp {
                self.max_finalized_timestamp = Some(
                    self.max_finalized_timestamp
                        .map_or(timestamp, |current| current.max(timestamp)),
                );
                self.unfinalized_activity_min_timestamp = None;
                self.has_unfinalized_activity_without_timestamp = false;
            } else {
                self.unfinalized_activity_min_timestamp = None;
                self.has_unfinalized_activity_without_timestamp = true;
            }
        } else {
            self.ends_at_finalized_turn_boundary = false;
            if let Some(timestamp) = event.timestamp {
                self.unfinalized_activity_min_timestamp = Some(
                    self.unfinalized_activity_min_timestamp
                        .map_or(timestamp, |current| current.min(timestamp)),
                );
            } else {
                self.has_unfinalized_activity_without_timestamp = true;
            }
        }
    }

    fn signatures_before(
        &self,
        parent_path: &Path,
        cutoff: DateTime<Utc>,
        parent_is_archived: bool,
    ) -> Result<Vec<TokenUsageSignature>, ParentTimelineError> {
        if self.has_token_without_timestamp {
            return Err(ParentTimelineError::Invalid(format!(
                "父 rollout {} 的 token_count 缺少有效 timestamp",
                parent_path.display()
            )));
        }
        let unfinalized_activity_is_after_cutoff = !self.has_unfinalized_activity_without_timestamp
            && self
                .unfinalized_activity_min_timestamp
                .is_none_or(|timestamp| timestamp > cutoff);
        let finalized_through_cutoff = self
            .max_finalized_timestamp
            .is_some_and(|timestamp| timestamp >= cutoff)
            && unfinalized_activity_is_after_cutoff;
        // A completed/aborted turn at or after the cutoff seals the parent
        // prefix even if a newer turn is now active. A file that currently
        // ends at a terminal boundary is also safe when the parent completed
        // before the child was created and stayed idle in `sessions`.
        if !parent_is_archived && !finalized_through_cutoff && !self.ends_at_finalized_turn_boundary
        {
            return Err(ParentTimelineError::BehindCutoff(format!(
                "父 rollout {} 尚无覆盖 child fork 时刻的已完成 turn",
                parent_path.display()
            )));
        }
        Ok(self.signatures_at(cutoff))
    }

    fn signatures_at(&self, cutoff: DateTime<Utc>) -> Vec<TokenUsageSignature> {
        self.events
            .iter()
            .filter(|event| event.timestamp <= cutoff)
            .map(|event| event.signature.clone())
            .collect()
    }
}

#[derive(Debug, PartialEq, Eq)]
enum ParentTimelineError {
    BehindCutoff(String),
    Invalid(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParentResolveFailureKind {
    BehindCutoff,
    Retryable,
}

#[derive(Debug)]
struct ParentResolveFailure {
    reason: String,
}

#[derive(Debug)]
struct CachedParentTimeline {
    stamp: ParentFileStamp,
    timeline: Arc<ParentTokenTimeline>,
}

#[derive(Debug)]
struct ParsedTokenEvent {
    metadata: UsageMetadata,
    signature: TokenUsageSignature,
    delta: DeltaTokens,
    event_index: Option<u32>,
    model: String,
    timestamp: Option<String>,
}

#[derive(Debug)]
enum ParentResolution {
    None,
    Parent(String),
    Deferred(String),
}

#[derive(Debug)]
struct ParsedCodexFile {
    root_thread_id: Option<String>,
    /// root `session_meta` 的线程 ID（稳定逻辑线程）：单段文件名与文件名
    /// UUID 一致，revert/resume 的双段文件名对应前置 UUID。
    meta_thread_id: Option<String>,
    root_meta_seen: bool,
    root_timestamp: Option<DateTime<Utc>>,
    parent: ParentResolution,
    token_events: Vec<ParsedTokenEvent>,
    has_billable_tokens: bool,
    incomplete_tail: bool,
}

#[derive(Default)]
struct CodexReplayCaches {
    parent_timelines: HashMap<PathBuf, CachedParentTimeline>,
}
static CACHES: OnceLock<Mutex<CodexReplayCaches>> = OnceLock::new();
fn replay_caches() -> &'static Mutex<CodexReplayCaches> {
    CACHES.get_or_init(|| Mutex::new(CodexReplayCaches::default()))
}
fn snapshot_parent_dependencies(
    parent_id: &str,
    rollout_index: &RolloutIndex,
) -> Vec<ParentDependency> {
    rollout_index
        .get(parent_id)
        .into_iter()
        .flatten()
        .map(|path| ParentDependency::snapshot(path))
        .collect()
}

fn is_rollout_filename(file_name: &str) -> bool {
    if !file_name.starts_with("rollout-") || !file_name.ends_with(".jsonl") {
        return false;
    }
    let stem = file_name.trim_end_matches(".jsonl");
    stem.get(stem.len().saturating_sub(36)..)
        .is_some_and(|candidate| uuid::Uuid::parse_str(candidate).is_ok())
}

fn is_archived_rollout_path(file_path: &Path) -> bool {
    file_path
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("archived_sessions"))
}

fn non_empty_string(value: Option<&serde_json::Value>) -> Option<String> {
    value
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn thread_id_from_filename(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    let candidate = stem.get(stem.len().checked_sub(36)?..)?;
    uuid::Uuid::parse_str(candidate)
        .ok()
        .map(|value| value.hyphenated().to_string())
}

/// 双段文件名（`rollout-…-<threadId>_<rolloutId>.jsonl`）里下划线前的线程
/// 本体 UUID；单段文件名返回 `None`。
///
/// `thread/revert` 为同一线程新建替换 rollout 时产生这种文件名（见
/// openai/codex#38127）：末段是新生成的 rollout ID，其后的 resume 继续
/// 向该文件追加。root meta 的 `id` 始终是原线程 ID，一致性校验需同时
/// 接受两个 UUID。
fn leading_thread_id_from_filename(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    let len = stem.len();
    // 布局尾部：…<uuidA>_<uuidB>。uuidB 占 36 字符，其前是 '_'（共 37）
    if !stem.get(len.checked_sub(37)?..)?.starts_with('_') {
        return None;
    }
    let candidate = stem.get(len.checked_sub(73)?..len.checked_sub(37)?)?;
    uuid::Uuid::parse_str(candidate)
        .ok()
        .map(|value| value.hyphenated().to_string())
}

fn explicit_parent_from_meta(payload: &serde_json::Value) -> ParentResolution {
    let forked_from = non_empty_string(payload.get("forked_from_id"));
    let spawned_from = payload
        .get("source")
        .and_then(|source| source.get("subagent"))
        .and_then(|subagent| subagent.get("thread_spawn"))
        .and_then(|spawn| non_empty_string(spawn.get("parent_thread_id")));

    match (forked_from, spawned_from) {
        (None, None) => ParentResolution::None,
        (Some(parent), None) | (None, Some(parent)) => ParentResolution::Parent(parent),
        (Some(forked), Some(spawned)) if forked == spawned => ParentResolution::Parent(forked),
        (Some(forked), Some(spawned)) => ParentResolution::Deferred(format!(
            "forked_from_id ({forked}) 与 thread_spawn.parent_thread_id ({spawned}) 不一致"
        )),
    }
}

fn parse_timestamp(value: Option<&serde_json::Value>) -> Option<DateTime<Utc>> {
    value
        .and_then(serde_json::Value::as_str)
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.with_timezone(&Utc))
}

fn parse_signature_counters(value: Option<&serde_json::Value>) -> Option<TokenCountersSignature> {
    let value = value?.as_object()?;
    Some(TokenCountersSignature {
        input: value
            .get("input_tokens")
            .and_then(serde_json::Value::as_u64),
        cached_input: value
            .get("cached_input_tokens")
            .or_else(|| value.get("cache_read_input_tokens"))
            .and_then(serde_json::Value::as_u64),
        output: value
            .get("output_tokens")
            .and_then(serde_json::Value::as_u64),
        reasoning_output: value
            .get("reasoning_output_tokens")
            .and_then(serde_json::Value::as_u64),
        total: value
            .get("total_tokens")
            .and_then(serde_json::Value::as_u64),
    })
}

fn parse_token_signature(info: &serde_json::Value) -> Option<TokenUsageSignature> {
    let total = parse_signature_counters(info.get("total_token_usage"));
    let last = parse_signature_counters(info.get("last_token_usage"));
    (total.is_some() || last.is_some()).then_some(TokenUsageSignature { total, last })
}

fn token_snapshot_source(payload: &serde_json::Value) -> Option<String> {
    payload
        .get("rate_limits")
        .and_then(|rate_limits| rate_limits.get("limit_id"))
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// 单个同步 pass 的共享状态。
///
/// - `cursors`：pass 开始时一次性预载的 `session_log_sync` 快照，替代逐文件
///   SELECT（尤其是 archived 继承的 `substr` 后缀匹配无法走索引，逐文件跑等于
///   每 pass 全表扫 N 次）。快照语义：同 pass 内其他文件刚写入的游标对后续
///   archived 继承不可见——影响仅是多一轮由 request_id 去重兜底的重扫，
///   不丢数据、不双算。
/// - `pricing`：模型定价 pass 级缓存。定价表在 pass 进行中被修改时本 pass
///   仍用旧价，下一个同步 pass 生效。
fn normalize_codex_model(raw: &str) -> String {
    // Step 1: 小写
    let mut name = raw.to_lowercase();

    // Step 2: 剥离 "provider/" 前缀（如 openai/, azure/）
    if let Some(pos) = name.rfind('/') {
        name = name[pos + 1..].to_string();
    }

    // Step 3: 剥离 ISO 日期后缀 -YYYY-MM-DD（正好 11 字符）
    if name.len() > 11 && name.is_char_boundary(name.len() - 11) {
        let suffix = &name[name.len() - 11..];
        if suffix.is_ascii()
            && suffix.as_bytes()[0] == b'-'
            && suffix[1..5].chars().all(|c| c.is_ascii_digit())
            && suffix.as_bytes()[5] == b'-'
            && suffix[6..8].chars().all(|c| c.is_ascii_digit())
            && suffix.as_bytes()[8] == b'-'
            && suffix[9..11].chars().all(|c| c.is_ascii_digit())
        {
            name.truncate(name.len() - 11);
        }
    }

    // Step 4: 剥离紧凑日期后缀 -YYYYMMDD（正好 9 字符）
    if name.len() > 9 {
        let parts: Vec<&str> = name.rsplitn(2, '-').collect();
        if parts.len() == 2 {
            if let Some(suffix) = parts.first() {
                if suffix.len() == 8 && suffix.chars().all(|c| c.is_ascii_digit()) {
                    name = parts[1].to_string();
                }
            }
        }
    }

    name
}

/// 计算两次累计值之间的 delta
fn compute_delta(prev: &Option<CumulativeTokens>, current: &CumulativeTokens) -> DeltaTokens {
    match prev {
        None => DeltaTokens {
            input: current.input as u32,
            cached_input: current.cached_input as u32,
            output: current.output as u32,
        },
        Some(p) => DeltaTokens {
            input: current.input.saturating_sub(p.input) as u32,
            cached_input: current.cached_input.saturating_sub(p.cached_input) as u32,
            output: current.output.saturating_sub(p.output) as u32,
        },
    }
}

fn update_high_water(high_water: &mut CumulativeTokens, current: &CumulativeTokens) {
    high_water.input = high_water.input.max(current.input);
    high_water.cached_input = high_water.cached_input.max(current.cached_input);
    high_water.output = high_water.output.max(current.output);
}

/// 从 JSON Value 中提取累计 token 用量
fn parse_cumulative_tokens(total_usage: &serde_json::Value) -> Option<CumulativeTokens> {
    let fields = total_usage.as_object()?;
    if ![
        "input_tokens",
        "cached_input_tokens",
        "cache_read_input_tokens",
        "output_tokens",
        "reasoning_output_tokens",
        "total_tokens",
    ]
    .iter()
    .any(|field| fields.contains_key(*field))
    {
        return None;
    }
    Some(CumulativeTokens {
        input: total_usage
            .get("input_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        cached_input: total_usage
            .get("cached_input_tokens")
            .or_else(|| total_usage.get("cache_read_input_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        output: total_usage
            .get("output_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
    })
}

type RolloutIndex = HashMap<String, Vec<PathBuf>>;

fn collect_codex_session_files(codex_dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();

    // 1. 扫描 sessions/YYYY/MM/DD/*.jsonl（日期分区目录）
    let sessions_dir = codex_dir.join("sessions");
    if sessions_dir.is_dir() {
        collect_jsonl_recursive(&sessions_dir, &mut files, 0, 3);
    }

    // 2. 扫描 archived_sessions/*.jsonl（扁平归档目录）
    let archived_dir = codex_dir.join("archived_sessions");
    if archived_dir.is_dir() {
        if let Ok(entries) = fs::read_dir(&archived_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                    files.push(path);
                }
            }
        }
    }

    files.retain(|path| {
        path.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(is_rollout_filename)
    });
    files.sort();
    files
}

fn build_rollout_index(files: &[PathBuf]) -> RolloutIndex {
    let mut index = RolloutIndex::new();
    for path in files {
        if let Some(thread_id) = thread_id_from_filename(path) {
            index.entry(thread_id).or_default().push(path.clone());
        }
        // paginated / resume 的续写页（`<threadId>_<rolloutId>`）同时登记在线程
        // 本体 UUID 下：子代理的 forked_from_id 指向线程本体，解析父时间线时
        // 必须能看到首页之后的分段。
        if let Some(thread_id) = leading_thread_id_from_filename(path) {
            index.entry(thread_id).or_default().push(path.clone());
        }
    }
    for paths in index.values_mut() {
        paths.sort();
    }
    index
}

/// 递归扫描目录下的 .jsonl 文件（限制最大深度）
fn collect_jsonl_recursive(dir: &Path, files: &mut Vec<PathBuf>, depth: u32, max_depth: u32) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && depth < max_depth {
            collect_jsonl_recursive(&path, files, depth + 1, max_depth);
        } else if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            files.push(path);
        }
    }
}

fn parse_codex_file(
    file_path: &Path,
    root_thread_id: Option<String>,
) -> Result<ParsedCodexFile, anyhow::Error> {
    let file =
        fs::File::open(file_path).map_err(|e| anyhow::anyhow!(format!("无法打开文件: {e}")))?;
    let mut reader = BufReader::with_capacity(CODEX_JSONL_BUFFER_CAPACITY, file);
    let mut root_meta_seen = false;
    let mut root_timestamp = None;
    let mut meta_thread_id = None;
    let mut parent = ParentResolution::None;
    let mut current_model = "unknown".to_string();
    let mut current_metadata = UsageMetadata::default();
    // `total_token_usage` is session-cumulative, including across model and
    // rate-limit bucket changes. Divergent snapshots are handled by preferring
    // exact `last_token_usage`, not by splitting the cumulative baseline.
    let mut total_high_water = None;
    // Rate-limit refreshes can re-emit unchanged token info under another
    // `limit_id`. Same-source repeats are identified by that source's latest
    // full snapshot; cross-source repeats must match the immediately preceding
    // token event. Do not compare against other sources' older snapshots:
    // those stale signatures can legitimately recur after a counter reset.
    let mut last_signature_by_source: HashMap<Option<String>, TokenUsageSignature> = HashMap::new();
    let mut previous_token_signature = None;
    let mut event_index = 0u32;
    let mut token_events = Vec::new();
    let mut has_billable_tokens = false;
    let incomplete_tail;

    loop {
        let mut bytes = Vec::new();
        let read = reader
            .read_until(b'\n', &mut bytes)
            .map_err(|e| anyhow::anyhow!(format!("无法读取 Codex 日志: {e}")))?;
        // Count the incomplete suffix too, so an unchanged crashed/closed
        // rollout is skipped rather than fully reparsed on every sync pass.
        // A live writer may have only written part of the final JSON record.
        // Leave its line cursor unconsumed for the next file change, but retain
        // support for a complete final JSON record without a newline.
        if read == 0
            || (bytes.last() != Some(&b'\n')
                && serde_json::from_slice::<serde_json::Value>(&bytes).is_err())
        {
            incomplete_tail = read != 0;
            break;
        }
        let line = match String::from_utf8(bytes) {
            Ok(line) => line,
            Err(_) => continue,
        };
        if line.trim().is_empty() {
            continue;
        }

        let is_event_msg = line.contains("\"event_msg\"");
        let is_turn_context = line.contains("\"turn_context\"");
        let is_session_meta = line.contains("\"session_meta\"");
        if !is_event_msg && !is_turn_context && !is_session_meta {
            continue;
        }
        if is_event_msg
            && !line.contains("\"token_count\"")
            && !line.contains("\"thread_settings_applied\"")
        {
            continue;
        }

        let value: serde_json::Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(_) => continue,
        };
        let Some(event_type) = value.get("type").and_then(serde_json::Value::as_str) else {
            continue;
        };

        match event_type {
            "session_meta" if !root_meta_seen => {
                root_meta_seen = true;
                root_timestamp = parse_timestamp(value.get("timestamp"));
                let payload = value.get("payload").unwrap_or(&serde_json::Value::Null);
                parent = explicit_parent_from_meta(payload);

                meta_thread_id = non_empty_string(
                    payload
                        .get("id")
                        .or_else(|| payload.get("thread_id"))
                        .or_else(|| payload.get("threadId")),
                )
                .map(|id| {
                    uuid::Uuid::parse_str(&id)
                        .map(|value| value.hyphenated().to_string())
                        .unwrap_or(id)
                });
                if let (Some(filename_id), Some(meta_id)) =
                    (&root_thread_id, meta_thread_id.as_ref())
                {
                    let leading_id = leading_thread_id_from_filename(file_path);
                    let matches =
                        filename_id == meta_id || leading_id.as_deref() == Some(meta_id.as_str());
                    if !matches {
                        parent = ParentResolution::Deferred(format!(
                            "文件名线程 ID ({filename_id}) 与 root meta ID ({meta_id}) 不一致"
                        ));
                    }
                }

                if let ParentResolution::Parent(parent_id) = &mut parent {
                    match uuid::Uuid::parse_str(parent_id) {
                        Ok(value) => *parent_id = value.hyphenated().to_string(),
                        Err(_) => {
                            parent = ParentResolution::Deferred(format!(
                                "显式 parent_thread_id 不是有效 UUID: {parent_id}"
                            ));
                        }
                    }
                }
                if matches!((&root_thread_id, &parent), (Some(root), ParentResolution::Parent(parent_id)) if root == parent_id)
                {
                    parent = ParentResolution::Deferred(
                        "parent_thread_id 与 root_thread_id 相同".to_string(),
                    );
                }
            }
            "turn_context" => {
                if let Some(payload) = value.get("payload") {
                    let context = UsageMetadata::from_request(payload);
                    current_metadata.reasoning_effort = context.reasoning_effort;
                    if payload.get("service_tier").is_some() {
                        current_metadata.service_tier = context.service_tier;
                        current_metadata.service_tier_source = context.service_tier_source;
                    }
                    if let Some(model) = payload
                        .get("model")
                        .or_else(|| payload.get("info").and_then(|info| info.get("model")))
                        .and_then(serde_json::Value::as_str)
                    {
                        current_model = normalize_codex_model(model);
                    }
                }
            }
            "event_msg" => {
                let Some(payload) = value.get("payload") else {
                    continue;
                };
                if payload.get("type").and_then(serde_json::Value::as_str)
                    == Some("thread_settings_applied")
                {
                    let thread_id = payload.get("thread_id").and_then(serde_json::Value::as_str);
                    if thread_id.is_some()
                        && thread_id == meta_thread_id.as_deref().or(root_thread_id.as_deref())
                    {
                        if let Some(settings) = payload.get("thread_settings") {
                            current_metadata = UsageMetadata::from_request(settings);
                        }
                    }
                    continue;
                }
                if payload.get("type").and_then(serde_json::Value::as_str) != Some("token_count") {
                    continue;
                }
                let Some(info) = payload.get("info").filter(|info| !info.is_null()) else {
                    continue;
                };
                let Some(signature) = parse_token_signature(info) else {
                    continue;
                };

                if let Some(model) = info
                    .get("model")
                    .or_else(|| info.get("model_name"))
                    .or_else(|| payload.get("model"))
                    .and_then(serde_json::Value::as_str)
                {
                    current_model = normalize_codex_model(model);
                }

                let snapshot_source = token_snapshot_source(payload);
                let total = info
                    .get("total_token_usage")
                    .and_then(parse_cumulative_tokens);
                let last = info
                    .get("last_token_usage")
                    .and_then(parse_cumulative_tokens);
                if total.is_none() && last.is_none() {
                    continue;
                }
                let has_total_snapshot = total.is_some();
                let duplicate_snapshot = has_total_snapshot
                    && (last_signature_by_source.get(&snapshot_source) == Some(&signature)
                        || previous_token_signature.as_ref() == Some(&signature));
                if has_total_snapshot {
                    last_signature_by_source.insert(snapshot_source, signature.clone());
                }
                previous_token_signature = Some(signature.clone());

                let delta = if duplicate_snapshot {
                    DeltaTokens {
                        input: 0,
                        cached_input: 0,
                        output: 0,
                    }
                } else if let Some(last) = last {
                    // Codex provides the exact per-request usage. Prefer it to
                    // subtracting cumulative snapshots, which may come from
                    // multiple independently advancing rate-limit lanes.
                    DeltaTokens {
                        input: last.input as u32,
                        cached_input: last.cached_input as u32,
                        output: last.output as u32,
                    }
                } else if let Some(total) = total.as_ref() {
                    compute_delta(&total_high_water, total)
                } else {
                    continue;
                };
                if let Some(total) = total {
                    if let Some(high_water) = total_high_water.as_mut() {
                        update_high_water(high_water, &total);
                    } else {
                        total_high_water = Some(total);
                    }
                }
                let delta = DeltaTokens {
                    cached_input: delta.cached_input.min(delta.input),
                    ..delta
                };
                let nonzero_index = if delta.is_zero() {
                    None
                } else {
                    has_billable_tokens = true;
                    event_index = event_index.saturating_add(1);
                    Some(event_index)
                };

                token_events.push(ParsedTokenEvent {
                    metadata: current_metadata.clone(),
                    signature,
                    delta,
                    event_index: nonzero_index,
                    model: current_model.clone(),
                    timestamp: value
                        .get("timestamp")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned),
                });
            }
            _ => {}
        }
    }

    Ok(ParsedCodexFile {
        root_thread_id,
        meta_thread_id,
        root_meta_seen,
        root_timestamp,
        parent,
        token_events,
        has_billable_tokens,
        incomplete_tail,
    })
}

fn load_parent_timeline(
    parent_path: &Path,
) -> Result<Arc<ParentTokenTimeline>, ParentTimelineError> {
    let file = fs::File::open(parent_path).map_err(|error| {
        ParentTimelineError::Invalid(format!(
            "无法打开父 rollout {}: {error}",
            parent_path.display()
        ))
    })?;
    let stamp = ParentFileStamp::from_file(&file);
    let cached_timeline = stamp.and_then(|stamp| {
        replay_caches().lock().ok().and_then(|caches| {
            caches
                .parent_timelines
                .get(parent_path)
                .filter(|entry| entry.stamp == stamp)
                .map(|entry| Arc::clone(&entry.timeline))
        })
    });
    if let Some(timeline) = cached_timeline {
        return Ok(timeline);
    }

    let mut timeline = ParentTokenTimeline::default();

    // 必须扫描完整父文件，不能在首个未来时间戳处 break：rollout 写入顺序
    // 不承诺时间戳严格单调。缓存完整时间线后，不同 child cutoff 只需内存过滤。
    // 父时间线不能像普通增量导入那样跳过坏行；半写入的 task_started/token_count
    // 若被忽略，前一个 terminal event 会被误当成稳定文件尾。
    let mut reader = BufReader::with_capacity(CODEX_JSONL_BUFFER_CAPACITY, file);
    for (line_index, line) in (&mut reader).lines().enumerate() {
        let line_number = line_index + 1;
        let line = line.map_err(|error| {
            ParentTimelineError::Invalid(format!(
                "读取父 rollout {} 第 {line_number} 行失败: {error}",
                parent_path.display()
            ))
        })?;
        if line.trim().is_empty() {
            continue;
        }
        let value = serde_json::from_str::<serde_json::Value>(&line).map_err(|error| {
            ParentTimelineError::Invalid(format!(
                "解析父 rollout {} 第 {line_number} 行失败: {error}",
                parent_path.display()
            ))
        })?;
        let timestamp = parse_timestamp(value.get("timestamp"));
        let record_type = value.get("type").and_then(serde_json::Value::as_str);
        let payload_type = value
            .get("payload")
            .and_then(|payload| payload.get("type"))
            .and_then(serde_json::Value::as_str);

        let is_terminal_event = record_type == Some("event_msg")
            && matches!(payload_type, Some("task_complete" | "turn_aborted"));
        let is_turn_activity = matches!(record_type, Some("turn_context" | "response_item"))
            || (record_type == Some("event_msg")
                && matches!(
                    payload_type,
                    Some("task_started" | "user_message" | "token_count")
                ));

        if is_terminal_event || is_turn_activity {
            timeline.record_lifecycle(ParentLifecycleEvent {
                timestamp,
                terminal: is_terminal_event,
            });
        }

        if record_type != Some("event_msg") || payload_type != Some("token_count") {
            continue;
        }
        let Some(info) = value
            .get("payload")
            .and_then(|payload| payload.get("info"))
            .filter(|info| !info.is_null())
        else {
            continue;
        };
        let Some(signature) = parse_token_signature(info) else {
            continue;
        };
        let Some(timestamp) = timestamp else {
            timeline.has_token_without_timestamp = true;
            continue;
        };
        timeline.events.push(TimestampedTokenSignature {
            timestamp,
            signature,
        });
    }

    let final_stamp = ParentFileStamp::from_file(reader.get_ref());
    if stamp.is_some() && final_stamp != stamp {
        return Err(ParentTimelineError::Invalid(format!(
            "父 rollout {} 在读取期间发生变化",
            parent_path.display()
        )));
    }

    let timeline = Arc::new(timeline);
    if let (Some(stamp), Ok(mut caches)) = (stamp, replay_caches().lock()) {
        caches.parent_timelines.insert(
            parent_path.to_path_buf(),
            CachedParentTimeline {
                stamp,
                timeline: Arc::clone(&timeline),
            },
        );
    }
    Ok(timeline)
}

fn resolve_parent_signatures(
    parent_id: &str,
    cutoff: DateTime<Utc>,
    rollout_index: &RolloutIndex,
) -> Result<Vec<TokenUsageSignature>, ParentResolveFailure> {
    let Some(candidates) = rollout_index.get(parent_id) else {
        return Err(ParentResolveFailure {
            reason: format!("找不到父 rollout: {parent_id}"),
        });
    };

    let dependencies = snapshot_parent_dependencies(parent_id, rollout_index);
    let failure = |error: ParentTimelineError| {
        let (_kind, reason) = match error {
            ParentTimelineError::BehindCutoff(reason) => {
                (ParentResolveFailureKind::BehindCutoff, reason)
            }
            ParentTimelineError::Invalid(reason) => (ParentResolveFailureKind::Retryable, reason),
        };
        ParentResolveFailure { reason }
    };
    let mut ordered = candidates.iter().collect::<Vec<_>>();
    ordered.sort_by(|left, right| left.file_name().cmp(&right.file_name()));
    let mut merged = ParentTokenTimeline::default();
    let mut segments: Vec<(Option<String>, Arc<ParentTokenTimeline>, bool)> = Vec::new();
    let mut latest_path: Option<&Path> = None;
    for candidate in ordered {
        let timeline = load_parent_timeline(candidate).map_err(&failure)?;
        // Validate every copy before collapsing duplicate rollouts. A malformed
        // copy must not disappear just because its timestamped signatures match.
        if timeline.has_token_without_timestamp {
            return Err(failure(ParentTimelineError::Invalid(format!(
                "父 rollout {} 的 token_count 缺少有效 timestamp",
                candidate.display(),
            ))));
        }
        let rollout_id = thread_id_from_filename(candidate);
        if let Some((_, first, all_archived)) =
            segments.iter_mut().find(|(id, _, _)| *id == rollout_id)
        {
            if first.signatures_at(cutoff) != timeline.signatures_at(cutoff)
                || first.lifecycle_events != timeline.lifecycle_events
            {
                return Err(failure(ParentTimelineError::Invalid(format!(
                    "父 rollout UUID {parent_id} 对应多个内容不一致的文件",
                ))));
            }
            *all_archived &= is_archived_rollout_path(candidate);
            continue;
        }
        merged.events.extend(timeline.events.iter().cloned());
        // Replay lifecycle transitions across page boundaries; a page containing
        // only metadata must not erase an earlier active or completed turn.
        for event in &timeline.lifecycle_events {
            merged.record_lifecycle(*event);
        }
        segments.push((rollout_id, timeline, is_archived_rollout_path(candidate)));
        latest_path = Some(candidate.as_path());
    }
    if snapshot_parent_dependencies(parent_id, rollout_index) != dependencies {
        return Err(failure(ParentTimelineError::Invalid(format!(
            "父 rollout {parent_id} 在合并页面期间发生变化",
        ))));
    }
    let Some(latest_path) = latest_path else {
        return Err(failure(ParentTimelineError::Invalid(format!(
            "找不到父 rollout: {parent_id}"
        ))));
    };
    let latest_is_archived = segments.last().is_some_and(|(_, _, archived)| *archived);
    merged
        .signatures_before(latest_path, cutoff, latest_is_archived)
        .map_err(failure)
}

fn matching_replay_prefix(child: &[ParsedTokenEvent], parent: &[TokenUsageSignature]) -> usize {
    let mut parent_offset = 0usize;
    let mut matched = 0usize;
    for event in child {
        let Some(relative_match) = parent[parent_offset..]
            .iter()
            .position(|signature| signature == &event.signature)
        else {
            break;
        };
        parent_offset += relative_match + 1;
        matched += 1;
    }
    matched
}

pub(super) fn import(
    root: &Path,
    report: &mut ImportReport,
    should_scan: &impl Fn(&Path) -> bool,
) -> anyhow::Result<()> {
    let files = collect_codex_session_files(root);
    let index = build_rollout_index(&files);
    // Bound caches to this pass; dependencies are freshly checked for each child.
    if let Ok(mut caches) = replay_caches().lock() {
        caches.parent_timelines.clear();
    }
    for path in files {
        if !should_scan(&path) {
            continue;
        }
        let parsed = parse_codex_file(&path, thread_id_from_filename(&path))?;
        report.files_scanned += 1;
        report.scanned_paths.push(path.clone());
        if parsed.incomplete_tail {
            report.deferred_paths.push(path.clone());
        }
        if !parsed.has_billable_tokens {
            continue;
        }
        let Some(root_id) = parsed
            .root_thread_id
            .as_deref()
            .filter(|_| parsed.root_meta_seen)
        else {
            report.deferred_paths.push(path);
            continue;
        };
        let replay = match &parsed.parent {
            ParentResolution::None => 0,
            ParentResolution::Deferred(_reason) => {
                report.deferred_paths.push(path);
                continue;
            }
            ParentResolution::Parent(parent) => {
                let Some(cutoff) = parsed.root_timestamp else {
                    report.deferred_paths.push(path);
                    continue;
                };
                match resolve_parent_signatures(parent, cutoff, &index) {
                    Ok(signatures) => matching_replay_prefix(&parsed.token_events, &signatures),
                    Err(failure) => {
                        let _reason = failure.reason;
                        report.deferred_paths.push(path);
                        continue;
                    }
                }
            }
        };
        for event in parsed.token_events.iter().skip(replay) {
            let Some(event_index) = event.event_index else {
                continue;
            };
            let Some(created_at) = event
                .timestamp
                .as_deref()
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|t| t.timestamp())
            else {
                report.deferred_paths.push(path.clone());
                continue;
            };
            report.records.push(UsageRecord {
                request_id: format!("codex_session:thread-v1:{root_id}:{event_index}"),
                app_type: "codex".into(),
                provider_id: "_codex_session".into(),
                provider_type: "codex_session".into(),
                data_source: "codex_session".into(),
                model: event.model.clone(),
                request_model: event.model.clone(),
                pricing_model: None,
                service_tier: event.metadata.service_tier.clone(),
                service_tier_source: event.metadata.service_tier_source.clone(),
                reasoning_effort: event.metadata.reasoning_effort.clone(),
                service_tier_pricing_version: Some(2),
                input_tokens: event.delta.input as i64,
                output_tokens: event.delta.output as i64,
                cache_read_tokens: event.delta.cached_input as i64,
                cache_creation_tokens: 0,
                input_token_semantics: 1,
                created_at,
                session_id: Some(parsed.meta_thread_id.as_deref().unwrap_or(root_id).into()),
                source_path: path.clone(),
                is_streaming: true,
                status_code: 200,
                latency_ms: 0,
                reported_total_cost_usd: None,
                identity: None,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    const P: &str = "00000000-0000-4000-8000-000000000001";
    const R: &str = "00000000-0000-4000-8000-000000000002";
    const C: &str = "00000000-0000-4000-8000-000000000003";
    fn meta(id: &str, parent: Option<&str>) -> Value {
        let mut v =
            json!({"type":"session_meta","timestamp":"2026-09-26T10:03:00Z","payload":{"id":id}});
        if let Some(p) = parent {
            v["payload"]["forked_from_id"] = json!(p);
        }
        v
    }
    fn token(n: u64, minute: u64) -> Value {
        json!({"type":"event_msg","timestamp":format!("2026-09-26T10:{minute:02}:00Z"),"payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":n,"cached_input_tokens":0,"output_tokens":0}}}})
    }
    fn done() -> Value {
        json!({"type":"event_msg","timestamp":"2026-09-26T10:02:00Z","payload":{"type":"task_complete"}})
    }
    fn write(root: &Path, suffix: &str, lines: &[Value]) -> PathBuf {
        let dir = root.join("sessions");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("rollout-2026-09-26-{suffix}.jsonl"));
        fs::write(
            &path,
            lines
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n")
                + "\n",
        )
        .unwrap();
        path
    }
    fn scan(root: &Path) -> ImportReport {
        let mut report = ImportReport::default();
        import(root, &mut report, &|_| true).unwrap();
        report
    }
    #[test]
    fn paginated_parent_recovers_unchanged_child_and_preserves_request_indices() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), P, &[meta(P, None), token(10, 0)]);
        let child = write(
            dir.path(),
            C,
            &[meta(C, Some(P)), token(10, 0), token(20, 1), token(30, 4)],
        );
        assert!(scan(dir.path()).deferred_paths.contains(&child));
        write(
            dir.path(),
            &format!("{P}_{R}"),
            &[meta(P, None), token(20, 1), done()],
        );
        let report = scan(dir.path());
        assert!(report.deferred_paths.is_empty());
        let rows = report
            .records
            .iter()
            .filter(|r| r.source_path == child)
            .collect::<Vec<_>>();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].input_tokens, 10);
        assert_eq!(rows[0].request_id, format!("codex_session:thread-v1:{C}:3"));
        assert_eq!(scan(dir.path()).records, report.records);
        let resumed = report
            .records
            .iter()
            .find(|r| r.request_id.contains(R))
            .unwrap();
        assert_eq!(resumed.session_id.as_deref(), Some(P));
    }
    #[test]
    fn metadata_timeline_keeps_unknown_and_observed_request_settings() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            C,
            &[
                meta(C, None),
                token(10, 0),
                json!({"type":"turn_context","payload":{"model":"gpt-5.5","service_tier":"priority","reasoning":{"effort":"xhigh"}}}),
                token(20, 1),
                json!({"type":"turn_context","payload":{"model":"gpt-5.5","service_tier":"default","reasoning_effort":"low"}}),
                token(30, 2),
            ],
        );
        let rows = scan(dir.path()).records;
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].service_tier, None);
        assert_eq!(rows[1].service_tier.as_deref(), Some("priority"));
        assert_eq!(rows[1].reasoning_effort.as_deref(), Some("xhigh"));
        assert_eq!(rows[1].service_tier_source.as_deref(), Some("request"));
        assert_eq!(rows[2].service_tier.as_deref(), Some("default"));
        assert_eq!(rows[2].reasoning_effort.as_deref(), Some("low"));
    }
    #[test]
    fn missing_parent_bad_parent_and_conflicting_parent_are_deferred() {
        let dir = tempfile::tempdir().unwrap();
        let child = write(
            dir.path(),
            C,
            &[meta(C, Some(P)), token(10, 0), token(20, 4)],
        );
        assert_eq!(scan(dir.path()).deferred_paths, vec![child.clone()]);
        let parent = write(dir.path(), P, &[meta(P, None), token(10, 0), done()]);
        let mut bytes = fs::read_to_string(&parent).unwrap();
        bytes.push_str("{\"type\":\"event_msg\"");
        fs::write(&parent, bytes).unwrap();
        assert!(scan(dir.path()).deferred_paths.contains(&child));
        write(dir.path(), P, &[meta(P, None), token(10, 0), done()]);
        assert!(!scan(dir.path()).deferred_paths.contains(&child));
        let mut conflict = meta(C, Some(P));
        conflict["payload"]["source"] = json!({"subagent":{"thread_spawn":{"parent_thread_id":R}}});
        write(dir.path(), C, &[conflict, token(20, 4)]);
        assert!(scan(dir.path()).deferred_paths.contains(&child));
    }
    #[test]
    fn partial_child_tail_is_retried_and_model_switch_keeps_total_baseline() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), C, &[meta(C, None), token(10, 0)]);
        let original = fs::read_to_string(&path).unwrap();
        fs::write(&path, format!("{original}{{\"type\":")).unwrap();
        assert_eq!(scan(dir.path()).records.len(), 1);
        write(
            dir.path(),
            C,
            &[
                meta(C, None),
                token(10, 0),
                json!({"type":"turn_context","payload":{"model":"gpt-5.5"}}),
                token(15, 1),
            ],
        );
        let records = scan(dir.path()).records;
        assert_eq!(records.len(), 2);
        assert_eq!(records[1].input_tokens, 5);
    }
}

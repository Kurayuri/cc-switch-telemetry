use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u32 = 3;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct UsageEvent {
    pub request_id: String,
    pub created_at: i64,
    pub app_type: String,
    pub provider_id: String,
    pub model: String,
    #[serde(default)]
    pub request_model: Option<String>,
    #[serde(default)]
    pub pricing_model: Option<String>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
    #[serde(default)]
    pub input_token_semantics: i64,
    pub total_cost_usd: String,
    pub latency_ms: i64,
    pub status_code: i64,
    pub is_streaming: bool,
    #[serde(default)]
    pub data_source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum MutationKind {
    Upsert,
    Delete,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct EventMutation {
    pub operation: MutationKind,
    pub app_type: String,
    pub request_id: String,
    #[serde(default)]
    pub content_hash: String,
    #[serde(default)]
    pub event: Option<UsageEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct EventBatch {
    pub schema_version: u32,
    pub events: Vec<UsageEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSnapshot {
    pub schema_version: u32,
    pub providers: Vec<ProviderEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProviderEntry {
    pub app_type: String,
    pub provider_id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct BatchResponse {
    pub accepted: Vec<String>,
    pub duplicates: Vec<String>,
    pub rejected: Vec<RejectedEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct SyncBeginRequest {
    pub schema_version: u32,
    pub generation_id: String,
    pub replace_all: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SyncBeginResponse {
    pub generation_id: String,
    pub resumed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct EventMutationBatch {
    pub schema_version: u32,
    pub generation_id: String,
    pub mutations: Vec<EventMutation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RejectedEvent {
    pub event_id: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct RollupSnapshot {
    pub schema_version: u32,
    pub date: String,
    pub app_type: String,
    pub provider_id: String,
    pub model: String,
    pub request_model: String,
    pub pricing_model: String,
    pub request_count: i64,
    pub success_count: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
    pub input_token_semantics: i64,
    pub total_cost_usd: String,
    pub avg_latency_ms: f64,
    pub day_start_utc: i64,
    pub day_end_utc: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct RollupMutation {
    pub operation: MutationKind,
    pub snapshot_key: String,
    #[serde(default)]
    pub content_hash: String,
    #[serde(default)]
    pub snapshot: Option<RollupSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct RollupMutationBatch {
    pub schema_version: u32,
    pub generation_id: String,
    pub mutations: Vec<RollupMutation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct ProviderMutationBatch {
    pub schema_version: u32,
    pub generation_id: String,
    pub providers: Vec<ProviderEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct SyncCommitRequest {
    pub schema_version: u32,
    pub generation_id: String,
    pub expected_event_mutations: usize,
    pub expected_rollup_mutations: usize,
    pub manifest_hash: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SyncCommitResponse {
    pub inserted: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub deleted: usize,
    pub rollups: usize,
    pub providers: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QuotaProviderStatus {
    Ok,
    NotAvailable,
    CredentialParseFailed,
    LoginExpired,
    QueryFailed,
    CommandFailed,
    TimedOut,
    InvalidOutput,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum QuotaTargetKind {
    SubscriptionTool,
    CodexOAuth,
    UsageScript,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum QuotaMetricKind {
    UtilizationPercent,
    Balance,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct QuotaMetric {
    pub key: String,
    pub label: String,
    pub kind: QuotaMetricKind,
    #[serde(default)]
    pub utilization_percent: Option<f64>,
    #[serde(default)]
    pub used: Option<f64>,
    #[serde(default)]
    pub remaining: Option<f64>,
    #[serde(default)]
    pub total: Option<f64>,
    #[serde(default)]
    pub unit: Option<String>,
    #[serde(default)]
    pub resets_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct QuotaProviderState {
    pub app_type: String,
    pub provider_id: String,
    pub provider_name: String,
    pub status: QuotaProviderStatus,
    #[serde(default)]
    pub target_kind: Option<QuotaTargetKind>,
    pub checked_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct QuotaObservation {
    pub observation_id: String,
    pub app_type: String,
    pub provider_id: String,
    pub sampled_at: i64,
    pub metrics: Vec<QuotaMetric>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct QuotaUploadBatch {
    pub schema_version: u32,
    pub provider_states: Vec<QuotaProviderState>,
    pub observations: Vec<QuotaObservation>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct QuotaBatchResponse {
    pub accepted: Vec<String>,
    pub duplicates: Vec<String>,
    pub provider_states: usize,
}

pub fn event_id(node_id: &str, app_type: &str, request_id: &str) -> String {
    format!(
        "{}:{node_id}:{}:{app_type}:{request_id}",
        node_id.len(),
        app_type.len()
    )
}

pub fn event_source_key(app_type: &str, request_id: &str) -> String {
    format!("{}:{app_type}:{request_id}", app_type.len())
}

pub fn split_event_source_key(key: &str) -> Option<(&str, &str)> {
    let (length, remainder) = key.split_once(':')?;
    let length = length.parse::<usize>().ok()?;
    if length == 0 || remainder.len() <= length || !remainder.is_char_boundary(length) {
        return None;
    }
    let (app_type, request_id) = remainder.split_at(length);
    let request_id = request_id.strip_prefix(':')?;
    (!app_type.is_empty() && !request_id.is_empty()).then_some((app_type, request_id))
}

pub fn rollup_key(
    node_id: &str,
    date: &str,
    app_type: &str,
    provider_id: &str,
    model: &str,
    request_model: &str,
    pricing_model: &str,
) -> String {
    [
        node_id,
        date,
        app_type,
        provider_id,
        model,
        request_model,
        pricing_model,
    ]
    .join("|")
}

pub fn rollup_source_key(snapshot: &RollupSnapshot) -> String {
    [
        snapshot.date.as_str(),
        snapshot.app_type.as_str(),
        snapshot.provider_id.as_str(),
        snapshot.model.as_str(),
        snapshot.request_model.as_str(),
        snapshot.pricing_model.as_str(),
    ]
    .join("|")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_stable() {
        assert_eq!(event_id("n", "a", "r"), "1:n:1:a:r");
        assert_eq!(event_source_key("a", "r"), "1:a:r");
        assert_eq!(split_event_source_key("1:a:r|2"), Some(("a", "r|2")));
        assert_eq!(split_event_source_key("3:a:b:c"), Some(("a:b", "c")));
        assert_eq!(split_event_source_key("3:猫:r"), Some(("猫", "r")));
        assert_eq!(
            rollup_key("n", "2026-01-01", "a", "p", "m", "", ""),
            "n|2026-01-01|a|p|m||"
        );
    }
}

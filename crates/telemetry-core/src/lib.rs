use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u32 = 2;

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
    pub source_kind: String,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    pub avg_latency_ms: i64,
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

pub fn event_id(node_id: &str, request_id: &str) -> String {
    format!("{node_id}:{request_id}")
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
        assert_eq!(event_id("n", "r"), "n:r");
        assert_eq!(
            rollup_key("n", "2026-01-01", "a", "p", "m", "", ""),
            "n|2026-01-01|a|p|m||"
        );
    }
}

use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UsageEvent {
    pub event_id: String,
    pub node_id: String,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventBatch {
    pub schema_version: u32,
    pub node_id: String,
    pub events: Vec<UsageEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct BatchResponse {
    pub accepted: Vec<String>,
    pub duplicates: Vec<String>,
    pub rejected: Vec<RejectedEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RejectedEvent {
    pub event_id: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RollupSnapshot {
    pub schema_version: u32,
    pub node_id: String,
    pub snapshot_key: String,
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
    pub total_cost_usd: String,
    pub avg_latency_ms: i64,
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

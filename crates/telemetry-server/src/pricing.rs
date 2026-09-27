//! Reference prices are snapshots for display-only cost allocation, not bills.
use crate::{admin, ServerState};
use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    sync::OnceLock,
    time::{Duration, Instant},
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReferencePricing {
    pub model: String,
    pub resolved_model: String,
    pub source: String,
    pub fetched_at: i64,
    pub fresh: Option<f64>,
    pub creation: Option<f64>,
    pub read: Option<f64>,
    pub output: Option<f64>,
}

impl ReferencePricing {
    pub fn prices(&self) -> [Option<f64>; 4] {
        [self.fresh, self.creation, self.read, self.output]
    }
    pub fn validate(&self, model: &str) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.model == model
                && self.fetched_at > 0
                && !self.resolved_model.is_empty()
                && self.resolved_model.len() <= 512
                && !self.source.is_empty()
                && self.source.len() <= 256
                && self
                    .prices()
                    .iter()
                    .all(|price| price.is_none_or(|v| v.is_finite() && v >= 0.0)),
            "invalid reference price snapshot"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
struct Provider {
    #[serde(default)]
    models: BTreeMap<String, Model>,
}
#[derive(Debug, Clone, Deserialize)]
struct Model {
    cost: Option<Cost>,
}
#[derive(Debug, Clone, PartialEq, Deserialize)]
struct Cost {
    input: Option<f64>,
    output: Option<f64>,
    cache_read: Option<f64>,
    cache_write: Option<f64>,
}
type Catalog = BTreeMap<String, Provider>;

fn select(
    catalog: &Catalog,
    requested: &str,
    fetched_at: i64,
    source: &str,
) -> anyhow::Result<ReferencePricing> {
    let normalized = requested.to_ascii_lowercase();
    let preferred = if normalized.contains("gpt")
        || normalized.contains("codex")
        || normalized
            .strip_prefix('o')
            .is_some_and(|tail| tail.starts_with(|c: char| c.is_ascii_digit()))
    {
        Some("openai")
    } else if normalized.contains("claude") {
        Some("anthropic")
    } else if normalized.contains("gemini") {
        Some("google")
    } else {
        None
    };
    for candidate in cc_switch_usage_core::pricing_candidates(requested) {
        let matches: Vec<_> = catalog
            .iter()
            .flat_map(|(provider, entry)| {
                entry
                    .models
                    .iter()
                    .filter(|(name, _)| {
                        cc_switch_usage_core::pricing_candidates(name).contains(&candidate)
                    })
                    .filter_map(move |(name, model)| {
                        model.cost.as_ref().map(|cost| (provider, name, cost))
                    })
            })
            .collect();
        let canonical: Vec<_> = matches
            .iter()
            .copied()
            .filter(|(provider, _, _)| Some(provider.as_str()) == preferred)
            .collect();
        let choices = if canonical.is_empty() {
            &matches
        } else {
            &canonical
        };
        // Prefer the exact model id over aliases and dated variants.
        let exact: Vec<_> = choices
            .iter()
            .copied()
            .filter(|(_, name, _)| name.as_str() == candidate)
            .collect();
        let choices = if exact.is_empty() { choices } else { &exact };
        if let Some((provider, name, cost)) = choices.first() {
            anyhow::ensure!(
                choices.iter().all(|(_, _, other)| *other == *cost),
                "model price is ambiguous"
            );
            let price = |value: Option<f64>| value.filter(|v| v.is_finite() && *v >= 0.0);
            let result = ReferencePricing {
                model: requested.to_owned(),
                resolved_model: format!("{provider}/{name}"),
                source: source.to_owned(),
                fetched_at,
                fresh: price(cost.input),
                creation: price(cost.cache_write),
                read: price(cost.cache_read),
                output: price(cost.output),
            };
            anyhow::ensure!(
                result.prices().iter().any(Option::is_some),
                "model has no usable prices"
            );
            result.validate(requested)?;
            return Ok(result);
        }
    }
    anyhow::bail!("model reference prices not found")
}

struct CachedCatalog {
    url: String,
    loaded: Instant,
    fetched_at: i64,
    catalog: Catalog,
}
static CACHE: OnceLock<tokio::sync::Mutex<Option<CachedCatalog>>> = OnceLock::new();

pub(crate) async fn reference_price(
    model: &str,
    refresh: bool,
) -> anyhow::Result<ReferencePricing> {
    anyhow::ensure!(
        !model.trim().is_empty() && model.len() <= 256 && !model.chars().any(char::is_control),
        "invalid model"
    );
    let url = std::env::var("TELEMETRY_MODELS_DEV_URL")
        .unwrap_or_else(|_| "https://models.dev/api.json".into());
    let source = if url == "https://models.dev/api.json" {
        "models.dev"
    } else {
        "configured catalog"
    };
    let mut cache = CACHE
        .get_or_init(|| tokio::sync::Mutex::new(None))
        .lock()
        .await;
    if refresh
        || cache
            .as_ref()
            .is_none_or(|c| c.url != url || c.loaded.elapsed() >= Duration::from_secs(86400))
    {
        let response = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()?
            .get(&url)
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("reference price catalog unavailable"))?;
        anyhow::ensure!(
            response.status().is_success(),
            "reference price catalog returned an error"
        );
        let catalog: Catalog = response
            .json()
            .await
            .map_err(|_| anyhow::anyhow!("invalid reference price catalog"))?;
        *cache = Some(CachedCatalog {
            url,
            loaded: Instant::now(),
            fetched_at: chrono::Utc::now().timestamp(),
            catalog,
        });
    }
    let cached = cache.as_ref().unwrap();
    select(&cached.catalog, model, cached.fetched_at, source)
}

#[derive(Deserialize)]
pub(crate) struct PriceQuery {
    model: String,
    #[serde(default)]
    refresh: bool,
}

pub(crate) async fn resolve(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Query(query): Query<PriceQuery>,
) -> Response {
    if let Err(response) = admin::require_admin(&state, &headers) {
        return *response;
    }
    match reference_price(query.model.trim(), query.refresh).await {
        Ok(price) => Json(price).into_response(),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"message": error.to_string()})),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deterministic_prices_keep_missing_distinct_from_zero() {
        let catalog = serde_json::from_value(serde_json::json!({
            "reseller": {"models": {"gpt-example": {"cost": {"input": 100}}}},
            "openai": {"models": {"gpt-example": {"cost": {"input": 2, "output": 8, "cache_read": 0}}}}
        })).unwrap();
        let result = select(&catalog, "openai/gpt-example", 100, "fixture").unwrap();
        assert_eq!(result.fresh, Some(2.0));
        assert_eq!(result.read, Some(0.0));
        assert_eq!(result.creation, None);
        assert_eq!(result.resolved_model, "openai/gpt-example");
        assert!(select(&catalog, "unknown", 100, "fixture").is_err());
        let ambiguous = serde_json::from_value(serde_json::json!({
            "a": {"models": {"example": {"cost": {"input": 1}}}},
            "b": {"models": {"example": {"cost": {"input": 2}}}}
        }))
        .unwrap();
        assert!(select(&ambiguous, "example", 100, "fixture").is_err());
    }
}

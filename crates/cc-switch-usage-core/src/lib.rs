//! Canonical, host-neutral usage-accounting policy for cc-switch consumers.
//!
//! Keep parsing and persistence adapters outside this crate unless they can be
//! expressed without Tauri state. Token semantics, pricing, normalization,
//! deduplication, and rollup range decisions belong here so every consumer
//! produces the same answer from the same rows.

use chrono::{NaiveDate, TimeZone, Timelike};
use rust_decimal::Decimal;

pub const CACHE_INCLUSIVE_APP_TYPES: &[&str] = &["codex", "gemini", "grokbuild"];

pub const INPUT_TOKEN_SEMANTICS_LEGACY: i64 = 0;
pub const INPUT_TOKEN_SEMANTICS_TOTAL: i64 = 1;
pub const INPUT_TOKEN_SEMANTICS_FRESH: i64 = 2;

pub const SESSION_PROXY_DEDUP_WINDOW_SECONDS: i64 = 10 * 60;

pub fn is_cache_inclusive_app(app_type: &str) -> bool {
    CACHE_INCLUSIVE_APP_TYPES.contains(&app_type)
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TokenCounts {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
}

pub fn fresh_input_tokens(
    app_type: &str,
    semantics: i64,
    input_tokens: i64,
    cache_read_tokens: i64,
    cache_creation_tokens: i64,
) -> i64 {
    if semantics == INPUT_TOKEN_SEMANTICS_FRESH || !is_cache_inclusive_app(app_type) {
        return input_tokens;
    }

    let cached = if semantics == INPUT_TOKEN_SEMANTICS_TOTAL {
        cache_read_tokens.saturating_add(cache_creation_tokens)
    } else {
        cache_read_tokens
    };
    input_tokens
        .checked_sub(cached)
        .filter(|value| *value >= 0)
        .unwrap_or(input_tokens)
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModelPricing {
    pub input_cost_per_million: Decimal,
    pub output_cost_per_million: Decimal,
    pub cache_read_cost_per_million: Decimal,
    pub cache_creation_cost_per_million: Decimal,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CostBreakdown {
    pub input_cost: Decimal,
    pub output_cost: Decimal,
    pub cache_read_cost: Decimal,
    pub cache_creation_cost: Decimal,
    pub total_cost: Decimal,
}

pub fn calculate_cost(
    app_type: &str,
    usage: TokenCounts,
    pricing: &ModelPricing,
    cost_multiplier: Decimal,
) -> CostBreakdown {
    let million = Decimal::from(1_000_000);
    let billable_input_tokens = if is_cache_inclusive_app(app_type) {
        usage
            .input_tokens
            .saturating_sub(usage.cache_read_tokens)
            .saturating_sub(usage.cache_creation_tokens)
    } else {
        usage.input_tokens
    }
    .max(0);
    let input_cost =
        Decimal::from(billable_input_tokens) * pricing.input_cost_per_million / million;
    let output_cost =
        Decimal::from(usage.output_tokens.max(0)) * pricing.output_cost_per_million / million;
    let cache_read_cost = Decimal::from(usage.cache_read_tokens.max(0))
        * pricing.cache_read_cost_per_million
        / million;
    let cache_creation_cost = Decimal::from(usage.cache_creation_tokens.max(0))
        * pricing.cache_creation_cost_per_million
        / million;
    let total_cost =
        (input_cost + output_cost + cache_read_cost + cache_creation_cost) * cost_multiplier;

    CostBreakdown {
        input_cost,
        output_cost,
        cache_read_cost,
        cache_creation_cost,
        total_cost,
    }
}

pub fn folded_app_type(app_type: &str) -> &str {
    if app_type == "claude-desktop" {
        "claude"
    } else {
        app_type
    }
}

pub fn effective_model<'a>(model: &'a str, pricing_model: Option<&'a str>) -> &'a str {
    pricing_model
        .filter(|value| !value.is_empty())
        .unwrap_or(model)
}

pub mod sql {
    use super::{
        CACHE_INCLUSIVE_APP_TYPES, INPUT_TOKEN_SEMANTICS_FRESH, INPUT_TOKEN_SEMANTICS_LEGACY,
        INPUT_TOKEN_SEMANTICS_TOTAL, SESSION_PROXY_DEDUP_WINDOW_SECONDS,
    };

    pub fn fresh_input(alias: &str) -> String {
        let prefix = if alias.is_empty() {
            String::new()
        } else {
            format!("{alias}.")
        };
        let app_type_list = CACHE_INCLUSIVE_APP_TYPES
            .iter()
            .map(|value| format!("'{value}'"))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "CASE \
                  WHEN {prefix}input_token_semantics = {INPUT_TOKEN_SEMANTICS_FRESH} THEN {prefix}input_tokens \
                  WHEN {prefix}app_type IN ({app_type_list}) \
                       AND {prefix}input_token_semantics = {INPUT_TOKEN_SEMANTICS_TOTAL} \
                       AND {prefix}input_tokens >= ({prefix}cache_read_tokens + {prefix}cache_creation_tokens) \
                  THEN ({prefix}input_tokens - {prefix}cache_read_tokens - {prefix}cache_creation_tokens) \
                  WHEN {prefix}app_type IN ({app_type_list}) \
                       AND {prefix}input_token_semantics = {INPUT_TOKEN_SEMANTICS_LEGACY} \
                       AND {prefix}input_tokens >= {prefix}cache_read_tokens \
                  THEN ({prefix}input_tokens - {prefix}cache_read_tokens) \
                  ELSE {prefix}input_tokens END"
        )
    }

    pub fn folded_app_type(column: &str) -> String {
        format!("CASE WHEN {column} = 'claude-desktop' THEN 'claude' ELSE {column} END")
    }

    pub fn effective_model(alias: &str) -> String {
        format!("COALESCE(NULLIF({alias}.pricing_model, ''), {alias}.model)")
    }

    fn data_source(alias: &str) -> String {
        format!("COALESCE({alias}.data_source, 'proxy')")
    }

    /// Build cc-switch's effective-log predicate.
    ///
    /// `partition_match` is appended to the proxy subquery. The desktop app
    /// passes `None`; a central collector passes a node equality predicate so
    /// deduplication can never cross machines.
    pub fn effective_usage_log_filter(alias: &str, partition_match: Option<&str>) -> String {
        effective_usage_log_filter_for_table(alias, "proxy_request_logs", partition_match)
    }

    pub fn effective_usage_log_filter_for_table(
        alias: &str,
        table: &str,
        partition_match: Option<&str>,
    ) -> String {
        let source = data_source(alias);
        let proxy_source = data_source("proxy_dedup");
        let app_match = format!(
            "proxy_dedup.app_type IN ({alias}.app_type, CASE WHEN {alias}.app_type = 'claude' THEN 'claude-desktop' ELSE {alias}.app_type END)"
        );
        let partition = partition_match
            .map(|predicate| format!("\n                  AND ({predicate})"))
            .unwrap_or_default();
        format!(
            "NOT (\n\
                {source} IN ('session_log', 'codex_session', 'gemini_session', 'opencode_session', 'grok_session', 'pi_session')\n\
                AND EXISTS (\n\
                    SELECT 1\n\
                    FROM {table} proxy_dedup\n\
                    WHERE {proxy_source} = 'proxy'{partition}\n\
                      AND {app_match}\n\
                      AND proxy_dedup.status_code >= 200\n\
                      AND proxy_dedup.status_code < 300\n\
                      AND proxy_dedup.input_tokens = {alias}.input_tokens\n\
                      AND proxy_dedup.output_tokens = {alias}.output_tokens\n\
                      AND proxy_dedup.cache_read_tokens = {alias}.cache_read_tokens\n\
                      AND (\n\
                          proxy_dedup.cache_creation_tokens = {alias}.cache_creation_tokens\n\
                          OR (\n\
                              {alias}.cache_creation_tokens = 0\n\
                              AND {source} IN ('codex_session', 'gemini_session', 'opencode_session', 'grok_session', 'pi_session')\n\
                          )\n\
                      )\n\
                      AND proxy_dedup.created_at BETWEEN\n\
                          {alias}.created_at - {SESSION_PROXY_DEDUP_WINDOW_SECONDS}\n\
                          AND {alias}.created_at + {SESSION_PROXY_DEDUP_WINDOW_SECONDS}\n\
                      AND (\n\
                          LOWER(proxy_dedup.model) = LOWER({alias}.model)\n\
                          OR LOWER(proxy_dedup.model) = 'unknown'\n\
                          OR LOWER({alias}.model) = 'unknown'\n\
                      )\n\
                )\n\
            )"
        )
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RollupDateBounds {
    pub start: Option<String>,
    pub end: Option<String>,
    pub is_empty: bool,
}

/// Return complete local dates fully covered by an inclusive timestamp range.
pub fn compute_rollup_date_bounds<Tz: TimeZone>(
    timezone: &Tz,
    start_ts: Option<i64>,
    end_ts: Option<i64>,
) -> Option<RollupDateBounds> {
    let start = match start_ts {
        Some(timestamp) => {
            let local = timezone.timestamp_opt(timestamp, 0).single()?;
            let day = local.date_naive();
            if local.time().num_seconds_from_midnight() == 0 {
                Some(day.format("%Y-%m-%d").to_string())
            } else {
                Some(day.succ_opt()?.format("%Y-%m-%d").to_string())
            }
        }
        None => None,
    };
    let end = match end_ts {
        Some(timestamp) => {
            let local = timezone.timestamp_opt(timestamp, 0).single()?;
            let day = local.date_naive();
            if local.time().hour() == 23 && local.time().minute() == 59 {
                Some(day.format("%Y-%m-%d").to_string())
            } else {
                Some(day.pred_opt()?.format("%Y-%m-%d").to_string())
            }
        }
        None => None,
    };
    let is_empty = matches!((&start, &end), (Some(start), Some(end)) if start > end);
    Some(RollupDateBounds {
        start,
        end,
        is_empty,
    })
}

/// Resolve a source-local day to half-open UTC bounds, using the same DST-gap
/// fallback as cc-switch's rollup cutoff.
pub fn local_day_utc_bounds<Tz: TimeZone>(timezone: &Tz, day: NaiveDate) -> Option<(i64, i64)> {
    fn midnight<Tz: TimeZone>(timezone: &Tz, day: NaiveDate) -> Option<i64> {
        let naive = day.and_hms_opt(0, 0, 0)?;
        match timezone.from_local_datetime(&naive) {
            chrono::LocalResult::Single(value) => Some(value.timestamp()),
            chrono::LocalResult::Ambiguous(earliest, _) => Some(earliest.timestamp()),
            chrono::LocalResult::None => {
                let bumped = naive.checked_add_signed(chrono::Duration::hours(1))?;
                match timezone.from_local_datetime(&bumped) {
                    chrono::LocalResult::Single(value) => Some(value.timestamp()),
                    chrono::LocalResult::Ambiguous(earliest, _) => Some(earliest.timestamp()),
                    chrono::LocalResult::None => None,
                }
            }
        }
    }

    let next = day.succ_opt()?;
    Some((midnight(timezone, day)?, midnight(timezone, next)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{FixedOffset, TimeZone};
    use rusqlite::Connection;
    use std::str::FromStr;

    #[test]
    fn scalar_and_sql_fresh_input_match() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE logs (app_type TEXT, input_tokens INTEGER, cache_read_tokens INTEGER, cache_creation_tokens INTEGER, input_token_semantics INTEGER);",
            )
            .unwrap();
        for row in [
            ("codex", 1000, 300, 200, INPUT_TOKEN_SEMANTICS_TOTAL),
            ("gemini", 900, 400, 0, INPUT_TOKEN_SEMANTICS_LEGACY),
            ("grokbuild", 500, 200, 100, INPUT_TOKEN_SEMANTICS_FRESH),
            ("claude", 100, 1000, 0, INPUT_TOKEN_SEMANTICS_LEGACY),
        ] {
            connection
                .execute(
                    "INSERT INTO logs VALUES (?1,?2,?3,?4,?5)",
                    rusqlite::params![row.0, row.1, row.2, row.3, row.4],
                )
                .unwrap();
        }
        let expression = sql::fresh_input("l");
        let mut statement = connection
            .prepare(&format!("SELECT app_type,input_tokens,cache_read_tokens,cache_creation_tokens,input_token_semantics,{expression} FROM logs l"))
            .unwrap();
        let rows = statement
            .query_map([], |row| {
                let app: String = row.get(0)?;
                let input: i64 = row.get(1)?;
                let read: i64 = row.get(2)?;
                let creation: i64 = row.get(3)?;
                let semantics: i64 = row.get(4)?;
                let sql_value: i64 = row.get(5)?;
                Ok((
                    fresh_input_tokens(&app, semantics, input, read, creation),
                    sql_value,
                ))
            })
            .unwrap();
        for row in rows {
            let (scalar, sql_value) = row.unwrap();
            assert_eq!(scalar, sql_value);
        }
    }

    #[test]
    fn canonical_cost_uses_fresh_input_for_cache_inclusive_apps() {
        let pricing = ModelPricing {
            input_cost_per_million: Decimal::from_str("10").unwrap(),
            output_cost_per_million: Decimal::ZERO,
            cache_read_cost_per_million: Decimal::from_str("1").unwrap(),
            cache_creation_cost_per_million: Decimal::ZERO,
        };
        let cost = calculate_cost(
            "codex",
            TokenCounts {
                input_tokens: 1000,
                cache_read_tokens: 600,
                ..TokenCounts::default()
            },
            &pricing,
            Decimal::ONE,
        );
        assert_eq!(cost.input_cost, Decimal::from_str("0.004").unwrap());
        assert_eq!(cost.total_cost, Decimal::from_str("0.0046").unwrap());
    }

    #[test]
    fn rollups_only_cover_complete_local_days() {
        let timezone = FixedOffset::east_opt(8 * 3600).unwrap();
        let start = timezone
            .with_ymd_and_hms(2026, 8, 1, 12, 0, 0)
            .unwrap()
            .timestamp();
        let end = timezone
            .with_ymd_and_hms(2026, 8, 3, 23, 59, 59)
            .unwrap()
            .timestamp();
        let bounds = compute_rollup_date_bounds(&timezone, Some(start), Some(end)).unwrap();
        assert_eq!(bounds.start.as_deref(), Some("2026-08-02"));
        assert_eq!(bounds.end.as_deref(), Some("2026-08-03"));
        assert!(!bounds.is_empty);
    }

    #[test]
    fn display_normalization_is_stable() {
        assert_eq!(folded_app_type("claude-desktop"), "claude");
        assert_eq!(folded_app_type("codex"), "codex");
        assert_eq!(effective_model("alias", Some("priced")), "priced");
        assert_eq!(effective_model("alias", Some("")), "alias");
    }
}

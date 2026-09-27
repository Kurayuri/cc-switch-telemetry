//! Fixed Fast tariff policy pinned to cc-switch 87d966b7.
use crate::CostBreakdown;
use rust_decimal::Decimal;
fn model_matches(model: &str, base: &str) -> bool {
    model == base
        || model.strip_prefix(base).is_some_and(|suffix| {
            suffix.len() == 11
                && suffix.starts_with('-')
                && chrono::NaiveDate::parse_from_str(&suffix[1..], "%Y-%m-%d").is_ok()
        })
}

pub fn factors(model: &str, tier: Option<&str>, _input_tokens: u64) -> Option<(Decimal, Decimal)> {
    if !matches!(tier, Some("fast" | "priority")) {
        return None;
    }
    // Exact model names and official dated snapshots only; never guess reseller aliases.
    let matches = |base: &str| model_matches(model, base);
    if ["claude-opus-5", "claude-opus-4-8"].contains(&model) {
        return (tier == Some("fast")).then_some((Decimal::from(2), Decimal::from(2)));
    }
    if [
        "gpt-6-astra",
        "gpt-5.6-sol",
        "gpt-5.6-terra",
        "gpt-5.6-luna",
    ]
    .iter()
    .any(|m| matches(m))
    {
        return Some((Decimal::from(2), Decimal::from(2)));
    }
    if matches("gpt-5.5") {
        return Some((Decimal::new(25, 1), Decimal::new(25, 1)));
    }
    if matches("gpt-5.4")
        || ["gpt-5.4-mini", "gpt-5.3-codex", "gpt-5.2", "gpt-5.1"]
            .iter()
            .any(|m| matches(m))
    {
        return Some((Decimal::from(2), Decimal::from(2)));
    }
    None
}

pub fn apply(cost: &mut CostBreakdown, factors: (Decimal, Decimal), multiplier: Decimal) {
    cost.input_cost *= factors.0;
    cost.cache_read_cost *= factors.0;
    cost.cache_creation_cost *= factors.0;
    cost.output_cost *= factors.1;
    cost.total_cost =
        (cost.input_cost + cost.output_cost + cost.cache_read_cost + cost.cache_creation_cost)
            * multiplier;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tariffs_are_exact_and_do_not_add_context_surcharges() {
        for model in [
            "gpt-6-astra",
            "gpt-5.6-sol",
            "gpt-5.4-mini",
            "gpt-5.3-codex",
        ] {
            assert_eq!(
                factors(model, Some("priority"), 1_000_000).unwrap().0,
                Decimal::from(2)
            );
        }
        assert_eq!(
            factors("gpt-5.5-2026-09-26", Some("fast"), 1).unwrap().0,
            Decimal::new(25, 1)
        );
        for model in ["gpt-5.5-reseller", "gpt-5.5-2026-99-99", "gpt-5.5-custom"] {
            assert!(factors(model, Some("fast"), 1).is_none());
        }
        assert!(factors("claude-opus-5", Some("priority"), 1).is_none());
        assert_eq!(
            factors("claude-opus-5", Some("fast"), 1).unwrap().0,
            Decimal::from(2)
        );
        assert!(factors("gpt-5.5", Some("default"), 1).is_none());
    }
}

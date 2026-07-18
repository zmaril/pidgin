//! Cost math ported from pi-ai's `calculateCost` (`packages/ai/src/models.ts:639`).

use crate::types::{Model, ModelCostRates, Usage, UsageCost};

/// Compute the cost breakdown for a `usage` against a `model`'s pricing.
///
/// A direct port of pi's `calculateCost` (`models.ts:639-659`), preserving its
/// arithmetic exactly:
///
/// - The applicable rate tier is the highest `inputTokensAbove` threshold that
///   the request's total input tokens (`input + cacheRead + cacheWrite`) exceed;
///   the base rates apply when no tier matches.
/// - Anthropic charges 2x base input for 1-hour cache writes, so the
///   `cacheWrite1h` subset is billed at `input * 2` and the remaining
///   (`cacheWrite - cacheWrite1h`) short writes at the tier's `cacheWrite` rate.
///
/// Unlike the TypeScript original, this does not mutate `usage`; it returns a
/// fresh [`UsageCost`].
pub fn calculate_cost<C>(model: &Model<C>, usage: &Usage) -> UsageCost {
    let input_tokens = usage.input + usage.cache_read + usage.cache_write;

    let mut rates: ModelCostRates = model.cost.base_rates();
    let mut matched_threshold: Option<u64> = None;
    if let Some(tiers) = &model.cost.tiers {
        for tier in tiers {
            let above = tier.input_tokens_above;
            if input_tokens > above && matched_threshold.is_none_or(|m| above > m) {
                rates = tier.rates();
                matched_threshold = Some(above);
            }
        }
    }

    // Anthropic charges 2x base input for 1h cache writes.
    let long_write = usage.cache_write_1h.unwrap_or(0) as f64;
    let short_write = usage.cache_write as f64 - long_write;

    let input = (rates.input / 1_000_000.0) * usage.input as f64;
    let output = (rates.output / 1_000_000.0) * usage.output as f64;
    let cache_read = (rates.cache_read / 1_000_000.0) * usage.cache_read as f64;
    let cache_write =
        (rates.cache_write * short_write + rates.input * 2.0 * long_write) / 1_000_000.0;
    let total = input + output + cache_read + cache_write;

    UsageCost {
        input,
        output,
        cache_read,
        cache_write,
        total,
    }
}

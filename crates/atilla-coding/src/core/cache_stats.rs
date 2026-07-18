//! Prompt-cache waste accounting across a session.
//!
//! Ported from pi's `core/cache-stats.ts`. Walks a session's entries and,
//! for each assistant turn, charges any prompt tokens that were present in the
//! previous turn's prompt but re-billed instead of read from cache. Model
//! switches count as misses; compaction/branch-summary resets do not (the
//! context legitimately changed).
//!
//! NOTE (seams): pi imports `AssistantMessage` (from `pi-ai`) and
//! `SessionEntry` (from `session-manager`). Both are unported, so this module
//! defines the minimal mirrors it reads — [`AssistantMessage`], [`Usage`],
//! [`Cost`], [`SessionEntry`], [`Message`]. They collapse into re-exports once
//! those modules land. Where pi keys its miss map by `AssistantMessage`
//! reference identity, this port keys by the entry's index in the input slice.

use std::collections::HashMap;

/// Prompt-cache TTL. Idle gaps longer than this are worth surfacing as the
/// likely cause of a miss. Anthropic's default cache TTL is 5 minutes.
pub const CACHE_TTL_MS: i64 = 5 * 60 * 1000;

/// Per-turn misses at or below this are cache-breakpoint granularity noise.
const NOISE_FLOOR_TOKENS: i64 = 1024;

/// Per-bucket cost breakdown for one message, in dollars. Mirrors pi's
/// `usage.cost`.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Cost {
    /// Dollars billed for uncached input tokens.
    pub input: f64,
    /// Dollars billed for output tokens.
    pub output: f64,
    /// Dollars billed for cache-read tokens.
    pub cache_read: f64,
    /// Dollars billed for cache-write tokens (includes the write premium).
    pub cache_write: f64,
    /// Total dollars billed.
    pub total: f64,
}

/// Token accounting for one assistant message. Mirrors pi's `usage`.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Usage {
    /// Uncached input tokens.
    pub input: i64,
    /// Output tokens.
    pub output: i64,
    /// Tokens read from the prompt cache.
    pub cache_read: i64,
    /// Tokens written to the prompt cache.
    pub cache_write: i64,
    /// Provider-reported total tokens.
    pub total_tokens: i64,
    /// Per-bucket cost breakdown.
    pub cost: Cost,
}

/// The fields of an assistant message this module reads. Minimal mirror of
/// pi-ai's `AssistantMessage`.
#[derive(Debug, Clone, PartialEq)]
pub struct AssistantMessage {
    /// Provider id (e.g. `"anthropic"`).
    pub provider: String,
    /// Model id.
    pub model: String,
    /// Token/cost usage for the turn.
    pub usage: Usage,
    /// Epoch-millis timestamp of the request.
    pub timestamp: i64,
}

impl AssistantMessage {
    fn model_key(&self) -> String {
        format!("{}/{}", self.provider, self.model)
    }
}

/// A session message. Only assistant messages participate in cache accounting;
/// all other roles are opaque here. Minimal mirror of the message roles in
/// pi's `SessionEntry`.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// An assistant turn.
    Assistant(AssistantMessage),
    /// Any other role (user, tool result, …) — ignored by the scan.
    Other,
}

/// One entry in a session log. Minimal mirror of pi's `SessionEntry`.
#[derive(Debug, Clone, PartialEq)]
pub enum SessionEntry {
    /// A conversation message.
    Message(Message),
    /// A compaction reset: the next turn's prompt is new content.
    Compaction,
    /// A branch-summary reset: likewise new content.
    BranchSummary,
}

/// Minimal pricing lookup, satisfied by the model runtime. `cache_read` is in
/// dollars per million tokens. Mirrors pi's `ModelPriceSource`.
pub trait ModelPriceSource {
    /// Look up the cache-read price for a `(provider, model_id)` pair.
    fn get_model(&self, provider: &str, model_id: &str) -> Option<ModelPrice>;
}

/// A model's cache-read price, in dollars per million tokens.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelPrice {
    /// Cache-read price, dollars per million tokens.
    pub cache_read: f64,
}

/// A counted cache miss on a single assistant message.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CacheMiss {
    /// Prompt tokens in the previous turn's prompt but not read from cache.
    pub missed_tokens: i64,
    /// Extra dollars paid vs. a full cache hit; 0 when pricing is unknown.
    pub missed_cost: f64,
    /// Milliseconds since the previous request (which last refreshed the cache).
    pub idle_ms: i64,
    /// True when the model changed relative to the previous request.
    pub model_changed: bool,
}

/// Cumulative cache-waste totals across a scan.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CacheWasteTotals {
    /// Total missed tokens.
    pub missed_tokens: i64,
    /// Total extra dollars paid.
    pub missed_cost: f64,
    /// Number of counted misses (turns above the noise floor).
    pub miss_count: usize,
}

/// The last request seen by the scan; everything in its prompt should be cached.
#[derive(Debug, Clone)]
struct PreviousRequest {
    prompt_tokens: i64,
    model_key: String,
    timestamp: i64,
    /// Sticky: some earlier request in this segment reported cache activity.
    reported_cache: bool,
}

/// Compute the cache miss for one assistant message relative to the previous
/// request. Returns `None` when nothing is counted: first turn, after a reset,
/// no cache activity ever reported, or a miss below the noise floor.
fn detect_miss(
    prev: Option<&PreviousRequest>,
    message: &AssistantMessage,
    models: &dyn ModelPriceSource,
) -> Option<CacheMiss> {
    let usage = &message.usage;
    let prompt_tokens = usage.input + usage.cache_read + usage.cache_write;
    let prev = prev?;
    // A zero-cache turn only counts when cache activity was reported before: on
    // cache-read-only providers that is a total miss, while on providers that
    // never report caching it means nothing.
    if prompt_tokens <= 0 || (usage.cache_read + usage.cache_write == 0 && !prev.reported_cache) {
        return None;
    }

    let missed_tokens = prev.prompt_tokens.min(prompt_tokens) - usage.cache_read;
    if missed_tokens <= NOISE_FLOOR_TOKENS {
        return None;
    }

    // Extra cost = missed tokens billed at the actual paid rate (input/cacheWrite,
    // incl. write premium) instead of the cache-read rate.
    let paid_tokens = usage.input + usage.cache_write;
    let paid_per_token = if paid_tokens > 0 {
        (usage.cost.input + usage.cost.cache_write) / paid_tokens as f64
    } else {
        0.0
    };
    let read_per_token = if usage.cache_read > 0 {
        usage.cost.cache_read / usage.cache_read as f64
    } else {
        models
            .get_model(&message.provider, &message.model)
            .map_or(0.0, |m| m.cache_read)
            / 1_000_000.0
    };

    Some(CacheMiss {
        missed_tokens,
        missed_cost: missed_tokens as f64 * (paid_per_token - read_per_token).max(0.0),
        idle_ms: (message.timestamp - prev.timestamp).max(0),
        model_changed: message.model_key() != prev.model_key,
    })
}

/// Fold an assistant message into a [`PreviousRequest`], carrying forward the
/// sticky `reported_cache` flag. Returns `None` for zero-prompt turns, matching
/// pi's `asPreviousRequest` (the caller keeps the old `prev` in that case).
fn as_previous_request(
    message: &AssistantMessage,
    reported_cache: bool,
) -> Option<PreviousRequest> {
    let usage = &message.usage;
    let prompt_tokens = usage.input + usage.cache_read + usage.cache_write;
    if prompt_tokens <= 0 {
        return None;
    }
    Some(PreviousRequest {
        prompt_tokens,
        model_key: message.model_key(),
        timestamp: message.timestamp,
        reported_cache: reported_cache || usage.cache_read + usage.cache_write > 0,
    })
}

struct ScanResult {
    prev: Option<PreviousRequest>,
    totals: CacheWasteTotals,
    /// Counted misses, keyed by the entry's index in the scanned slice.
    misses: HashMap<usize, CacheMiss>,
}

fn scan(entries: &[SessionEntry], models: &dyn ModelPriceSource) -> ScanResult {
    let mut prev: Option<PreviousRequest> = None;
    let mut totals = CacheWasteTotals::default();
    let mut misses = HashMap::new();

    for (index, entry) in entries.iter().enumerate() {
        match entry {
            SessionEntry::Compaction | SessionEntry::BranchSummary => {
                // The context legitimately changed; the next turn's prompt is
                // new content, not re-billed content. Model switches are NOT
                // exempt: they re-bill the full prompt and should be counted.
                prev = None;
            }
            SessionEntry::Message(Message::Assistant(message)) => {
                if let Some(miss) = detect_miss(prev.as_ref(), message, models) {
                    totals.missed_tokens += miss.missed_tokens;
                    totals.missed_cost += miss.missed_cost;
                    totals.miss_count += 1;
                    misses.insert(index, miss);
                }
                let reported = prev.as_ref().is_some_and(|p| p.reported_cache);
                if let Some(next) = as_previous_request(message, reported) {
                    prev = Some(next);
                }
            }
            SessionEntry::Message(Message::Other) => {}
        }
    }

    ScanResult {
        prev,
        totals,
        misses,
    }
}

/// Cumulative cache waste across a session: prompt tokens that should have been
/// cache reads but were re-billed. Port of `computeCacheWaste`.
pub fn compute_cache_waste(
    entries: &[SessionEntry],
    models: &dyn ModelPriceSource,
) -> CacheWasteTotals {
    scan(entries, models).totals
}

/// All counted cache misses across a session, keyed by the scanned entry index
/// that paid for them. Port of `collectCacheMisses` (pi keys by message
/// reference; see the module NOTE).
pub fn collect_cache_misses(
    entries: &[SessionEntry],
    models: &dyn ModelPriceSource,
) -> HashMap<usize, CacheMiss> {
    scan(entries, models).misses
}

/// Detect a cache miss on a just-completed assistant message. `entries` must
/// not yet contain `message` (message_end fires before persistence). Port of
/// `detectCacheMiss`.
pub fn detect_cache_miss(
    entries: &[SessionEntry],
    message: &AssistantMessage,
    models: &dyn ModelPriceSource,
) -> Option<CacheMiss> {
    detect_miss(scan(entries, models).prev.as_ref(), message, models)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fixed price source: $0.30/M cache-read fallback on full-miss turns.
    struct Models;
    impl ModelPriceSource for Models {
        fn get_model(&self, _provider: &str, _model_id: &str) -> Option<ModelPrice> {
            Some(ModelPrice { cache_read: 0.3 })
        }
    }

    /// Builder for an assistant message, mirroring the test's `assistant()`.
    #[derive(Clone)]
    struct AssistantOpts {
        input: i64,
        cache_read: i64,
        cache_write: i64,
        cost: Cost,
        model: &'static str,
        timestamp: i64,
    }

    fn base_opts() -> AssistantOpts {
        AssistantOpts {
            input: 0,
            cache_read: 0,
            cache_write: 0,
            cost: Cost::default(),
            model: "test-model",
            timestamp: 0,
        }
    }

    fn assistant(opts: AssistantOpts) -> AssistantMessage {
        AssistantMessage {
            provider: "test".to_string(),
            model: opts.model.to_string(),
            usage: Usage {
                input: opts.input,
                output: 10,
                cache_read: opts.cache_read,
                cache_write: opts.cache_write,
                total_tokens: 0,
                cost: opts.cost,
            },
            timestamp: opts.timestamp,
        }
    }

    fn entry(message: AssistantMessage) -> SessionEntry {
        SessionEntry::Message(Message::Assistant(message))
    }

    // Turn 1: fresh 100k cache write at $3.75/M.
    fn turn1() -> AssistantMessage {
        assistant(AssistantOpts {
            cache_write: 100_000,
            cost: Cost {
                cache_write: 0.375,
                ..Cost::default()
            },
            timestamp: 0,
            ..base_opts()
        })
    }

    // Turn 2: healthy, everything read back at $0.30/M.
    fn turn2() -> AssistantMessage {
        assistant(AssistantOpts {
            cache_read: 100_000,
            cache_write: 5_000,
            cost: Cost {
                cache_read: 0.03,
                cache_write: 0.019,
                ..Cost::default()
            },
            timestamp: 60_000,
            ..base_opts()
        })
    }

    #[test]
    fn accumulates_missed_tokens_and_cost_across_turns() {
        // Turn 3: full miss, previous 105k prompt re-billed at $3.75/M write.
        let turn3 = assistant(AssistantOpts {
            cache_write: 110_000,
            cost: Cost {
                cache_write: 0.4125,
                ..Cost::default()
            },
            timestamp: 120_000,
            ..base_opts()
        });
        let totals = compute_cache_waste(&[entry(turn1()), entry(turn2()), entry(turn3)], &Models);
        assert_eq!(totals.missed_tokens, 105_000);
        assert!((totals.missed_cost - 0.36225).abs() < 1e-5, "{totals:?}");
    }

    #[test]
    fn counts_nothing_for_healthy_sessions() {
        let totals = compute_cache_waste(&[entry(turn1()), entry(turn2())], &Models);
        assert_eq!(totals.missed_tokens, 0);
        assert_eq!(totals.missed_cost, 0.0);
    }

    #[test]
    fn skips_the_turn_after_a_compaction_reset() {
        let after_reset = assistant(AssistantOpts {
            cache_write: 20_000,
            cost: Cost {
                cache_write: 0.075,
                ..Cost::default()
            },
            ..base_opts()
        });
        let totals = compute_cache_waste(
            &[entry(turn1()), SessionEntry::Compaction, entry(after_reset)],
            &Models,
        );
        assert_eq!(totals.missed_tokens, 0);
    }

    #[test]
    fn counts_misses_caused_by_model_switches() {
        let other = assistant(AssistantOpts {
            cache_write: 100_000,
            cost: Cost {
                cache_write: 0.375,
                ..Cost::default()
            },
            model: "other-model",
            ..base_opts()
        });
        let totals = compute_cache_waste(&[entry(turn1()), entry(other)], &Models);
        assert_eq!(totals.missed_tokens, 100_000);
        assert_eq!(totals.miss_count, 1);
    }

    #[test]
    fn skips_providers_that_report_no_cache_activity() {
        let a = assistant(AssistantOpts {
            input: 100_000,
            ..base_opts()
        });
        let b = assistant(AssistantOpts {
            input: 110_000,
            ..base_opts()
        });
        let totals = compute_cache_waste(&[entry(a), entry(b)], &Models);
        assert_eq!(totals.missed_tokens, 0);
    }

    #[test]
    fn maps_counted_misses_to_their_entry_index() {
        let miss_turn = assistant(AssistantOpts {
            cache_write: 110_000,
            cost: Cost {
                cache_write: 0.4125,
                ..Cost::default()
            },
            timestamp: 120_000,
            ..base_opts()
        });
        let misses =
            collect_cache_misses(&[entry(turn1()), entry(turn2()), entry(miss_turn)], &Models);
        assert_eq!(misses.len(), 1);
        // The miss belongs to the third entry (index 2).
        assert_eq!(misses.get(&2).unwrap().missed_tokens, 105_000);
    }

    #[test]
    fn detects_a_miss_on_a_just_completed_message_with_idle_time() {
        let miss_message = assistant(AssistantOpts {
            cache_write: 110_000,
            cost: Cost {
                cache_write: 0.4125,
                ..Cost::default()
            },
            timestamp: 600_000,
            ..base_opts()
        });
        let miss = detect_cache_miss(&[entry(turn1()), entry(turn2())], &miss_message, &Models)
            .expect("miss");
        assert_eq!(miss.missed_tokens, 105_000);
        assert!((miss.missed_cost - 0.36225).abs() < 1e-5, "{miss:?}");
        // 600s - 60s since the previous request.
        assert_eq!(miss.idle_ms, 540_000);
        assert!(!miss.model_changed);
    }

    #[test]
    fn flags_model_switches_on_detected_misses() {
        let other = assistant(AssistantOpts {
            cache_write: 110_000,
            cost: Cost {
                cache_write: 0.4125,
                ..Cost::default()
            },
            model: "other-model",
            timestamp: 120_000,
            ..base_opts()
        });
        let miss =
            detect_cache_miss(&[entry(turn1()), entry(turn2())], &other, &Models).expect("miss");
        assert_eq!(miss.missed_tokens, 105_000);
        assert!(miss.model_changed);
    }

    #[test]
    fn returns_none_for_healthy_turns() {
        let healthy = assistant(AssistantOpts {
            cache_read: 105_000,
            cache_write: 2_000,
            cost: Cost {
                cache_read: 0.0315,
                cache_write: 0.0075,
                ..Cost::default()
            },
            timestamp: 120_000,
            ..base_opts()
        });
        assert!(detect_cache_miss(&[entry(turn1()), entry(turn2())], &healthy, &Models).is_none());
    }

    #[test]
    fn returns_none_for_the_first_turn_of_a_session() {
        assert!(detect_cache_miss(&[], &turn1(), &Models).is_none());
    }
}

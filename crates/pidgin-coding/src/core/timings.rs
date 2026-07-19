//! Startup profiling instrumentation.
//!
//! Ported from pi's `core/timings.ts`. Upstream keys off a `PI_TIMING=1`
//! environment gate and `Date.now()` module-global state; here the state is an
//! explicit [`Timings`] value and the clock is injected as a `now_ms`
//! parameter so the accounting is deterministic and unit-testable.
//!
//! NOTE (seam): pi reads the wall clock via `Date.now()` inside `reset`/`time`.
//! Callers pass the current epoch-millis instead (e.g. from `SystemTime`), and
//! the `enabled` gate is set at construction rather than read from the
//! environment, keeping this module free of ambient I/O.

use std::collections::BTreeMap;

/// Which timing namespace an entry belongs to. Mirrors pi's `TimingLabel`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Namespace {
    /// The main startup path.
    Main,
    /// Extension loading.
    Extensions,
}

impl Namespace {
    /// Title used when rendering the namespace.
    fn title(self) -> &'static str {
        match self {
            Namespace::Main => "main",
            Namespace::Extensions => "extensions",
        }
    }
}

/// One recorded measurement: a label and the milliseconds since the previous
/// mark in its namespace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Timing {
    /// Label supplied at the call site.
    pub label: String,
    /// Milliseconds elapsed since the previous mark.
    pub ms: i64,
}

#[derive(Debug, Clone)]
struct NamespaceState {
    timings: Vec<Timing>,
    last_time: i64,
}

/// Accumulates per-namespace startup timings. Construct with
/// [`Timings::new`]; when `enabled` is false every operation is a no-op,
/// mirroring pi's `PI_TIMING` gate.
#[derive(Debug, Clone)]
pub struct Timings {
    enabled: bool,
    namespaces: BTreeMap<Namespace, NamespaceState>,
}

impl Timings {
    /// Create a timings accumulator. `enabled` mirrors `PI_TIMING === "1"`.
    pub fn new(enabled: bool) -> Self {
        Timings {
            enabled,
            namespaces: BTreeMap::new(),
        }
    }

    /// Whether recording is active.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Reset (or initialize) a namespace, anchoring its clock at `now_ms`.
    /// Port of `resetTimings`.
    pub fn reset(&mut self, namespace: Namespace, now_ms: i64) {
        if !self.enabled {
            return;
        }
        self.namespaces.insert(
            namespace,
            NamespaceState {
                timings: Vec::new(),
                last_time: now_ms,
            },
        );
    }

    /// Record `label` in `namespace`, measuring the gap since the previous
    /// mark. Auto-initializes the namespace at `now_ms` on first use. Port of
    /// `time`.
    pub fn time(&mut self, label: impl Into<String>, namespace: Namespace, now_ms: i64) {
        if !self.enabled {
            return;
        }
        let state = self
            .namespaces
            .entry(namespace)
            .or_insert_with(|| NamespaceState {
                timings: Vec::new(),
                last_time: now_ms,
            });
        state.timings.push(Timing {
            label: label.into(),
            ms: now_ms - state.last_time,
        });
        state.last_time = now_ms;
    }

    /// The raw recorded timings for a namespace, if any were captured.
    pub fn timings(&self, namespace: Namespace) -> &[Timing] {
        self.namespaces
            .get(&namespace)
            .map_or(&[], |s| s.timings.as_slice())
    }

    /// Render all namespaces to the lines pi's `printTimings` writes to stderr
    /// (returned instead of printed). Negative-duration marks are filtered, a
    /// `TOTAL` line sums the rest, and empty groups are omitted — matching
    /// `printTimingGroup`.
    pub fn render(&self) -> Vec<String> {
        let mut out = Vec::new();
        if !self.enabled {
            return out;
        }
        for (namespace, state) in &self.namespaces {
            let printable: Vec<&Timing> = state.timings.iter().filter(|t| t.ms >= 0).collect();
            if printable.is_empty() {
                continue;
            }
            let title = format!("Startup Timings: {}", namespace.title());
            out.push(format!("\n--- {title} ---"));
            for t in &printable {
                out.push(format!("  {}: {}ms", t.label, t.ms));
            }
            let total: i64 = printable.iter().map(|t| t.ms).sum();
            out.push(format!("  TOTAL: {total}ms"));
            out.push(format!("{}\n", "-".repeat(title.len() + 8)));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_records_nothing() {
        let mut t = Timings::new(false);
        t.reset(Namespace::Main, 0);
        t.time("phase", Namespace::Main, 100);
        assert!(t.timings(Namespace::Main).is_empty());
        assert!(t.render().is_empty());
    }

    #[test]
    fn measures_gaps_between_marks() {
        let mut t = Timings::new(true);
        t.reset(Namespace::Main, 1_000);
        t.time("a", Namespace::Main, 1_050);
        t.time("b", Namespace::Main, 1_200);
        assert_eq!(
            t.timings(Namespace::Main),
            &[
                Timing {
                    label: "a".to_string(),
                    ms: 50
                },
                Timing {
                    label: "b".to_string(),
                    ms: 150
                },
            ]
        );
    }

    #[test]
    fn time_auto_initializes_namespace() {
        let mut t = Timings::new(true);
        // No explicit reset; first mark anchors the clock, so ms == 0.
        t.time("first", Namespace::Extensions, 500);
        t.time("second", Namespace::Extensions, 700);
        assert_eq!(
            t.timings(Namespace::Extensions),
            &[
                Timing {
                    label: "first".to_string(),
                    ms: 0
                },
                Timing {
                    label: "second".to_string(),
                    ms: 200
                },
            ]
        );
    }

    #[test]
    fn render_filters_negative_and_totals_the_rest() {
        let mut t = Timings::new(true);
        t.reset(Namespace::Main, 100);
        t.time("ok", Namespace::Main, 130); // +30
        t.time("clock-skew", Namespace::Main, 120); // -10, filtered
        t.time("ok2", Namespace::Main, 200); // +80
        let lines = t.render();
        let joined = lines.join("\n");
        assert!(joined.contains("ok: 30ms"), "{joined}");
        assert!(joined.contains("ok2: 80ms"), "{joined}");
        assert!(!joined.contains("clock-skew"), "{joined}");
        assert!(joined.contains("TOTAL: 110ms"), "{joined}");
    }

    #[test]
    fn render_omits_empty_groups() {
        let mut t = Timings::new(true);
        t.reset(Namespace::Main, 0);
        // Only a negative mark -> nothing printable -> group omitted.
        t.time("skew", Namespace::Main, -5);
        assert!(t.render().is_empty());
    }
}

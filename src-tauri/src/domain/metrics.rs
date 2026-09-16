//! Raw counters and the three frozen KPIs.
//!
//! The formulas are ported verbatim from Demo Studio's `derive/formulas.py`
//! (`avg_context_per_call`, `rewrite_ratio`, `batching_ratio`) so the two products agree on
//! every number. Studio freezes them behind an ADR; Pigeon inherits both the definitions and
//! that constraint. A zero denominator is `None` — *undefined*, never zero.

use serde::{Deserialize, Serialize};

use crate::api::errors::EngineError;

/// The raw six, plus the three the detail card adds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Metrics {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub api_calls: u64,
    pub tool_calls: u64,
    pub user_turns: u64,
    pub duration_ms: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    /// The engine's *own* dollar figure where it states one (OpenCode). Pigeon never invents a
    /// price: Claude and Codex are subscription logins here, so a per-token cost would be fiction.
    pub provider_cost_usd: Option<f64>,
}

impl Metrics {
    pub fn kpis(&self) -> Kpis {
        Kpis {
            context_per_call: ratio(self.cache_read, self.api_calls),
            rewrite_ratio: ratio(self.cache_write, self.cache_read),
            batching_ratio: ratio(self.tool_calls, self.api_calls),
        }
    }
}

/// `cache_read ÷ api_calls`, `cache_write ÷ cache_read`, `tool_calls ÷ api_calls`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Kpis {
    pub context_per_call: Option<f64>,
    pub rewrite_ratio: Option<f64>,
    pub batching_ratio: Option<f64>,
}

/// The one division in the product. A zero denominator is undefined, not zero.
pub fn ratio(numerator: u64, denominator: u64) -> Option<f64> {
    if denominator == 0 {
        None
    } else {
        Some(numerator as f64 / denominator as f64)
    }
}

/// How a session's counters were obtained. Diagnostic only; it changes no number.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricBasis {
    /// Claude: a full-file fold, deduped by `message.id` with an elementwise MAX per key.
    Fold,
    /// Codex: the last cumulative `token_count` event in the session's newest rollout.
    Deltas,
    /// OpenCode: columns already on the `session` row.
    Columns,
}

/// Pending, ready and unavailable are three different things, and none of them is zero.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum MetricState {
    /// Not counted yet. The row renders "counting", never a blank and never a 0.
    #[default]
    Pending,
    Ready {
        metrics: Metrics,
        basis: MetricBasis,
        counted_at_ms: i64,
    },
    /// The count could not be produced. Renders as a stated absence with its reason.
    Unavailable { error: EngineError },
}

impl MetricState {
    pub fn metrics(&self) -> Option<&Metrics> {
        match self {
            MetricState::Ready { metrics, .. } => Some(metrics),
            _ => None,
        }
    }
    pub fn is_ready(&self) -> bool {
        matches!(self, MetricState::Ready { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_zero_denominator_is_undefined_not_zero() {
        let empty = Metrics::default();
        let kpis = empty.kpis();
        assert_eq!(kpis.context_per_call, None);
        assert_eq!(kpis.rewrite_ratio, None);
        assert_eq!(kpis.batching_ratio, None);
    }

    #[test]
    fn the_three_formulas_match_the_frozen_definitions() {
        let m = Metrics {
            cache_read: 1_000,
            cache_write: 250,
            api_calls: 4,
            tool_calls: 10,
            ..Default::default()
        };
        let kpis = m.kpis();
        assert_eq!(kpis.context_per_call, Some(250.0)); // cache_read / api_calls
        assert_eq!(kpis.rewrite_ratio, Some(0.25)); // cache_write / cache_read
        assert_eq!(kpis.batching_ratio, Some(2.5)); // tool_calls / api_calls
    }

    #[test]
    fn a_real_zero_is_not_an_absence() {
        // Zero tool calls over four API calls is a real, meaningful 0.0 — not None.
        let m = Metrics {
            api_calls: 4,
            tool_calls: 0,
            ..Default::default()
        };
        assert_eq!(m.kpis().batching_ratio, Some(0.0));
    }
}

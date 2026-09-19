//! Project rollup: sum the counters, then recompute the KPIs from the sums.
//!
//! Averaging session KPIs would weight a 3-call session the same as a 300-call one. The rule is
//! sum-then-recompute, and `project_kpis_are_recomputed_not_averaged` pins it.

use super::{Kpis, MetricState, Metrics, ProjectKey, ProviderId, SessionStatus};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// How many sessions in this project are in each display state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusCounts {
    pub running: u32,
    pub needs_you: u32,
    pub finished: u32,
    pub unknown: u32,
}

impl StatusCounts {
    pub fn add(&mut self, status: Option<SessionStatus>) {
        match status {
            Some(SessionStatus::Running | SessionStatus::Delegating) => self.running += 1,
            Some(SessionStatus::NeedsYou) => self.needs_you += 1,
            Some(SessionStatus::Finished) => self.finished += 1,
            Some(SessionStatus::Unknown) => self.unknown += 1,
            None => {}
        }
    }
    pub fn any_live(&self) -> bool {
        self.running > 0 || self.needs_you > 0 || self.unknown > 0
    }
}

/// Summed counters. Every field is additive by construction; the KPIs are not, which is why
/// they are not fields here.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MetricsTotals {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub api_calls: u64,
    pub tool_calls: u64,
    pub user_turns: u64,
    pub duration_ms: u64,
    pub reasoning_tokens: u64,
    /// Sums only over rows whose engine states a figure. `cost_rows` says how many that was.
    pub provider_cost_usd: f64,
}

impl MetricsTotals {
    pub fn add(&mut self, m: &Metrics) {
        self.input_tokens += m.input_tokens;
        self.output_tokens += m.output_tokens;
        self.cache_read += m.cache_read;
        self.cache_write += m.cache_write;
        self.api_calls += m.api_calls;
        self.tool_calls += m.tool_calls;
        self.user_turns += m.user_turns;
        self.duration_ms += m.duration_ms.unwrap_or(0);
        self.reasoning_tokens += m.reasoning_tokens.unwrap_or(0);
        self.provider_cost_usd += m.provider_cost_usd.unwrap_or(0.0);
    }

    /// The same three formulas, over the sums.
    pub fn kpis(&self) -> Kpis {
        Kpis {
            context_per_call: super::metrics::ratio(self.cache_read, self.api_calls),
            rewrite_ratio: super::metrics::ratio(self.cache_write, self.cache_read),
            batching_ratio: super::metrics::ratio(self.tool_calls, self.api_calls),
        }
    }
}

/// One working directory's consumption, across every engine that worked in it.
#[derive(Clone, Debug)]
pub struct ProjectSummary {
    pub project: ProjectKey,
    pub project_name: String,
    pub cwd: String,
    pub project_leaf: String,
    /// Sessions in the requested scope.
    pub sessions: u32,
    /// How many of those have landed their metrics. `counted < sessions` renders as partial.
    pub counted: u32,
    pub providers: BTreeMap<ProviderId, u32>,
    pub status_counts: StatusCounts,
    pub totals: MetricsTotals,
    pub kpis: Kpis,
    pub last_active_ms: i64,
    /// How many rows contributed a provider-stated cost. Zero means the cost figure is not shown.
    pub cost_rows: u32,
}

/// Build a summary from the sessions already filtered to the requested scope.
pub fn summarize<'a, I>(project: &ProjectKey, rows: I) -> ProjectSummary
where
    I: IntoIterator<Item = (&'a MetricState, Option<SessionStatus>, ProviderId, i64)>,
{
    let mut summary = ProjectSummary {
        project: project.clone(),
        project_name: project.leaf(),
        cwd: project.display_path(),
        project_leaf: project.leaf(),
        sessions: 0,
        counted: 0,
        providers: BTreeMap::new(),
        status_counts: StatusCounts::default(),
        totals: MetricsTotals::default(),
        kpis: Kpis::default(),
        last_active_ms: 0,
        cost_rows: 0,
    };
    for (state, status, provider, last_active_ms) in rows {
        summary.sessions += 1;
        *summary.providers.entry(provider).or_insert(0) += 1;
        summary.status_counts.add(status);
        summary.last_active_ms = summary.last_active_ms.max(last_active_ms);
        if let MetricState::Ready { metrics, .. } = state {
            summary.counted += 1;
            summary.totals.add(metrics);
            if metrics.provider_cost_usd.is_some() {
                summary.cost_rows += 1;
            }
        }
    }
    summary.kpis = summary.totals.kpis();
    summary
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::MetricBasis;

    fn ready(cache_read: u64, cache_write: u64, api_calls: u64, tool_calls: u64) -> MetricState {
        MetricState::Ready {
            metrics: Metrics {
                cache_read,
                cache_write,
                api_calls,
                tool_calls,
                ..Default::default()
            },
            basis: MetricBasis::Fold,
            counted_at_ms: 0,
        }
    }

    #[test]
    fn project_kpis_are_recomputed_not_averaged() {
        // Two sessions with very different weights. The mean of the row KPIs is 150; the
        // recomputed figure is 1100/6 ≈ 183.3. Only the second is correct.
        let a = ready(100, 0, 1, 0); // context/call 100
        let b = ready(1000, 0, 5, 0); // context/call 200
        let rows = vec![
            (&a, None, ProviderId::ClaudeCode, 10),
            (&b, None, ProviderId::Codex, 20),
        ];
        let summary = summarize(&ProjectKey("/p".into()), rows);
        assert_eq!(summary.totals.cache_read, 1100);
        assert_eq!(summary.totals.api_calls, 6);
        let expected = 1100.0 / 6.0;
        assert!((summary.kpis.context_per_call.expect("defined") - expected).abs() < 1e-9);
        assert_ne!(summary.kpis.context_per_call, Some(150.0));
        assert_eq!(summary.last_active_ms, 20);
        assert_eq!(summary.providers.len(), 2);
    }

    #[test]
    fn counted_lags_sessions_while_a_row_is_pending() {
        let a = ready(10, 0, 1, 0);
        let pending = MetricState::Pending;
        let rows = vec![
            (&a, Some(SessionStatus::Running), ProviderId::ClaudeCode, 5),
            (
                &pending,
                Some(SessionStatus::Finished),
                ProviderId::ClaudeCode,
                7,
            ),
        ];
        let summary = summarize(&ProjectKey("/p".into()), rows);
        assert_eq!(summary.sessions, 2);
        assert_eq!(summary.counted, 1);
        assert_eq!(summary.totals.cache_read, 10);
        assert_eq!(summary.status_counts.running, 1);
        assert_eq!(summary.status_counts.finished, 1);
    }

    #[test]
    fn totals_equal_the_sum_of_the_rows() {
        let a = ready(3, 1, 2, 4);
        let b = ready(5, 2, 3, 6);
        let rows = vec![
            (&a, None, ProviderId::OpenCode, 1),
            (&b, None, ProviderId::OpenCode, 2),
        ];
        let s = summarize(&ProjectKey("/p".into()), rows);
        assert_eq!(s.totals.cache_read, 8);
        assert_eq!(s.totals.cache_write, 3);
        assert_eq!(s.totals.api_calls, 5);
        assert_eq!(s.totals.tool_calls, 10);
        assert_eq!(s.cost_rows, 0);
    }
}

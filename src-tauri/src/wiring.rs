//! Where the engine adapters are plugged into the services.
//!
//! This is the only file that knows all three engines at once. Keeping it small and separate is
//! what makes the adapter contract's acceptance test meaningful: adding or removing an engine
//! should touch `adapters/`, this file, and nothing else.

use std::path::PathBuf;
use std::sync::Arc;

use crate::adapters::{claude, codex, opencode, ProviderAdapter};
use crate::api::errors::{EngineError, ErrorKind};
use crate::app_state::LiveStatus;
use crate::domain::{MetricBasis, Metrics, ProviderId, Session, SessionKey};
use crate::services::metrics::MetricSource;
use crate::services::status::{
    OpenCodeAttribution, OpenCodeAttributor, OpenCodeProcess, StatusReport, StatusService,
    StopOutcome,
};

/// Attributes a live OpenCode process to its session, **exactly** when the installed bridge can
/// say which session the process is working on, and only by the folder otherwise.
///
/// **Why the folder is not enough.** A cwd is a folder, and two `opencode` processes can share one
/// — measured on this Mac 2026-09-18: two live processes in this project, both `cwd=pigeon`, both
/// resolving to the same newest session, which then collapsed into one row and hid the other
/// session entirely. The bridge plugin runs inside each process and records the session ids it sees
/// against its own pid, so the common case is exact. The folder fallback is kept for a machine that
/// has not installed the bridge, but it is **refused when it would be a guess**: if more than one
/// discovered session shares the folder, no process is attributed to any of them rather than all of
/// them being attributed to the newest.
struct HostOpenCodeAttributor {
    adapter: opencode::OpenCodeAdapter,
}

impl HostOpenCodeAttributor {
    fn new() -> Self {
        Self {
            adapter: opencode::OpenCodeAdapter::new(),
        }
    }
}

impl OpenCodeAttributor for HostOpenCodeAttributor {
    fn attribute(
        &self,
        processes: &[OpenCodeProcess],
    ) -> Result<Vec<OpenCodeAttribution>, EngineError> {
        let report = self.adapter.discover_sessions();
        if let Some(problem) = report.problem {
            return Err(problem);
        }
        let known: std::collections::BTreeSet<String> = report
            .sessions
            .iter()
            .map(|session| session.key.sid.clone())
            .collect();
        let bridge = self.adapter.bridge();

        let mut attributed = Vec::new();
        for process in processes {
            // Exact first: the bridge names the session this process is working on.
            let claimed = bridge.claimed_session(process.pid, &known);
            let (sid, how) = match claimed {
                Some(sid) => (sid.to_string(), "the installed bridge names this session"),
                None => {
                    let Some(cwd) = process.cwd.as_ref() else {
                        continue;
                    };
                    let candidates: Vec<&_> = report
                        .sessions
                        .iter()
                        .filter(|candidate| candidate.cwd.as_ref() == Some(cwd))
                        .collect();
                    // One session in the folder is the folder's answer. Two is a coin flip, and
                    // this file does not flip coins: the invariant is "never guess an attribution".
                    let [only] = candidates.as_slice() else {
                        continue;
                    };
                    (
                        only.key.sid.clone(),
                        "it is the only OpenCode session in this process's folder",
                    )
                }
            };
            let Some(session) = report
                .sessions
                .iter()
                .find(|candidate| candidate.key.sid == sid)
            else {
                continue;
            };
            let activity = self.adapter.read_activity(&sid)?;
            let active_subagents = self.adapter.active_subagents(&sid)?;
            let state =
                if activity.state == crate::domain::LiveState::Waiting && active_subagents > 0 {
                    crate::domain::LiveState::Delegating
                } else {
                    activity.state
                };
            let mut evidence = vec![format!("{how}: {sid}")];
            if let Some(cwd) = process.cwd.as_ref() {
                evidence.push(format!("its working directory is {}", cwd.display()));
            }
            if active_subagents > 0 {
                evidence.push(format!(
                    "OpenCode has {active_subagents} active child session(s) delegated from this session"
                ));
            }
            evidence.extend(activity.evidence);
            attributed.push(OpenCodeAttribution {
                pid: Some(process.pid),
                sid,
                state,
                since_ms: activity.since_ms.or(Some(session.last_active_ms)),
                raw_word: activity.raw_word,
                evidence,
                active_subagents,
            });
        }
        Ok(attributed)
    }
}

/// The three adapters, constructed once.
pub fn adapters() -> Vec<Box<dyn ProviderAdapter>> {
    vec![
        Box::new(claude::ClaudeAdapter::new()),
        Box::new(codex::CodexAdapter::new()),
        Box::new(opencode::OpenCodeAdapter::new()),
    ]
}

/// How each engine's counters are actually obtained.
///
/// The three are genuinely different shapes of work and the seam does not pretend otherwise:
/// Claude folds a whole transcript, Codex reads the tail of its newest rollout, and OpenCode's
/// numbers are already columns on a row it has read.
pub struct EngineMetrics {
    opencode: opencode::OpenCodeAdapter,
}

impl EngineMetrics {
    pub fn new() -> Self {
        Self {
            opencode: opencode::OpenCodeAdapter::new(),
        }
    }
}

impl Default for EngineMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl MetricSource for EngineMetrics {
    fn read(&self, session: &Session) -> Result<(Metrics, MetricBasis), EngineError> {
        let provider = session.key.provider_id;
        match provider {
            ProviderId::ClaudeCode => {
                let path = first_source(session, provider)?;
                claude::fold_usage(&path).map(|m| (m, MetricBasis::Fold))
            }
            ProviderId::Codex => {
                if session.source.paths.is_empty() {
                    return Err(EngineError::of(provider, ErrorKind::RootMissing));
                }
                codex::read_metrics(&session.source.paths).map(|m| (m, MetricBasis::Deltas))
            }
            ProviderId::OpenCode => self
                .opencode
                .read_metrics(&session.key.sid)
                .map(|m| (m, MetricBasis::Columns)),
        }
    }
}

/// The transcript a fold should read. A session with no recorded source is an absence, not an
/// empty count — returning zeros here would put a confident 0 on a row we never opened.
fn first_source(session: &Session, provider: ProviderId) -> Result<PathBuf, EngineError> {
    session
        .source
        .paths
        .first()
        .cloned()
        .ok_or_else(|| EngineError::of(provider, ErrorKind::RootMissing))
}

pub fn metric_source() -> Arc<dyn MetricSource> {
    Arc::new(EngineMetrics::new())
}

pub fn status_service() -> StatusService {
    StatusService::new().with_attributor(Arc::new(HostOpenCodeAttributor::new()))
}

/// The real status service behind the [`LiveStatus`] seam.
///
/// A newtype rather than an `impl LiveStatus for StatusService`, so the seam the services are
/// tested against stays a trait the host happens to satisfy rather than the service itself.
pub struct HostStatus(pub Arc<StatusService>);

impl LiveStatus for HostStatus {
    /// One call into the service, so the snapshot and its problems are the same pass. Two calls
    /// here could straddle `SNAPSHOT_TTL` and describe two — see [`LiveStatus::observe`].
    fn observe(&self) -> StatusReport {
        self.0.snapshot()
    }

    fn stop_session(&self, key: &SessionKey) -> Result<StopOutcome, EngineError> {
        self.0.stop_session(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{MetricState, ProjectKey, SessionKey, SourceSummary};

    fn session(provider: ProviderId, paths: Vec<PathBuf>) -> Session {
        Session {
            key: SessionKey::new(provider, "sid"),
            cwd: None,
            project: ProjectKey::none(),
            title: "t".into(),
            name: None,
            git_branch: None,
            first_active_ms: None,
            last_active_ms: 0,
            closed_at_ms: None,
            resumable: false,
            resume_blocked_reason: None,
            metrics: MetricState::Pending,
            source: SourceSummary::many(paths),
            diagnostics: Default::default(),
            signature: None,
        }
    }

    #[test]
    fn all_three_engines_are_registered_exactly_once() {
        let built = adapters();
        let mut seen: Vec<ProviderId> = built.iter().map(|a| a.provider()).collect();
        seen.sort();
        assert_eq!(
            seen,
            vec![
                ProviderId::ClaudeCode,
                ProviderId::Codex,
                ProviderId::OpenCode
            ]
        );
    }

    #[test]
    fn a_session_with_no_recorded_source_is_an_absence_not_a_zero() {
        let source = EngineMetrics::new();
        for provider in [ProviderId::ClaudeCode, ProviderId::Codex] {
            let err = source
                .read(&session(provider, vec![]))
                .expect_err("no source means no count");
            assert_eq!(err.kind, ErrorKind::RootMissing);
        }
    }

    #[test]
    fn each_engine_reports_the_basis_it_actually_used() {
        // The basis changes no number; it is how a reader tells a full fold from a tail read when
        // two engines disagree about a session that ran in both. The earlier version of this test
        // was named for that and asserted only that a missing file errors — it never mentioned
        // `MetricBasis` at all.
        let source = EngineMetrics::new();

        let dir = tempfile::tempdir().expect("tempdir");
        let transcript = dir.path().join("a.jsonl");
        std::fs::write(
            &transcript,
            "{\"type\":\"user\",\"timestamp\":\"2026-09-13T00:00:00.000Z\",\
             \"message\":{\"role\":\"user\",\"content\":\"hi\"}}\n",
        )
        .expect("write");

        let (_, basis) = source
            .read(&session(ProviderId::ClaudeCode, vec![transcript.clone()]))
            .expect("claude folds");
        assert_eq!(
            basis,
            MetricBasis::Fold,
            "Claude reads the whole transcript"
        );

        let rollout = dir.path().join("rollout.jsonl");
        std::fs::write(
            &rollout,
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"other\"}}\n",
        )
        .expect("write");
        let (_, basis) = source
            .read(&session(ProviderId::Codex, vec![rollout]))
            .expect("codex tails");
        assert_eq!(
            basis,
            MetricBasis::Deltas,
            "Codex reads the cumulative tail"
        );
    }
}

//! Wire DTOs. Field names are camelCase and match `docs/contracts/apis.md` exactly.
//!
//! These are separate from the domain objects on purpose. A `PathBuf` becomes a string, a
//! `ProjectKey` becomes its three display forms, a `ResumeBlockedReason` becomes a sentence, and
//! a `SourceSignature` becomes nothing at all — it is cache identity and never crosses the wire.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::api::errors::EngineError;
use crate::domain::{
    AccountStatus, Capacity, CapacityWindow, Identity, Kpis, LiveObservation, LiveState,
    MetricState, Metrics, MetricsTotals, ProjectSummary, ProviderId, ProviderSummary, Session,
    SessionKey, SessionStatus, StatusCounts, StatusSnapshot,
};
use crate::util::repository_name;

/// Whether Pigeon can see Codex waiting on the owner, and whether it already does.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexHooksStatusDto {
    pub installed: bool,
}

/// What one enable attempt did. `trusted` is the field that matters: Codex silently skips an
/// untrusted hook, so `installed && !trusted` means the feature is present and doing nothing.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexHooksReportDto {
    pub installed: bool,
    pub trusted: bool,
    pub message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostInfo {
    pub os: String,
    pub arch: String,
    pub version: String,
}

impl HostInfo {
    pub fn detect() -> Self {
        Self {
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

/// Which slice of the world the View is asking for.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    /// An engine process/terminal is open for this session anywhere on the host right now.
    Live,
    /// No longer live, and closed within the configured window.
    Recent,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum MetricStateDto {
    Pending,
    Ready { value: MetricsDto },
    Unavailable { error: EngineError },
}

impl From<&MetricState> for MetricStateDto {
    fn from(state: &MetricState) -> Self {
        match state {
            MetricState::Pending => MetricStateDto::Pending,
            MetricState::Ready { metrics, .. } => MetricStateDto::Ready {
                value: MetricsDto::from(metrics),
            },
            MetricState::Unavailable { error } => MetricStateDto::Unavailable {
                error: error.clone(),
            },
        }
    }
}

/// [`Metrics`] plus its derived KPIs. The KPIs are computed here, at projection time, and are
/// never stored — that is what keeps one definition of each.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MetricsDto {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub api_calls: u64,
    pub tool_calls: u64,
    pub user_turns: u64,
    pub duration_ms: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub provider_cost_usd: Option<f64>,
    pub kpis: Kpis,
}

impl From<&Metrics> for MetricsDto {
    fn from(m: &Metrics) -> Self {
        Self {
            input_tokens: m.input_tokens,
            output_tokens: m.output_tokens,
            cache_read: m.cache_read,
            cache_write: m.cache_write,
            api_calls: m.api_calls,
            tool_calls: m.tool_calls,
            user_turns: m.user_turns,
            duration_ms: m.duration_ms,
            reasoning_tokens: m.reasoning_tokens,
            provider_cost_usd: m.provider_cost_usd,
            kpis: m.kpis(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionDto {
    pub key: SessionKey,
    pub cwd: Option<String>,
    pub project: String,
    pub project_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository_name: Option<String>,
    pub project_leaf: String,
    pub title: String,
    pub name: Option<String>,
    pub git_branch: Option<String>,
    pub first_active_ms: Option<i64>,
    pub last_active_ms: i64,
    pub closed_at_ms: Option<i64>,
    pub resumable: bool,
    pub resume_blocked_reason: Option<String>,
    pub metrics: MetricStateDto,
    pub source_summary: Option<String>,
    pub diagnostics: DiagnosticsDto,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticsDto {
    pub unknown_types: BTreeMap<String, u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRowDto {
    #[serde(flatten)]
    pub session: SessionDto,
    /// Joined from the live snapshot by the complete [`SessionKey`], never by `sid` alone.
    pub status: Option<SessionStatus>,
}

impl From<&Session> for SessionDto {
    fn from(s: &Session) -> Self {
        Self {
            key: s.key.clone(),
            cwd: s.cwd.as_ref().map(|p| p.display().to_string()),
            project: s.project.0.clone(),
            project_name: s.project.leaf(),
            repository_name: repository_name(s.cwd.as_deref()),
            project_leaf: s.project.leaf(),
            title: s.title.clone(),
            name: s.name.clone(),
            git_branch: s.git_branch.clone(),
            first_active_ms: s.first_active_ms,
            last_active_ms: s.last_active_ms,
            closed_at_ms: s.closed_at_ms,
            resumable: s.resumable,
            resume_blocked_reason: s.resume_blocked_reason.map(|r| r.message().to_string()),
            metrics: MetricStateDto::from(&s.metrics),
            source_summary: s.source.display(),
            diagnostics: DiagnosticsDto {
                unknown_types: s.diagnostics.unknown_types.clone(),
            },
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionListResult {
    pub scope: Scope,
    pub since_ms: Option<i64>,
    pub rows: Vec<SessionRowDto>,
    pub problems: Vec<EngineError>,
    pub generated_at_ms: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSummaryDto {
    pub project: String,
    pub project_name: String,
    pub cwd: String,
    pub project_leaf: String,
    pub sessions: u32,
    pub counted: u32,
    pub providers: BTreeMap<String, u32>,
    pub status_counts: StatusCounts,
    pub totals: MetricsTotals,
    pub kpis: Kpis,
    pub last_active_ms: i64,
    pub cost_rows: u32,
}

impl From<&ProjectSummary> for ProjectSummaryDto {
    fn from(p: &ProjectSummary) -> Self {
        Self {
            project: p.project.0.clone(),
            project_name: p.project_name.clone(),
            cwd: p.cwd.clone(),
            project_leaf: p.project_leaf.clone(),
            sessions: p.sessions,
            counted: p.counted,
            providers: p
                .providers
                .iter()
                .map(|(k, v)| (k.as_str().to_string(), *v))
                .collect(),
            status_counts: p.status_counts,
            totals: p.totals,
            kpis: p.kpis,
            last_active_ms: p.last_active_ms,
            cost_rows: p.cost_rows,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSummaryResult {
    pub scope: Scope,
    pub since_ms: Option<i64>,
    pub projects: Vec<ProjectSummaryDto>,
    pub generated_at_ms: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveSessionStateDto {
    pub key: SessionKey,
    pub process: String,
    pub state: LiveState,
    pub project_leaf: Option<String>,
    pub title: Option<String>,
    pub name: Option<String>,
    pub since_ms: Option<i64>,
    pub raw_word: Option<String>,
    pub evidence: Vec<String>,
    pub pid: Option<u32>,
    pub console_id: Option<String>,
    pub observed_at_ms: i64,
}

impl LiveSessionStateDto {
    pub fn project(
        obs: &LiveObservation,
        project_leaf: Option<String>,
        title: Option<String>,
        name: Option<String>,
    ) -> Self {
        Self {
            key: obs.key.clone(),
            process: match obs.process {
                crate::domain::ProcessPresence::Present => "present".to_string(),
                crate::domain::ProcessPresence::Absent => "absent".to_string(),
            },
            state: obs.state,
            project_leaf,
            title,
            name,
            since_ms: obs.since_ms,
            raw_word: obs.raw_word.clone(),
            evidence: obs.evidence.clone(),
            pid: obs.pid,
            console_id: obs.console_id.clone(),
            observed_at_ms: obs.observed_at_ms,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusCountsDto {
    pub running: u32,
    pub needs_you: u32,
    pub unknown: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusSnapshotDto {
    pub generated_at_ms: i64,
    pub counts: StatusCountsDto,
    pub live: Vec<LiveSessionStateDto>,
}

impl StatusSnapshotDto {
    /// Build the snapshot DTO, decorating each observation with display context looked up by the
    /// complete key.
    pub fn project(
        snapshot: &StatusSnapshot,
        lookup: impl Fn(&SessionKey) -> (Option<String>, Option<String>, Option<String>),
    ) -> Self {
        let (running, needs_you, unknown) = snapshot.counts();
        Self {
            generated_at_ms: snapshot.generated_at_ms,
            counts: StatusCountsDto {
                running,
                needs_you,
                unknown,
            },
            live: snapshot
                .live
                .iter()
                .map(|obs| {
                    let (leaf, title, name) = lookup(&obs.key);
                    LiveSessionStateDto::project(obs, leaf, title, name)
                })
                .collect(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IdentityDto {
    pub provider: ProviderId,
    pub signed_in: bool,
    pub label: Option<String>,
    pub organization: Option<String>,
    pub plan: Option<String>,
    pub tier: Option<String>,
    pub mode: Option<String>,
    pub account_short: Option<String>,
    pub providers: Option<Vec<ProviderSummary>>,
    pub read_at_ms: i64,
    pub problem: Option<EngineError>,
}

impl From<&Identity> for IdentityDto {
    fn from(i: &Identity) -> Self {
        Self {
            provider: i.provider,
            signed_in: i.signed_in,
            label: i.label.clone(),
            organization: i.organization.clone(),
            plan: i.plan.clone(),
            tier: i.tier.clone(),
            mode: i.mode.clone(),
            account_short: i.account_short.clone(),
            providers: i.providers.clone(),
            read_at_ms: i.read_at_ms,
            problem: i.problem.clone(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapacityDto {
    pub provider: ProviderId,
    pub supported: bool,
    pub windows: Vec<CapacityWindow>,
    pub plan: Option<String>,
    pub stale: bool,
    pub source_age_s: Option<u64>,
    pub reached_limit: Option<String>,
    pub read_at_ms: i64,
    pub problem: Option<EngineError>,
}

impl From<&Capacity> for CapacityDto {
    fn from(c: &Capacity) -> Self {
        Self {
            provider: c.provider,
            supported: c.supported,
            windows: c.windows.clone(),
            plan: c.plan.clone(),
            stale: c.stale,
            source_age_s: c.source_age_s,
            reached_limit: c.reached_limit.clone(),
            read_at_ms: c.read_at_ms,
            problem: c.problem.clone(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountStatusDto {
    pub provider: ProviderId,
    pub identity: IdentityDto,
    pub capacity: CapacityDto,
}

impl From<&AccountStatus> for AccountStatusDto {
    fn from(a: &AccountStatus) -> Self {
        Self {
            provider: a.provider,
            identity: IdentityDto::from(&a.identity),
            capacity: CapacityDto::from(&a.capacity),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountStatusResult {
    pub accounts: BTreeMap<String, AccountStatusDto>,
    pub generated_at_ms: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsoleSummaryDto {
    pub id: String,
    pub session_key: Option<SessionKey>,
    pub provider: ProviderId,
    pub cwd: String,
    pub mode: String,
    pub state: String,
    pub cols: u16,
    pub rows: u16,
    pub scrollback_bytes: usize,
    pub exit_code: Option<i32>,
    pub started_at_ms: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsoleListResult {
    pub consoles: Vec<ConsoleSummaryDto>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsoleScrollbackDto {
    pub id: String,
    pub data_b64: String,
    pub truncated: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsoleOpenedDto {
    pub id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StopResultDto {
    pub key: SessionKey,
    pub stopped: Vec<StoppedProcessDto>,
    pub already_stopped: bool,
    pub ambiguous: Vec<AmbiguousProcessDto>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoppedProcessDto {
    pub pid: u32,
    pub evidence: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AmbiguousProcessDto {
    pub pid: u32,
    pub reason: String,
}

/// What `hover_toggle` answers with. The View reflects this rather than assuming its own click
/// won — the window can refuse to show, and a button that lies about it is worse than no button.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HoverVisibilityDto {
    pub visible: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PickedFolderDto {
    pub cwd: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::errors::{EngineError, ErrorKind};
    use crate::domain::{MetricBasis, ProjectKey, ResumeBlockedReason, SourceSummary};
    use std::path::PathBuf;

    fn session(state: MetricState) -> Session {
        Session {
            key: SessionKey::new(ProviderId::ClaudeCode, "0199-abc"),
            cwd: Some(PathBuf::from("/Users/k/proj")),
            project: ProjectKey::normalize(Some(std::path::Path::new("/Users/k/proj"))),
            title: "Fix the fold".into(),
            name: None,
            git_branch: Some("master".into()),
            first_active_ms: Some(10),
            last_active_ms: 20,
            closed_at_ms: None,
            resumable: true,
            resume_blocked_reason: None,
            metrics: state,
            source: SourceSummary::single(PathBuf::from("/a/b.jsonl")),
            diagnostics: Default::default(),
            signature: None,
        }
    }

    #[test]
    fn pending_ready_unavailable_and_zero_serialize_distinctly() {
        let pending = MetricStateDto::from(&MetricState::Pending);
        let zero = MetricStateDto::from(&MetricState::Ready {
            metrics: Metrics::default(),
            basis: MetricBasis::Fold,
            counted_at_ms: 1,
        });
        let bad = MetricStateDto::from(&MetricState::Unavailable {
            error: EngineError::of(ProviderId::Codex, ErrorKind::UnknownShape),
        });
        let p = serde_json::to_value(&pending).expect("serializes");
        let z = serde_json::to_value(&zero).expect("serializes");
        let b = serde_json::to_value(&bad).expect("serializes");
        assert_eq!(p["state"], "pending");
        assert!(p.get("value").is_none());
        assert_eq!(z["state"], "ready");
        assert_eq!(z["value"]["apiCalls"], 0);
        assert!(z["value"]["kpis"]["contextPerCall"].is_null());
        assert_eq!(b["state"], "unavailable");
        assert_eq!(b["error"]["kind"], "unknown_shape");
    }

    #[test]
    fn session_dto_uses_the_contracts_camel_case_names() {
        let dto = SessionDto::from(&session(MetricState::Pending));
        let json = serde_json::to_value(&dto).expect("serializes");
        assert_eq!(json["key"]["providerId"], "claude-code");
        assert_eq!(json["key"]["sid"], "0199-abc");
        assert_eq!(json["projectLeaf"], "proj");
        assert_eq!(json["lastActiveMs"], 20);
        assert_eq!(json["gitBranch"], "master");
        assert_eq!(json["sourceSummary"], "/a/b.jsonl");
        assert!(json["closedAtMs"].is_null());
        assert!(json["diagnostics"]["unknownTypes"].is_object());
    }

    #[test]
    fn a_blocked_resume_becomes_a_sentence_not_a_token() {
        let mut s = session(MetricState::Pending);
        s.resumable = false;
        s.resume_blocked_reason = Some(ResumeBlockedReason::WorkingDirectoryMissing);
        let json = serde_json::to_value(SessionDto::from(&s)).expect("serializes");
        assert_eq!(json["resumable"], false);
        let reason = json["resumeBlockedReason"].as_str().expect("a sentence");
        assert!(reason.contains("no longer exists"), "{reason}");
        assert!(!reason.contains("WorkingDirectoryMissing"));
    }

    #[test]
    fn a_row_flattens_the_session_and_adds_status() {
        let row = SessionRowDto {
            session: SessionDto::from(&session(MetricState::Pending)),
            status: Some(SessionStatus::NeedsYou),
        };
        let json = serde_json::to_value(&row).expect("serializes");
        assert_eq!(json["status"], "needs_you");
        assert_eq!(json["title"], "Fix the fold"); // flattened, not nested under `session`
        assert!(json.get("session").is_none());
    }
}

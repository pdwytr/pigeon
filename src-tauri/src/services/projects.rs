//! Project rollups over whichever scope the View asked for.
//!
//! A project is a working directory, and one folder's card shows every engine that worked in
//! it. The arithmetic lives in `domain::project`; this service decides *which rows* go into it.

use std::collections::BTreeMap;

use crate::domain::{MetricState, ProjectKey, ProjectSummary, Session, SessionKey, SessionStatus};

/// One session's contribution to its project's card: what it counted, how it is doing, and the
/// session itself. Named rather than spelled inline so the grouping map stays readable.
type ProjectRow<'a> = (MetricState, Option<SessionStatus>, &'a Session);

/// Group the in-scope sessions by project and summarize each.
///
/// Ordering is most-recently-active first, which matches the list above it; a project card that
/// jumped around between refreshes would be unusable.
pub fn summarize_all(
    sessions: &[&Session],
    metrics: impl Fn(&Session) -> MetricState,
    status: impl Fn(&SessionKey) -> Option<SessionStatus>,
) -> Vec<ProjectSummary> {
    let mut grouped: BTreeMap<ProjectKey, Vec<ProjectRow<'_>>> = BTreeMap::new();
    for session in sessions {
        grouped.entry(session.project.clone()).or_default().push((
            metrics(session),
            status(&session.key),
            session,
        ));
    }

    let mut out: Vec<ProjectSummary> = grouped
        .iter()
        .map(|(project, rows)| {
            let iter = rows.iter().map(|(state, status, session)| {
                (
                    state,
                    *status,
                    session.key.provider_id,
                    session.last_active_ms,
                )
            });
            crate::domain::summarize_project(project, iter)
        })
        .collect();
    out.sort_by(|a, b| {
        b.last_active_ms
            .cmp(&a.last_active_ms)
            .then_with(|| a.project.0.cmp(&b.project.0))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{MetricBasis, Metrics, ProviderId, SourceSummary};
    use std::path::{Path, PathBuf};

    fn session(sid: &str, provider: ProviderId, cwd: &str, last: i64) -> Session {
        Session {
            key: SessionKey::new(provider, sid),
            cwd: Some(PathBuf::from(cwd)),
            project: ProjectKey::normalize(Some(Path::new(cwd))),
            title: "t".into(),
            name: None,
            git_branch: None,
            first_active_ms: None,
            last_active_ms: last,
            closed_at_ms: None,
            resumable: true,
            resume_blocked_reason: None,
            metrics: MetricState::Pending,
            source: SourceSummary::default(),
            diagnostics: Default::default(),
            signature: None,
        }
    }

    fn ready(cache_read: u64, api_calls: u64) -> MetricState {
        MetricState::Ready {
            metrics: Metrics {
                cache_read,
                api_calls,
                ..Default::default()
            },
            basis: MetricBasis::Fold,
            counted_at_ms: 0,
        }
    }

    #[test]
    fn one_folder_gathers_every_engine_that_worked_in_it() {
        let a = session("a", ProviderId::ClaudeCode, "/Users/k/proj", 10);
        let b = session("b", ProviderId::Codex, "/Users/k/proj/", 20);
        let c = session("c", ProviderId::OpenCode, "/Users/k/other", 30);
        let rows = vec![&a, &b, &c];
        let out = summarize_all(&rows, |_| ready(10, 1), |_| None);

        assert_eq!(
            out.len(),
            2,
            "the trailing slash did not mint a second project"
        );
        let proj = out.iter().find(|p| p.project_leaf == "proj").expect("proj");
        assert_eq!(proj.sessions, 2);
        assert_eq!(proj.providers.len(), 2);
        assert_eq!(proj.totals.cache_read, 20);
    }

    #[test]
    fn project_kpis_are_recomputed_from_the_sums() {
        let a = session("a", ProviderId::ClaudeCode, "/p", 10);
        let b = session("b", ProviderId::ClaudeCode, "/p", 20);
        let rows = vec![&a, &b];
        let out = summarize_all(
            &rows,
            |s| {
                if s.key.sid == "a" {
                    ready(100, 1) // 100 per call
                } else {
                    ready(1000, 5) // 200 per call
                }
            },
            |_| None,
        );
        let p = &out[0];
        let expected = 1100.0 / 6.0;
        assert!((p.kpis.context_per_call.expect("defined") - expected).abs() < 1e-9);
        assert_ne!(
            p.kpis.context_per_call,
            Some(150.0),
            "not the mean of the row KPIs"
        );
    }

    #[test]
    fn a_pending_row_counts_toward_sessions_but_not_toward_counted() {
        let a = session("a", ProviderId::ClaudeCode, "/p", 10);
        let b = session("b", ProviderId::ClaudeCode, "/p", 20);
        let rows = vec![&a, &b];
        let out = summarize_all(
            &rows,
            |s| {
                if s.key.sid == "a" {
                    ready(10, 1)
                } else {
                    MetricState::Pending
                }
            },
            |_| None,
        );
        assert_eq!(out[0].sessions, 2);
        assert_eq!(out[0].counted, 1);
    }

    #[test]
    fn projects_sort_most_recently_active_first() {
        let a = session("a", ProviderId::ClaudeCode, "/old", 10);
        let b = session("b", ProviderId::ClaudeCode, "/new", 900);
        let rows = vec![&a, &b];
        let out = summarize_all(&rows, |_| MetricState::Pending, |_| None);
        assert_eq!(out[0].project_leaf, "new");
    }

    #[test]
    fn a_session_with_no_directory_lands_in_the_reserved_project() {
        let mut a = session("a", ProviderId::Codex, "/p", 10);
        a.cwd = None;
        a.project = ProjectKey::none();
        let rows = vec![&a];
        let out = summarize_all(&rows, |_| MetricState::Pending, |_| None);
        assert_eq!(out.len(), 1);
        assert!(out[0].project.is_none());
        assert_eq!(out[0].cwd, "(no directory)");
    }

    #[test]
    fn status_counts_come_from_the_joined_status_not_from_the_row() {
        let a = session("a", ProviderId::ClaudeCode, "/p", 10);
        let b = session("b", ProviderId::ClaudeCode, "/p", 20);
        let rows = vec![&a, &b];
        let out = summarize_all(
            &rows,
            |_| MetricState::Pending,
            |k| {
                if k.sid == "a" {
                    Some(SessionStatus::Running)
                } else {
                    Some(SessionStatus::Finished)
                }
            },
        );
        assert_eq!(out[0].status_counts.running, 1);
        assert_eq!(out[0].status_counts.finished, 1);
        assert!(out[0].status_counts.any_live());
    }
}

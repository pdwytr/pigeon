//! Discovery, scope filtering, sorting and merging across the three engines.
//!
//! This service is where an engine stops being an engine and becomes a row. It applies the
//! shared rules the adapters deliberately do not: project normalization, whether a session can
//! be resumed, which scope it falls into, and the merge order.
//!
//! **One engine's failure never blanks the list.** Each adapter's problem travels beside its
//! rows, so a missing Codex root still leaves Claude and OpenCode rendering.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::adapters::{ProviderAdapter, SessionCandidate};
use crate::api::errors::EngineError;
use crate::api::types::Scope;
use crate::cache::Cached;
use crate::domain::{
    MetricState, ProjectKey, ProviderId, ResumeBlockedReason, Session, SessionKey, SessionStatus,
};
use crate::util::now_ms;

/// How long a discovery result is reused. Ten seconds: long enough that scrolling the list does
/// not re-walk ~150 files, short enough that a session started in another terminal shows up
/// without the owner reaching for refresh.
const SESSIONS_TTL: Duration = Duration::from_secs(10);

/// Everything the three engines have, as of one moment.
#[derive(Clone, Debug, Default)]
pub struct Inventory {
    pub sessions: Vec<Session>,
    pub problems: Vec<EngineError>,
    pub generated_at_ms: i64,
}

impl Inventory {
    pub fn get(&self, key: &SessionKey) -> Option<&Session> {
        self.sessions.iter().find(|s| &s.key == key)
    }
}

/// Whether each engine's CLI is available. Injected rather than probed here, so this service
/// stays a pure function of its inputs in tests.
pub type InstalledFn = dyn Fn(ProviderId) -> bool + Send + Sync;

pub struct SessionsService {
    adapters: Arc<Vec<Box<dyn ProviderAdapter>>>,
    cache: Cached<Arc<Inventory>>,
    /// Live→closed transitions observed while this process has been running.
    ///
    /// Pigeon keeps no store, so it cannot know when a session closed before it started. The
    /// honest fallback is the engine's own last write (`last_active_ms`), which is never later
    /// than the true close. This map only ever makes the answer *more* accurate, for sessions
    /// Pigeon actually watched end.
    closed_at: Mutex<HashMap<SessionKey, i64>>,
    /// Keys seen live on the previous status pass, so a transition can be spotted.
    was_live: Mutex<HashSet<SessionKey>>,
    installed: Arc<InstalledFn>,
}

impl SessionsService {
    pub fn new(adapters: Vec<Box<dyn ProviderAdapter>>, installed: Arc<InstalledFn>) -> Self {
        Self::from_shared(Arc::new(adapters), installed)
    }

    /// The same, over adapters already shared with the accounts service, so the three engines
    /// are constructed once and both services agree about which exist.
    pub fn from_shared(
        adapters: Arc<Vec<Box<dyn ProviderAdapter>>>,
        installed: Arc<InstalledFn>,
    ) -> Self {
        Self {
            adapters,
            cache: Cached::new(SESSIONS_TTL),
            closed_at: Mutex::new(HashMap::new()),
            was_live: Mutex::new(HashSet::new()),
            installed,
        }
    }

    /// The full inventory, cached. `force` bypasses the TTL.
    ///
    /// The walk itself runs inside `spawn_blocking`, which the previous version's docstring
    /// claimed and the code did not do: `discover()` sat in a bare async block, so a filesystem
    /// crawl over ~150 transcripts plus a SQLite read ran on a runtime worker, and a panic in it
    /// escaped into whichever task had awaited it.
    pub async fn inventory(&self, force: bool) -> Arc<Inventory> {
        let adapters = Arc::clone(&self.adapters);
        let installed = Arc::clone(&self.installed);
        self.cache
            .get_or_refresh(force, move || async move {
                match tauri::async_runtime::spawn_blocking(move || {
                    walk(&adapters, installed.as_ref())
                })
                .await
                {
                    Ok(inventory) => Arc::new(inventory),
                    // A walk that panicked must not read as "this machine has no sessions". An
                    // empty list with no problem beside it is the one answer this product must
                    // never give.
                    Err(_) => Arc::new(Inventory {
                        sessions: vec![],
                        problems: vec![EngineError::new(
                            None,
                            crate::api::errors::ErrorKind::Io,
                            crate::api::errors::ErrorDetail::None,
                        )],
                        generated_at_ms: now_ms(),
                    }),
                }
            })
            .await
    }

    /// Walk every adapter now, store the result in the cache, and return it.
    ///
    /// For the background worker, which is already inside `spawn_blocking` and must not await.
    pub fn discover_now(&self) -> Arc<Inventory> {
        let inventory = Arc::new(self.discover());
        self.cache.store(Arc::clone(&inventory));
        inventory
    }

    /// Walk every adapter and normalize what comes back. Synchronous and blocking by design.
    pub fn discover(&self) -> Inventory {
        walk(&self.adapters, self.installed.as_ref())
    }

    /// Fold the newest live set in, recording any session that has just stopped being live.
    ///
    /// **`degraded` is the providers whose status read failed on this pass, and they are skipped.**
    /// Status degrades per provider: one unreadable `~/.claude/sessions` leaves the Claude keys out
    /// of `live` while Codex's are still there. Without this, that single bad poll would stamp
    /// `closed_at = now` on four sessions the owner is actively typing into, drop them out of Live,
    /// and list them in Recent badged "finished" seconds ago. It self-heals on the next good poll,
    /// but a confident wrong answer rendered in between is the thing this product exists not to do.
    pub fn observe_live(&self, live: &HashSet<SessionKey>, degraded: &HashSet<ProviderId>) {
        let now = now_ms();
        let mut was = match self.was_live.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        let ended: Vec<SessionKey> = was
            .difference(live)
            .filter(|key| !degraded.contains(&key.provider_id))
            .cloned()
            .collect();
        if !ended.is_empty() {
            if let Ok(mut closed) = self.closed_at.lock() {
                for key in ended {
                    closed.insert(key, now);
                }
            }
        }
        // A degraded provider's previously-live keys are KEPT in `was_live`, so the next good poll
        // can still see the transition if the session really did end meanwhile.
        let mut next = live.clone();
        for key in was.iter() {
            if degraded.contains(&key.provider_id) {
                next.insert(key.clone());
            }
        }
        *was = next;
    }

    /// When this session stopped being live, as best we can know.
    pub fn closed_at(&self, session: &Session) -> i64 {
        if let Ok(closed) = self.closed_at.lock() {
            if let Some(ms) = closed.get(&session.key) {
                return *ms;
            }
        }
        session.closed_at_ms.unwrap_or(session.last_active_ms)
    }

    /// Filter an inventory to one scope. `live` is membership in the live set; `recent` is
    /// everything else that closed inside the window.
    pub fn in_scope<'a>(
        &self,
        inventory: &'a Inventory,
        scope: Scope,
        live: &HashSet<SessionKey>,
        cutoff_ms: i64,
    ) -> Vec<&'a Session> {
        inventory
            .sessions
            .iter()
            .filter(|s| match scope {
                Scope::Live => live.contains(&s.key),
                Scope::Recent => !live.contains(&s.key) && self.closed_at(s) >= cutoff_ms,
            })
            .collect()
    }
}

/// Whether each engine's CLI can be found. Used by the app; tests inject their own answer.
///
/// This is a *pre-flight* signal for the View — it greys out Resume before the owner clicks.
/// The definitive resolution still happens at spawn time in the console service, because PATH
/// can change under a long-running app.
pub fn installed_probe() -> Arc<InstalledFn> {
    Arc::new(|provider: ProviderId| {
        let program = provider.program();
        let path = crate::pathenv::effective_path();
        std::env::split_paths(&path).any(|dir| {
            let candidate = dir.join(program);
            candidate.is_file()
        })
    })
}

/// One pass over every adapter, normalized. A free function so it can be moved onto a blocking
/// thread without borrowing the service.
fn walk(adapters: &[Box<dyn ProviderAdapter>], installed: &InstalledFn) -> Inventory {
    // Resolved ONCE per walk, not once per session. This stats the whole PATH until it finds the
    // program — 34 entries on this machine — and with ~150 sessions across three engines that was
    // up to five thousand syscalls every ten seconds, the full set every time for an engine that
    // is not installed at all.
    let present: Vec<(ProviderId, bool)> = ProviderId::ALL
        .iter()
        .map(|p| (*p, installed(*p)))
        .collect();

    let mut sessions = Vec::new();
    let mut problems = Vec::new();
    for adapter in adapters.iter() {
        let report = adapter.discover_sessions();
        if let Some(problem) = report.problem {
            problems.push(problem);
        }
        for candidate in report.sessions {
            if !candidate.key.is_valid() {
                // An engine that hands us an empty id has drifted. Say so; do not invent one.
                problems.push(EngineError::of(
                    adapter.provider(),
                    crate::api::errors::ErrorKind::UnknownShape,
                ));
                continue;
            }
            let installed_here = present
                .iter()
                .find(|(p, _)| *p == candidate.key.provider_id)
                .map(|(_, yes)| *yes)
                .unwrap_or(false);
            sessions.push(to_session(candidate, installed_here));
        }
    }
    sort_rows(&mut sessions);
    Inventory {
        sessions,
        problems,
        generated_at_ms: now_ms(),
    }
}

fn to_session(c: SessionCandidate, installed: bool) -> Session {
    let project = ProjectKey::normalize(c.cwd.as_deref());
    let (resumable, blocked) = resumability(&c, &project, installed);
    Session {
        key: c.key,
        cwd: c.cwd,
        project,
        title: c.title,
        name: c.name,
        git_branch: c.git_branch,
        first_active_ms: c.first_active_ms,
        last_active_ms: c.last_active_ms,
        closed_at_ms: c.closed_at_ms,
        resumable,
        resume_blocked_reason: blocked,
        metrics: MetricState::Pending,
        source: c.source,
        diagnostics: c.diagnostics,
        signature: Some(c.source_signature),
    }
}

/// Four things can stop a resume, and the owner deserves to know which one before clicking.
fn resumability(
    c: &SessionCandidate,
    project: &ProjectKey,
    installed: bool,
) -> (bool, Option<ResumeBlockedReason>) {
    if !c.key.is_valid() {
        return (false, Some(ResumeBlockedReason::InvalidSessionId));
    }
    if project.is_none() {
        return (false, Some(ResumeBlockedReason::MissingWorkingDirectory));
    }
    match &c.cwd {
        Some(cwd) if !cwd.is_dir() => {
            return (false, Some(ResumeBlockedReason::WorkingDirectoryMissing))
        }
        None => return (false, Some(ResumeBlockedReason::MissingWorkingDirectory)),
        _ => {}
    }
    if !installed {
        return (false, Some(ResumeBlockedReason::ProviderNotInstalled));
    }
    (c.resumable, None)
}

/// Merge order: most recent activity first, then provider, then the complete sid.
///
/// The two tiebreakers are not decoration. Two sessions can share a millisecond, and a list
/// whose order flickers between refreshes is a list the owner cannot keep their place in.
pub fn sort_rows(sessions: &mut [Session]) {
    sessions.sort_by(|a, b| {
        b.last_active_ms
            .cmp(&a.last_active_ms)
            .then_with(|| a.key.provider_id.cmp(&b.key.provider_id))
            .then_with(|| a.key.sid.cmp(&b.key.sid))
    });
}

/// A session's display status: its live observation, `Finished` when nothing is live for it, or
/// **`None` when we could not look**.
///
/// The third case is the one worth having. Status degrades per provider, and a session whose
/// engine's status source failed is missing from the live map for a reason that has nothing to do
/// with whether it is running. Calling that `Finished` is a confident answer to a question nobody
/// could answer; `None` renders as a neutral dash, which is what the contract types it for.
pub fn status_for(
    key: &SessionKey,
    live: &HashMap<SessionKey, SessionStatus>,
    degraded: &HashSet<ProviderId>,
) -> Option<SessionStatus> {
    if let Some(status) = live.get(key) {
        return Some(*status);
    }
    if degraded.contains(&key.provider_id) {
        return None;
    }
    Some(SessionStatus::Finished)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::ProviderSessionReport;
    use crate::domain::{
        Capacity, Diagnostics, FileSignature, Identity, SourceSignature, SourceSummary,
    };
    use std::path::PathBuf;

    struct FakeAdapter {
        provider: ProviderId,
        sessions: Vec<SessionCandidate>,
        problem: Option<EngineError>,
    }

    impl ProviderAdapter for FakeAdapter {
        fn provider(&self) -> ProviderId {
            self.provider
        }
        fn discover_sessions(&self) -> ProviderSessionReport {
            ProviderSessionReport {
                sessions: self.sessions.clone(),
                problem: self.problem.clone(),
            }
        }
        fn read_identity(&self) -> Identity {
            Identity::absent(self.provider, 0, None)
        }
        fn read_capacity(&self) -> Capacity {
            Capacity::unsupported(self.provider, 0)
        }
    }

    fn candidate(
        provider: ProviderId,
        sid: &str,
        cwd: Option<&str>,
        last: i64,
    ) -> SessionCandidate {
        SessionCandidate {
            key: SessionKey::new(provider, sid),
            cwd: cwd.map(PathBuf::from),
            title: "a title".into(),
            name: None,
            git_branch: None,
            first_active_ms: Some(last - 1000),
            last_active_ms: last,
            closed_at_ms: None,
            resumable: true,
            resume_blocked_reason: None,
            source: SourceSummary::single(PathBuf::from("/x/y.jsonl")),
            diagnostics: Diagnostics::default(),
            source_signature: SourceSignature::Codex {
                files: vec![FileSignature {
                    path: "/x/y.jsonl".into(),
                    size: 1,
                    mtime_ms: last,
                }],
            },
        }
    }

    fn service(adapters: Vec<Box<dyn ProviderAdapter>>) -> SessionsService {
        SessionsService::new(adapters, Arc::new(|_| true))
    }

    #[test]
    fn one_engines_failure_does_not_blank_the_other_engines_rows() {
        let broken = FakeAdapter {
            provider: ProviderId::Codex,
            sessions: vec![],
            problem: Some(EngineError::root_missing(ProviderId::Codex, "/nope")),
        };
        let working = FakeAdapter {
            provider: ProviderId::ClaudeCode,
            sessions: vec![candidate(ProviderId::ClaudeCode, "a", Some("/tmp"), 10)],
            problem: None,
        };
        let svc = service(vec![Box::new(broken), Box::new(working)]);
        let inv = svc.discover();
        assert_eq!(inv.sessions.len(), 1, "the working engine still rendered");
        assert_eq!(inv.problems.len(), 1, "and the broken one reported why");
    }

    #[test]
    fn rows_sort_by_activity_then_provider_then_the_complete_sid() {
        let a = FakeAdapter {
            provider: ProviderId::ClaudeCode,
            sessions: vec![
                candidate(ProviderId::ClaudeCode, "zzz", Some("/tmp"), 100),
                candidate(ProviderId::ClaudeCode, "aaa", Some("/tmp"), 100),
                candidate(ProviderId::ClaudeCode, "mid", Some("/tmp"), 500),
            ],
            problem: None,
        };
        let svc = service(vec![Box::new(a)]);
        let inv = svc.discover();
        let sids: Vec<&str> = inv.sessions.iter().map(|s| s.key.sid.as_str()).collect();
        assert_eq!(sids, vec!["mid", "aaa", "zzz"]);
    }

    #[test]
    fn two_engines_sharing_a_sid_stay_two_rows() {
        let a = FakeAdapter {
            provider: ProviderId::ClaudeCode,
            sessions: vec![candidate(ProviderId::ClaudeCode, "same", Some("/tmp"), 10)],
            problem: None,
        };
        let b = FakeAdapter {
            provider: ProviderId::Codex,
            sessions: vec![candidate(ProviderId::Codex, "same", Some("/tmp"), 10)],
            problem: None,
        };
        let inv = service(vec![Box::new(a), Box::new(b)]).discover();
        assert_eq!(inv.sessions.len(), 2);
        assert_ne!(inv.sessions[0].key, inv.sessions[1].key);
    }

    #[test]
    fn a_missing_cwd_blocks_resume_and_lands_in_the_reserved_project() {
        let a = FakeAdapter {
            provider: ProviderId::Codex,
            sessions: vec![candidate(ProviderId::Codex, "x", None, 10)],
            problem: None,
        };
        let inv = service(vec![Box::new(a)]).discover();
        let s = &inv.sessions[0];
        assert!(s.project.is_none());
        assert!(!s.resumable);
        assert_eq!(
            s.resume_blocked_reason,
            Some(ResumeBlockedReason::MissingWorkingDirectory)
        );
    }

    #[test]
    fn a_vanished_working_directory_blocks_resume_with_its_own_reason() {
        let a = FakeAdapter {
            provider: ProviderId::Codex,
            sessions: vec![candidate(
                ProviderId::Codex,
                "x",
                Some("/definitely/not/here"),
                10,
            )],
            problem: None,
        };
        let inv = service(vec![Box::new(a)]).discover();
        assert_eq!(
            inv.sessions[0].resume_blocked_reason,
            Some(ResumeBlockedReason::WorkingDirectoryMissing)
        );
    }

    #[test]
    fn an_uninstalled_engine_blocks_resume_even_when_the_folder_is_fine() {
        let a = FakeAdapter {
            provider: ProviderId::OpenCode,
            sessions: vec![candidate(ProviderId::OpenCode, "x", Some("/tmp"), 10)],
            problem: None,
        };
        let svc = SessionsService::new(vec![Box::new(a)], Arc::new(|_| false));
        let inv = svc.discover();
        assert_eq!(
            inv.sessions[0].resume_blocked_reason,
            Some(ResumeBlockedReason::ProviderNotInstalled)
        );
    }

    #[test]
    fn live_and_recent_scopes_are_disjoint() {
        let a = FakeAdapter {
            provider: ProviderId::ClaudeCode,
            sessions: vec![
                candidate(ProviderId::ClaudeCode, "live-one", Some("/tmp"), 1_000_000),
                candidate(ProviderId::ClaudeCode, "closed-one", Some("/tmp"), 900_000),
            ],
            problem: None,
        };
        let svc = service(vec![Box::new(a)]);
        let inv = svc.discover();
        let mut live = HashSet::new();
        live.insert(SessionKey::new(ProviderId::ClaudeCode, "live-one"));

        let in_live = svc.in_scope(&inv, Scope::Live, &live, 0);
        let in_recent = svc.in_scope(&inv, Scope::Recent, &live, 0);
        assert_eq!(in_live.len(), 1);
        assert_eq!(in_live[0].key.sid, "live-one");
        assert_eq!(in_recent.len(), 1);
        assert_eq!(in_recent[0].key.sid, "closed-one");
    }

    #[test]
    fn a_session_older_than_the_window_is_in_neither_scope() {
        let a = FakeAdapter {
            provider: ProviderId::ClaudeCode,
            sessions: vec![candidate(
                ProviderId::ClaudeCode,
                "ancient",
                Some("/tmp"),
                100,
            )],
            problem: None,
        };
        let svc = service(vec![Box::new(a)]);
        let inv = svc.discover();
        let live = HashSet::new();
        assert!(svc.in_scope(&inv, Scope::Live, &live, 50_000).is_empty());
        assert!(svc.in_scope(&inv, Scope::Recent, &live, 50_000).is_empty());
    }

    #[test]
    fn a_session_that_stops_being_live_records_when_it_closed() {
        let a = FakeAdapter {
            provider: ProviderId::Codex,
            sessions: vec![
                candidate(ProviderId::Codex, "ends", Some("/tmp"), 10),
                candidate(ProviderId::Codex, "keeps-going", Some("/tmp"), 10),
            ],
            problem: None,
        };
        let svc = service(vec![Box::new(a)]);
        let inv = svc.discover();
        let ends = inv
            .sessions
            .iter()
            .find(|s| s.key.sid == "ends")
            .expect("ends");
        let going = inv
            .sessions
            .iter()
            .find(|s| s.key.sid == "keeps-going")
            .expect("going");

        // Before we ever saw either live, the fallback is the engine's own last write.
        assert_eq!(svc.closed_at(ends), 10);

        let mut live: HashSet<SessionKey> = ["ends", "keeps-going"]
            .iter()
            .map(|sid| SessionKey::new(ProviderId::Codex, *sid))
            .collect();
        svc.observe_live(&live, &HashSet::new());

        let before = now_ms();
        live.remove(&SessionKey::new(ProviderId::Codex, "ends"));
        svc.observe_live(&live, &HashSet::new());
        let after = now_ms();

        // The recorded moment is THIS pass, not merely "some clock value" — the earlier version
        // of this assertion was `> 10`, which any epoch-ms number satisfies and which would have
        // passed had the wrong key been recorded, or every key, or none of the logic run at all.
        let recorded = svc.closed_at(ends);
        assert!(
            (before..=after).contains(&recorded),
            "expected a close stamped during this pass ({before}..={after}), got {recorded}"
        );
        // And the session that is STILL live must not have been touched.
        assert_eq!(
            svc.closed_at(going),
            10,
            "a live session was declared closed"
        );
    }

    #[test]
    fn a_provider_whose_status_read_failed_does_not_have_its_sessions_declared_closed() {
        // Status degrades per provider: one unreadable ~/.claude/sessions leaves the Claude keys
        // out of the live set while Codex's are still there. Treating that absence as "it ended"
        // stamped closed_at on sessions the owner was actively typing into, dropped them out of
        // Live, and listed them in Recent badged finished seconds ago.
        let a = FakeAdapter {
            provider: ProviderId::ClaudeCode,
            sessions: vec![candidate(
                ProviderId::ClaudeCode,
                "still-running",
                Some("/tmp"),
                10,
            )],
            problem: None,
        };
        let svc = service(vec![Box::new(a)]);
        let inv = svc.discover();
        let key = SessionKey::new(ProviderId::ClaudeCode, "still-running");

        let mut live = HashSet::new();
        live.insert(key.clone());
        svc.observe_live(&live, &HashSet::new());

        // The next pass cannot read Claude's status root at all.
        let mut degraded = HashSet::new();
        degraded.insert(ProviderId::ClaudeCode);
        svc.observe_live(&HashSet::new(), &degraded);

        assert_eq!(
            svc.closed_at(&inv.sessions[0]),
            10,
            "a degraded pass must not be read as a session ending"
        );

        // And the key is remembered, so a genuine end on a later good pass is still caught.
        svc.observe_live(&HashSet::new(), &HashSet::new());
        assert!(
            svc.closed_at(&inv.sessions[0]) > 10,
            "a real end is still recorded"
        );
    }

    #[test]
    fn an_engine_that_hands_us_an_empty_sid_is_reported_not_rendered() {
        let mut bad = candidate(ProviderId::Codex, "   ", Some("/tmp"), 10);
        bad.key = SessionKey::new(ProviderId::Codex, "   ");
        let a = FakeAdapter {
            provider: ProviderId::Codex,
            sessions: vec![bad],
            problem: None,
        };
        let inv = service(vec![Box::new(a)]).discover();
        assert!(inv.sessions.is_empty(), "no row invented from a bad id");
        assert_eq!(inv.problems.len(), 1, "and the drift was reported");
    }

    #[test]
    fn a_session_with_no_live_process_shows_as_finished() {
        let key = SessionKey::new(ProviderId::Codex, "s");
        let live = HashMap::new();
        assert_eq!(
            status_for(&key, &live, &HashSet::new()),
            Some(SessionStatus::Finished)
        );

        let mut live = HashMap::new();
        live.insert(key.clone(), SessionStatus::Running);
        assert_eq!(
            status_for(&key, &live, &HashSet::new()),
            Some(SessionStatus::Running)
        );
    }

    #[test]
    fn a_session_whose_engine_could_not_be_read_has_no_status_rather_than_finished() {
        // "Finished" is an answer. When the provider's status source failed, nobody has one.
        let key = SessionKey::new(ProviderId::ClaudeCode, "s");
        let mut degraded = HashSet::new();
        degraded.insert(ProviderId::ClaudeCode);
        assert_eq!(status_for(&key, &HashMap::new(), &degraded), None);

        // A different provider's failure says nothing about this session.
        let mut elsewhere = HashSet::new();
        elsewhere.insert(ProviderId::Codex);
        assert_eq!(
            status_for(&key, &HashMap::new(), &elsewhere),
            Some(SessionStatus::Finished)
        );
    }

    #[tokio::test]
    async fn the_inventory_is_cached_and_force_bypasses_it() {
        let a = FakeAdapter {
            provider: ProviderId::ClaudeCode,
            sessions: vec![candidate(ProviderId::ClaudeCode, "a", Some("/tmp"), 10)],
            problem: None,
        };
        let svc = service(vec![Box::new(a)]);
        let first = svc.inventory(false).await;
        let second = svc.inventory(false).await;
        assert!(Arc::ptr_eq(&first, &second), "the cached value was reused");
        let forced = svc.inventory(true).await;
        assert!(!Arc::ptr_eq(&first, &forced), "force re-walked");
    }
}

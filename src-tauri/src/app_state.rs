//! Shared in-memory state. One instance, held by Tauri, borrowed by every command.
//!
//! There is no store. Everything here is either a cache over the engines' own files or runtime
//! state that is meaningless once Pigeon exits — which is exactly why persisting it was
//! rejected: a poll history and a console's bytes have no value after the window closes.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::adapters::ProviderAdapter;
use crate::api::errors::{EngineError, ErrorKind};
use crate::domain::{ProviderId, SessionKey, SessionStatus, StatusSnapshot};
use crate::services::accounts::AccountsService;
use crate::services::console::ConsoleService;
use crate::services::metrics::{MetricSource, MetricsService};
use crate::services::sessions::SessionsService;
use crate::settings::Settings;

/// Whatever can tell us which sessions are live right now.
///
/// A trait rather than a concrete type so the sessions and project services can be tested with
/// a fixed answer, and so a status source that cannot read one engine does not have to pretend.
pub trait LiveStatus: Send + Sync {
    fn snapshot(&self) -> StatusSnapshot;

    /// Problems from the last status pass. They travel beside the rows, exactly as an adapter's
    /// do: an engine whose process facts we cannot read must not silently look idle.
    fn problems(&self) -> Vec<crate::api::errors::EngineError> {
        vec![]
    }

    /// Stop every process that can be *proven* to belong to this session. The default refuses,
    /// so a status source that cannot attribute processes cannot accidentally kill one.
    fn stop_session(
        &self,
        _key: &SessionKey,
    ) -> Result<crate::services::status::StopOutcome, crate::api::errors::EngineError> {
        Err(crate::api::errors::EngineError::new(
            None,
            crate::api::errors::ErrorKind::Unsupported,
            crate::api::errors::ErrorDetail::None,
        ))
    }
}

/// The answer when no status source is wired: nothing is known to be live.
///
/// This is deliberately *not* "everything is live". An unknown-status world should show the
/// owner an empty Live tab and a full Recent tab, which is visibly wrong-looking and prompts a
/// question — rather than a Live tab full of sessions that ended days ago, which looks right and
/// is a lie.
pub struct NoLiveStatus;

impl LiveStatus for NoLiveStatus {
    fn snapshot(&self) -> StatusSnapshot {
        StatusSnapshot {
            generated_at_ms: crate::util::now_ms(),
            live: vec![],
        }
    }
}

pub struct AppState {
    pub sessions: SessionsService,
    pub metrics: MetricsService,
    pub accounts: AccountsService,
    pub status: Arc<dyn LiveStatus>,
    pub console: ConsoleService,
    settings: Mutex<Settings>,
    /// Serializes the config file write and nothing else. Never held with `settings` held, so it
    /// cannot participate in a cycle; its whole job is to keep two writers from interleaving into
    /// one file.
    disk_lock: Mutex<()>,
    settings_path: PathBuf,
}

impl AppState {
    pub fn new(
        adapters: Vec<Box<dyn ProviderAdapter>>,
        metric_source: Arc<dyn MetricSource>,
        status: Arc<dyn LiveStatus>,
        console: ConsoleService,
        settings_path: PathBuf,
    ) -> Self {
        let shared: Arc<Vec<Box<dyn ProviderAdapter>>> = Arc::new(adapters);
        let installed = crate::services::sessions::installed_probe();
        // The sessions service needs its own handles; both hold the same Arc so the adapters are
        // constructed once and the two services agree about which engines exist.
        let sessions = SessionsService::from_shared(Arc::clone(&shared), installed);
        let accounts = AccountsService::new(Arc::clone(&shared));
        Self {
            sessions,
            metrics: MetricsService::new(metric_source),
            accounts,
            status,
            console,
            settings: Mutex::new(Settings::load(&settings_path)),
            disk_lock: Mutex::new(()),
            settings_path,
        }
    }

    pub fn settings(&self) -> Settings {
        self.settings.lock().map(|s| *s).unwrap_or_default()
    }

    /// Read, change and store under ONE lock.
    ///
    /// The previous shape — `settings()` then `set_settings()` — was a read-modify-write with the
    /// lock dropped in the middle, so two callers lost each other's changes: the View persisting a
    /// pane split while the owner pressed the hover key ended with the hover shown and the split
    /// silently reverted, with neither command failing.
    ///
    /// The disk write happens after the lock is released, because a file write is not an atom and
    /// this lock is taken on the command path. `disk_lock` then serializes writers so the file's
    /// order matches the order their changes landed in memory.
    pub fn update_settings<F>(&self, change: F) -> (Settings, Result<(), std::io::Error>)
    where
        F: FnOnce(Settings) -> Settings,
    {
        let next = match self.settings.lock() {
            Ok(mut guard) => {
                let next = change(*guard).sanitized();
                *guard = next;
                next
            }
            Err(_) => return (Settings::default(), Ok(())),
        };
        let _serialize = self.disk_lock.lock();
        (next, next.save(&self.settings_path))
    }

    /// The current live picture: the snapshot, the problems that came with it, and the
    /// projections the list and rollup paths need.
    ///
    /// **The problems travel with it deliberately.** A status source that cannot be read produces
    /// an empty live set, and an empty Live tab is indistinguishable from "nothing is running" —
    /// the one way this product can lie to its owner. Whoever renders a scope has to be able to
    /// say why it is empty, and whoever joins a status has to know which providers could not be
    /// asked.
    pub fn live(&self) -> LivePicture {
        let snapshot = self.status.snapshot();
        let keys: HashSet<SessionKey> = snapshot.live.iter().map(|o| o.key.clone()).collect();
        let statuses: HashMap<SessionKey, SessionStatus> = snapshot
            .live
            .iter()
            .map(|o| (o.key.clone(), SessionStatus::from(o.state)))
            .collect();

        let problems = self.status.problems();
        let degraded: HashSet<ProviderId> = problems.iter().filter_map(|p| p.provider).collect();

        // Record any session that has just stopped being live, so Recent can order by close time
        // rather than by the engine's last write — skipping any provider whose status read failed
        // on this pass, whose sessions are missing from `keys` for the wrong reason.
        self.sessions.observe_live(&keys, &degraded);

        // `Unsupported` is filtered, and only that one: it means the engine publishes nothing of
        // this kind by design — OpenCode holds no per-session file, socket or port, so its
        // processes cannot be attributed from the host at all. Repeating a permanent known limit
        // in a banner on every poll trains the owner to ignore the banner that matters.
        let reportable = problems
            .into_iter()
            .filter(|p| p.kind != ErrorKind::Unsupported)
            .collect();

        LivePicture {
            snapshot,
            problems: reportable,
            live: keys,
            statuses,
            degraded,
        }
    }
}

/// What one look at the live world produced. A struct rather than a five-tuple because four of
/// its five fields are collections and a caller destructuring positionally gets them wrong.
pub struct LivePicture {
    pub snapshot: StatusSnapshot,
    /// Worth showing the owner. A permanent design limit is already filtered out.
    pub problems: Vec<EngineError>,
    pub live: HashSet<SessionKey>,
    pub statuses: HashMap<SessionKey, SessionStatus>,
    /// Providers whose status source failed this pass. Their sessions are absent from `live` for
    /// a reason that says nothing about whether they are running.
    pub degraded: HashSet<ProviderId>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::ProviderId;

    /// A console that emits nowhere. These tests are about status problems, not about the pty.
    struct SilentEvents;

    impl crate::services::console::ConsoleEvents for SilentEvents {
        fn data(&self, _: crate::api::events::ConsoleData) {}
        fn exit(&self, _: crate::api::events::ConsoleExit) {}
    }

    struct ProblemStatus(Vec<EngineError>);

    impl LiveStatus for ProblemStatus {
        fn snapshot(&self) -> StatusSnapshot {
            StatusSnapshot {
                generated_at_ms: 1,
                live: vec![],
            }
        }
        fn problems(&self) -> Vec<EngineError> {
            self.0.clone()
        }
    }

    fn state_with(status: Arc<dyn LiveStatus>) -> AppState {
        let dir = std::env::temp_dir().join(format!("feather-test-{}", crate::util::now_ms()));
        AppState::new(
            vec![],
            Arc::new(|_: &crate::domain::Session| {
                Err(EngineError::of(
                    ProviderId::ClaudeCode,
                    ErrorKind::Unsupported,
                ))
            }),
            status,
            ConsoleService::with_events(Arc::new(SilentEvents)),
            dir.join("config.toml"),
        )
    }

    #[test]
    fn an_unwired_status_source_reports_nothing_live_rather_than_everything() {
        let snap = NoLiveStatus.snapshot();
        assert!(snap.live.is_empty());
        assert_eq!(snap.counts(), (0, 0, 0));
        assert!(NoLiveStatus.problems().is_empty());
    }

    #[test]
    fn a_status_failure_reaches_the_owner_so_an_empty_live_tab_is_explained() {
        // The one way this product can lie: a status root it cannot read produces no live
        // sessions, and an empty Live tab looks exactly like an idle machine.
        let unreadable = EngineError::root_missing(ProviderId::ClaudeCode, "/gone/sessions");
        let state = state_with(Arc::new(ProblemStatus(vec![unreadable.clone()])));
        let picture = state.live();
        assert!(picture.live.is_empty(), "nothing is live");
        assert_eq!(picture.problems.len(), 1, "and the reason travels with it");
        assert_eq!(picture.problems[0], unreadable);
    }

    #[test]
    fn a_permanent_design_limit_is_not_repeated_as_a_problem_every_poll() {
        // OpenCode holds no per-session file, socket or port, so its processes cannot be
        // attributed from the host at all. That is a known limit, not something wrong today, and
        // a banner repeating it on every poll trains the owner to ignore banners.
        let by_design = EngineError::of(ProviderId::OpenCode, ErrorKind::Unsupported);
        let state = state_with(Arc::new(ProblemStatus(vec![by_design])));
        let picture = state.live();
        assert!(
            picture.problems.is_empty(),
            "a design limit is not a failure to report"
        );
    }

    #[test]
    fn a_real_failure_still_reports_when_a_design_limit_sits_beside_it() {
        let by_design = EngineError::of(ProviderId::OpenCode, ErrorKind::Unsupported);
        let real = EngineError::of(ProviderId::Codex, ErrorKind::Io);
        let state = state_with(Arc::new(ProblemStatus(vec![by_design, real.clone()])));
        let picture = state.live();
        assert_eq!(picture.problems, vec![real]);
    }
}

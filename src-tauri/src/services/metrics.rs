//! Lazy metric folding, cached by source signature.
//!
//! **`sessions_list` never folds a transcript.** Claude's are up to 14.6 MB on this machine and
//! there are ~50 of them; folding on the list path would cost seconds before the first row
//! appeared. Rows therefore arrive with `MetricState::Pending`, a background task fills them,
//! and each filled batch is emitted on `sessions://metrics`.
//!
//! The cache key is the source signature — `(path, size, mtime)` for a file, `time_updated` for
//! an OpenCode row. Identical signatures must produce identical numbers, which is what makes a
//! resume, a console close, a cache eviction and an app restart all agree.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use crate::api::errors::EngineError;
use crate::domain::{MetricBasis, MetricState, Metrics, Session, SessionKey, SourceSignature};
use crate::util::now_ms;

/// How one engine's counters are actually obtained. Implemented by the adapter layer and
/// injected, so this service can be tested without touching a real transcript.
pub trait MetricSource: Send + Sync {
    fn read(&self, session: &Session) -> Result<(Metrics, MetricBasis), EngineError>;
}

/// A boxed closure is enough for the wiring and for every test.
impl<F> MetricSource for F
where
    F: Fn(&Session) -> Result<(Metrics, MetricBasis), EngineError> + Send + Sync,
{
    fn read(&self, session: &Session) -> Result<(Metrics, MetricBasis), EngineError> {
        self(session)
    }
}

pub struct MetricsService {
    source: Arc<dyn MetricSource>,
    /// Keyed by session, valued by the signature it was counted at plus the result. A session
    /// whose signature has moved on is recounted; one whose signature matches is not.
    cache: Mutex<HashMap<SessionKey, Entry>>,
    /// The sessions a fold is running for right now, so a second caller can tell "nobody has
    /// counted this yet" from "somebody is counting it as we speak". Guarded by its own mutex and
    /// held for one set operation, never across the fold.
    in_flight: Mutex<HashSet<SessionKey>>,
}

/// Holds one session's in-flight claim and clears it however [`MetricsService::ensure`] leaves —
/// including by unwinding. A claim stranded by a panicking fold would make that session
/// permanently uncountable, which is a worse failure than the duplicate read it prevents.
struct Claim<'a> {
    service: &'a MetricsService,
    key: SessionKey,
}

impl<'a> Claim<'a> {
    /// `Some` if this caller now owns the fold, `None` if somebody else already does.
    ///
    /// A poisoned set yields a claim: the cost is one duplicate fold, and refusing to count at
    /// all because a lock is poisoned would turn a slow path into a permanently empty one.
    fn take(service: &'a MetricsService, key: &SessionKey) -> Option<Self> {
        let claimed = match service.in_flight.lock() {
            Ok(mut flight) => flight.insert(key.clone()),
            Err(_) => true,
        };
        claimed.then(|| Self {
            service,
            key: key.clone(),
        })
    }
}

impl Drop for Claim<'_> {
    fn drop(&mut self) {
        if let Ok(mut flight) = self.service.in_flight.lock() {
            flight.remove(&self.key);
        }
    }
}

#[derive(Clone, Debug)]
struct Entry {
    signature: Option<SourceSignature>,
    state: MetricState,
}

impl MetricsService {
    pub fn new(source: Arc<dyn MetricSource>) -> Self {
        Self {
            source,
            cache: Mutex::new(HashMap::new()),
            in_flight: Mutex::new(HashSet::new()),
        }
    }

    /// The cached state for a session, without counting anything.
    pub fn peek(&self, session: &Session) -> MetricState {
        let Ok(cache) = self.cache.lock() else {
            return MetricState::Pending;
        };
        match cache.get(&session.key) {
            Some(entry) if entry.signature == session.signature => entry.state.clone(),
            // A moved signature means the engine wrote more since we counted. Pending, not stale.
            _ => MetricState::Pending,
        }
    }

    /// Count this session now, or return the cached answer if its source has not moved.
    ///
    /// Every lock is taken for one map or set operation. The fold itself — which can be tens of
    /// megabytes of JSONL — runs with **no** lock held, because a fold inside a process-wide
    /// mutex would stall every other caller behind one large file.
    ///
    /// **A fold already in flight is not started again.** `peek` answers Pending for the whole
    /// duration of a fold, so the background fill and an owner opening the same row both used to
    /// read the same 14.6 MB transcript. The second caller now returns Pending and takes the
    /// value from the `sessions://metrics` fill event instead: rows are contracted to arrive
    /// Pending anyway, so this costs the caller nothing it was promised, and it is the only
    /// coalescing that does not hold a lock across the read (invariant 9).
    pub fn ensure(&self, session: &Session) -> MetricState {
        let cached = self.peek(session);
        if !matches!(cached, MetricState::Pending) {
            return cached;
        }
        let Some(_claim) = Claim::take(self, &session.key) else {
            return MetricState::Pending;
        };
        let state = match self.source.read(session) {
            Ok((metrics, basis)) => MetricState::Ready {
                metrics,
                basis,
                counted_at_ms: now_ms(),
            },
            Err(error) => MetricState::Unavailable { error },
        };
        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(
                session.key.clone(),
                Entry {
                    signature: session.signature.clone(),
                    state: state.clone(),
                },
            );
        }
        state
    }

    /// Fill every session that is still pending, returning only the ones that changed. The
    /// caller emits these in batches.
    pub fn fill_pending(&self, sessions: &[Session]) -> Vec<(SessionKey, MetricState)> {
        let mut filled = Vec::new();
        for session in sessions {
            if matches!(self.peek(session), MetricState::Pending) {
                let state = self.ensure(session);
                // Still Pending means another thread owns this fold and will publish it. This
                // pass changed nothing for that session, so it is not news.
                if !matches!(state, MetricState::Pending) {
                    filled.push((session.key.clone(), state));
                }
            }
        }
        filled
    }

    /// Drop everything. Used when the owner forces a refresh from the top.
    pub fn clear(&self) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.clear();
        }
    }

    pub fn len(&self) -> usize {
        self.cache.lock().map(|c| c.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::errors::ErrorKind;
    use crate::domain::{FileSignature, ProjectKey, ProviderId, SourceSummary};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn session(sid: &str, size: u64) -> Session {
        Session {
            key: SessionKey::new(ProviderId::ClaudeCode, sid),
            cwd: Some(PathBuf::from("/tmp")),
            project: ProjectKey::normalize(Some(std::path::Path::new("/tmp"))),
            title: "t".into(),
            name: None,
            git_branch: None,
            first_active_ms: None,
            last_active_ms: 1,
            closed_at_ms: None,
            resumable: true,
            resume_blocked_reason: None,
            metrics: MetricState::Pending,
            source: SourceSummary::single(PathBuf::from("/tmp/a.jsonl")),
            diagnostics: Default::default(),
            signature: Some(SourceSignature::Claude {
                main: FileSignature {
                    path: "/tmp/a.jsonl".into(),
                    size,
                    mtime_ms: 5,
                },
                sidecar_mtime_ms: None,
            }),
        }
    }

    fn counting_source(calls: Arc<AtomicU32>) -> Arc<dyn MetricSource> {
        Arc::new(move |_: &Session| {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok((
                Metrics {
                    cache_read: 100,
                    api_calls: 2,
                    ..Default::default()
                },
                MetricBasis::Fold,
            ))
        })
    }

    #[test]
    fn an_unchanged_signature_is_counted_once() {
        let calls = Arc::new(AtomicU32::new(0));
        let svc = MetricsService::new(counting_source(Arc::clone(&calls)));
        let s = session("a", 10);
        let first = svc.ensure(&s);
        let second = svc.ensure(&s);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "the second call used the cache"
        );
        assert_eq!(
            first, second,
            "identical signatures produce identical metrics"
        );
    }

    #[test]
    fn a_moved_signature_is_recounted() {
        let calls = Arc::new(AtomicU32::new(0));
        let svc = MetricsService::new(counting_source(Arc::clone(&calls)));
        svc.ensure(&session("a", 10));
        svc.ensure(&session("a", 20)); // the engine wrote more
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_failed_count_is_unavailable_never_zero() {
        let svc = MetricsService::new(Arc::new(|_: &Session| {
            Err(EngineError::of(
                ProviderId::ClaudeCode,
                ErrorKind::UnknownShape,
            ))
        }));
        let state = svc.ensure(&session("a", 10));
        match state {
            MetricState::Unavailable { error } => assert_eq!(error.kind, ErrorKind::UnknownShape),
            other => panic!("a failure must not become numbers: {other:?}"),
        }
    }

    #[test]
    fn a_failure_is_cached_too_so_a_broken_file_is_not_re_read_every_pass() {
        let calls = Arc::new(AtomicU32::new(0));
        let inner = Arc::clone(&calls);
        let svc = MetricsService::new(Arc::new(move |_: &Session| {
            inner.fetch_add(1, Ordering::SeqCst);
            Err(EngineError::of(ProviderId::ClaudeCode, ErrorKind::Io))
        }));
        let s = session("a", 10);
        svc.ensure(&s);
        svc.ensure(&s);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn fill_pending_returns_only_what_it_changed() {
        let calls = Arc::new(AtomicU32::new(0));
        let svc = MetricsService::new(counting_source(Arc::clone(&calls)));
        let rows = vec![session("a", 1), session("b", 1)];
        let first = svc.fill_pending(&rows);
        assert_eq!(first.len(), 2);
        let second = svc.fill_pending(&rows);
        assert!(second.is_empty(), "nothing was still pending");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn peek_never_counts() {
        let calls = Arc::new(AtomicU32::new(0));
        let svc = MetricsService::new(counting_source(Arc::clone(&calls)));
        assert_eq!(svc.peek(&session("a", 1)), MetricState::Pending);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn clearing_the_cache_forces_a_recount() {
        let calls = Arc::new(AtomicU32::new(0));
        let svc = MetricsService::new(counting_source(Arc::clone(&calls)));
        let s = session("a", 1);
        svc.ensure(&s);
        svc.clear();
        assert!(svc.is_empty());
        svc.ensure(&s);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    /// **A fold is not re-entered while it is running.** Claude's largest transcript on this
    /// machine is 14.6 MB. The background fill and an owner opening that same row both land in
    /// `ensure`, and the window between the `peek` that answers Pending and the insert that
    /// stores the result is exactly as long as the fold — so before coalescing, both callers
    /// read the same tens of megabytes.
    ///
    /// The source blocks on its *first* call only, so a service that still double-folds fails
    /// this on the count rather than hanging the suite.
    #[test]
    fn a_transcript_another_thread_is_already_folding_is_not_folded_again() {
        let calls = Arc::new(AtomicU32::new(0));
        let (entered_tx, entered_rx) = std::sync::mpsc::channel::<()>();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        // Sender and Receiver are Send but not Sync, and a `MetricSource` is shared by reference.
        let entered_tx = Mutex::new(entered_tx);
        let release_rx = Mutex::new(release_rx);
        let counter = Arc::clone(&calls);
        let source: Arc<dyn MetricSource> = Arc::new(move |_: &Session| {
            if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                entered_tx.lock().expect("test sender").send(()).ok();
                release_rx.lock().expect("test receiver").recv().ok();
            }
            Ok((
                Metrics {
                    cache_read: 100,
                    api_calls: 2,
                    ..Default::default()
                },
                MetricBasis::Fold,
            ))
        });
        let service = Arc::new(MetricsService::new(source));

        let folding = {
            let service = Arc::clone(&service);
            std::thread::spawn(move || service.ensure(&session("a", 1)))
        };
        entered_rx
            .recv()
            .expect("the first fold reaches the source");

        // The first fold is in flight right now, and the cache still says Pending.
        let second = service.ensure(&session("a", 1));

        release_tx
            .send(())
            .expect("the first fold is still waiting");
        let first = folding.join().expect("the folding thread does not panic");

        assert_eq!(calls.load(Ordering::SeqCst), 1, "one fold, not two");
        assert!(
            matches!(first, MetricState::Ready { .. }),
            "the caller that claimed the fold gets the answer"
        );
        assert!(
            matches!(second, MetricState::Pending),
            "the second caller stays Pending and takes the value from the fill event, rather \
             than duplicating the read"
        );
    }

    /// `fill_pending` is contracted to return "only the ones that changed", and the caller emits
    /// whatever comes back on `sessions://metrics`. A session skipped because another thread is
    /// mid-fold has not changed: reporting it would publish a Pending state as news and tell the
    /// View to blank a row it is about to fill.
    #[test]
    fn a_session_skipped_because_its_fold_is_in_flight_is_not_reported_as_filled() {
        let calls = Arc::new(AtomicU32::new(0));
        let (entered_tx, entered_rx) = std::sync::mpsc::channel::<()>();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let entered_tx = Mutex::new(entered_tx);
        let release_rx = Mutex::new(release_rx);
        let counter = Arc::clone(&calls);
        let source: Arc<dyn MetricSource> = Arc::new(move |_: &Session| {
            if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                entered_tx.lock().expect("test sender").send(()).ok();
                release_rx.lock().expect("test receiver").recv().ok();
            }
            Ok((Metrics::default(), MetricBasis::Fold))
        });
        let service = Arc::new(MetricsService::new(source));

        let folding = {
            let service = Arc::clone(&service);
            std::thread::spawn(move || service.ensure(&session("a", 1)))
        };
        entered_rx
            .recv()
            .expect("the first fold reaches the source");

        let filled = service.fill_pending(&[session("a", 1)]);

        release_tx
            .send(())
            .expect("the first fold is still waiting");
        folding.join().expect("the folding thread does not panic");

        assert!(
            filled.is_empty(),
            "a skipped session changed nothing, so there is nothing to emit: {filled:?}"
        );
    }
}

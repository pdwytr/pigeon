//! Who is signed in on each engine, and how much allowance is left.
//!
//! Identity and capacity are cached separately because they change at different rates and cost
//! different things: identity is a file read that changes when the owner signs in or out
//! (5 minutes), capacity is a network call on Claude and a tail read on Codex (15 minutes).
//!
//! **No credential reaches this layer.** The adapters read a token, put it in one header and
//! drop it; what comes back is a percentage and a reset time.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use crate::adapters::ProviderAdapter;
use crate::api::errors::{EngineError, ErrorKind};
use crate::cache::Cached;
use crate::domain::{AccountStatus, Capacity, Identity, ProviderId};
use crate::util::now_ms;

const IDENTITY_TTL: Duration = Duration::from_secs(5 * 60);
const CAPACITY_TTL: Duration = Duration::from_secs(15 * 60);

pub struct AccountsService {
    adapters: Arc<Vec<Box<dyn ProviderAdapter>>>,
    identity: BTreeMap<ProviderId, Cached<Arc<Identity>>>,
    capacity: BTreeMap<ProviderId, Cached<Arc<Capacity>>>,
}

impl AccountsService {
    pub fn new(adapters: Arc<Vec<Box<dyn ProviderAdapter>>>) -> Self {
        let mut identity = BTreeMap::new();
        let mut capacity = BTreeMap::new();
        for provider in ProviderId::ALL {
            identity.insert(provider, Cached::new(IDENTITY_TTL));
            capacity.insert(provider, Cached::new(CAPACITY_TTL));
        }
        Self {
            adapters,
            identity,
            capacity,
        }
    }

    fn has(&self, provider: ProviderId) -> bool {
        self.adapters.iter().any(|a| a.provider() == provider)
    }

    /// Run one adapter call on a thread that has no tokio runtime attached to it.
    ///
    /// **`spawn_blocking` is not enough here, and finding that out cost a panic in a built app.**
    /// Every `ProviderAdapter` method is synchronous, and the Claude capacity read builds a
    /// current-thread runtime to make its HTTP request — which tokio refuses on any thread that is
    /// *in a runtime context*. A `spawn_blocking` worker is in one; that is precisely what makes
    /// `Handle::current()` work there. The first run of the built app died on exactly this,
    /// *"Cannot start a runtime from within a runtime"*, inside the account strip's first fetch,
    /// and moving the call to `spawn_blocking` did not fix it.
    ///
    /// A plain OS thread carries no such context, so the adapter's own documented contract — an
    /// async caller wants `fetch_capacity` directly, and this entry point is for a thread that is
    /// not driving a runtime — is honoured literally. The cost is one thread per *uncached* read:
    /// once per five minutes for identity, once per fifteen for capacity.
    async fn off_runtime<T, F>(
        adapters: Arc<Vec<Box<dyn ProviderAdapter>>>,
        provider: ProviderId,
        read: F,
    ) -> Option<T>
    where
        F: FnOnce(&dyn ProviderAdapter) -> T + Send + 'static,
        T: Send + 'static,
    {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let spawned = std::thread::Builder::new()
            .name(format!("feather-account-{}", provider.as_str()))
            .spawn(move || {
                let answer = adapters
                    .iter()
                    .find(|a| a.provider() == provider)
                    .map(|a| read(a.as_ref()));
                // The receiver is gone if the caller was dropped mid-read. Nothing to do about it.
                let _ = tx.send(answer);
            });
        if spawned.is_err() {
            return None;
        }
        rx.await.ok().flatten()
    }

    /// One engine's account. Identity and capacity are cached separately, and each is read on its
    /// own runtime-free thread.
    pub async fn status(&self, provider: ProviderId, force: bool) -> Option<AccountStatus> {
        if !self.has(provider) {
            return None;
        }
        let identity = self
            .identity
            .get(&provider)?
            .get_or_refresh(force, || async {
                let read =
                    Self::off_runtime(Arc::clone(&self.adapters), provider, |a| a.read_identity())
                        .await;
                // A read that could not be performed at all is not evidence the owner is signed
                // out, so it is reported as a problem rather than as a signed-out identity.
                Arc::new(read.unwrap_or_else(|| {
                    Identity::absent(
                        provider,
                        now_ms(),
                        Some(EngineError::of(provider, ErrorKind::Io)),
                    )
                }))
            })
            .await;
        let capacity = self
            .capacity
            .get(&provider)?
            .get_or_refresh(force, || async {
                let read =
                    Self::off_runtime(Arc::clone(&self.adapters), provider, |a| a.read_capacity())
                        .await;
                Arc::new(read.unwrap_or_else(|| {
                    Capacity::problem(provider, now_ms(), EngineError::of(provider, ErrorKind::Io))
                }))
            })
            .await;
        Some(AccountStatus {
            provider,
            identity: (*identity).clone(),
            capacity: (*capacity).clone(),
        })
    }

    /// Every engine, in a stable order.
    pub async fn all(&self, force: bool) -> Vec<AccountStatus> {
        let mut out = Vec::new();
        for provider in ProviderId::ALL {
            if let Some(status) = self.status(provider, force).await {
                out.push(status);
            }
        }
        out
    }

    pub fn invalidate(&self, provider: ProviderId) {
        if let Some(slot) = self.identity.get(&provider) {
            slot.clear();
        }
        if let Some(slot) = self.capacity.get(&provider) {
            slot.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::ProviderSessionReport;
    use crate::domain::{CapacityWindow, CapacityWindowName};
    use std::sync::atomic::{AtomicU32, Ordering};

    struct FakeAdapter {
        provider: ProviderId,
        identity_reads: Arc<AtomicU32>,
        capacity_reads: Arc<AtomicU32>,
        supported: bool,
    }

    impl ProviderAdapter for FakeAdapter {
        fn provider(&self) -> ProviderId {
            self.provider
        }
        fn discover_sessions(&self) -> ProviderSessionReport {
            ProviderSessionReport::default()
        }
        fn read_identity(&self) -> Identity {
            self.identity_reads.fetch_add(1, Ordering::SeqCst);
            let mut id = Identity::absent(self.provider, 1, None);
            id.signed_in = true;
            id.label = Some("someone@example.com".into());
            id
        }
        fn read_capacity(&self) -> Capacity {
            self.capacity_reads.fetch_add(1, Ordering::SeqCst);
            if !self.supported {
                return Capacity::unsupported(self.provider, 1);
            }
            Capacity {
                supported: true,
                windows: vec![CapacityWindow {
                    name: CapacityWindowName::FiveHour,
                    window_minutes: 300,
                    used_pct: 42.0,
                    resets_at_ms: Some(9),
                }],
                ..Capacity::unsupported(self.provider, 1)
            }
        }
    }

    fn service(supported: bool) -> (AccountsService, Arc<AtomicU32>, Arc<AtomicU32>) {
        let ids = Arc::new(AtomicU32::new(0));
        let caps = Arc::new(AtomicU32::new(0));
        let adapters: Vec<Box<dyn ProviderAdapter>> = vec![Box::new(FakeAdapter {
            provider: ProviderId::ClaudeCode,
            identity_reads: Arc::clone(&ids),
            capacity_reads: Arc::clone(&caps),
            supported,
        })];
        (AccountsService::new(Arc::new(adapters)), ids, caps)
    }

    /// An adapter whose capacity read builds a tokio runtime, exactly as the Claude adapter's
    /// does. Calling it from inside an async task panics with "Cannot start a runtime from within
    /// a runtime" — so this passing is proof the read happened on a blocking thread.
    struct RuntimeBuildingAdapter;

    impl ProviderAdapter for RuntimeBuildingAdapter {
        fn provider(&self) -> ProviderId {
            ProviderId::ClaudeCode
        }
        fn discover_sessions(&self) -> ProviderSessionReport {
            ProviderSessionReport::default()
        }
        fn read_identity(&self) -> Identity {
            Identity::absent(ProviderId::ClaudeCode, 1, None)
        }
        fn read_capacity(&self) -> Capacity {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("a current-thread runtime");
            runtime.block_on(async { Capacity::unsupported(ProviderId::ClaudeCode, 2) })
        }
    }

    /// The regression for a panic a built app actually hit on its first run: the accounts service
    /// used to call the adapter straight from the async closure, and Claude's capacity read
    /// blew up on the runtime thread before the account strip could render anything.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_adapter_that_builds_a_runtime_is_not_called_on_the_async_thread() {
        let adapters: Vec<Box<dyn ProviderAdapter>> = vec![Box::new(RuntimeBuildingAdapter)];
        let svc = AccountsService::new(Arc::new(adapters));
        let status = svc
            .status(ProviderId::ClaudeCode, false)
            .await
            .expect("an account");
        assert!(!status.capacity.supported);
        assert!(
            status.capacity.problem.is_none(),
            "it completed rather than failing"
        );
    }

    #[tokio::test]
    async fn an_account_is_read_once_and_then_cached() {
        let (svc, ids, caps) = service(true);
        svc.status(ProviderId::ClaudeCode, false)
            .await
            .expect("an account");
        svc.status(ProviderId::ClaudeCode, false)
            .await
            .expect("an account");
        assert_eq!(ids.load(Ordering::SeqCst), 1);
        assert_eq!(caps.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn force_re_reads_both_halves() {
        let (svc, ids, caps) = service(true);
        svc.status(ProviderId::ClaudeCode, false)
            .await
            .expect("an account");
        svc.status(ProviderId::ClaudeCode, true)
            .await
            .expect("an account");
        assert_eq!(ids.load(Ordering::SeqCst), 2);
        assert_eq!(caps.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn an_engine_with_no_allowance_reports_an_absence_not_a_zero_bar() {
        let (svc, _, _) = service(false);
        let status = svc
            .status(ProviderId::ClaudeCode, false)
            .await
            .expect("an account");
        assert!(!status.capacity.supported);
        assert!(status.capacity.windows.is_empty());
        assert!(
            status.capacity.problem.is_none(),
            "unsupported is an absence, not an error"
        );
    }

    #[tokio::test]
    async fn an_unconfigured_engine_returns_nothing_rather_than_a_blank_account() {
        let (svc, _, _) = service(true);
        assert!(svc.status(ProviderId::OpenCode, false).await.is_none());
    }

    #[tokio::test]
    async fn invalidating_forces_the_next_read() {
        let (svc, ids, _) = service(true);
        svc.status(ProviderId::ClaudeCode, false)
            .await
            .expect("an account");
        svc.invalidate(ProviderId::ClaudeCode);
        svc.status(ProviderId::ClaudeCode, false)
            .await
            .expect("an account");
        assert_eq!(ids.load(Ordering::SeqCst), 2);
    }
}

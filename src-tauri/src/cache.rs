//! Typed cache slots with a TTL and refresh coalescing.
//!
//! **The std mutex is never held across I/O.** A slot's `Mutex` guards one read or one store of
//! the cached value and nothing else; the refresh itself runs outside it, serialized by a
//! separate `tokio::Mutex` so two callers arriving together produce one refresh rather than two.
//! This is Demo Studio's locking rule (resolve → lock → write → unlock) applied before it can
//! be broken rather than after.

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// One cached value plus the moment it was stored.
///
/// The age is an [`Instant`], not a wall clock. `now_ms()` moves when NTP steps the clock, and a
/// backwards step made `saturating_sub` return 0 for every entry — so everything looked fresh
/// until real time caught up. `Instant` is monotonic and cannot do that.
///
/// `observed_at_ms` is a different thing: it is when the VALUE was observed, used to refuse a
/// write that is older than what is already stored.
#[derive(Clone, Debug)]
struct Entry<T> {
    value: T,
    stored_at: Instant,
    observed_at_ms: i64,
}

/// A cache of one value, refreshed on demand.
pub struct Cached<T> {
    slot: Mutex<Option<Entry<T>>>,
    refresh: tokio::sync::Mutex<()>,
    ttl: Duration,
}

impl<T: Clone> Cached<T> {
    pub fn new(ttl: Duration) -> Self {
        Self {
            slot: Mutex::new(None),
            refresh: tokio::sync::Mutex::new(()),
            ttl,
        }
    }

    /// The cached value if it is still inside its TTL. Never blocks on a refresh.
    pub fn fresh(&self) -> Option<T> {
        let guard = self.slot.lock().ok()?;
        let entry = guard.as_ref()?;
        if entry.stored_at.elapsed() <= self.ttl {
            Some(entry.value.clone())
        } else {
            None
        }
    }

    pub fn store(&self, value: T) {
        self.store_observed(value, i64::MAX);
    }

    /// Store a value observed at a stated moment, refusing one older than what is already there.
    ///
    /// Not every producer goes through [`Cached::get_or_refresh`]: the status service runs two
    /// refreshes concurrently on purpose, rather than hold a lock across two `lsof` launches. When
    /// the slower one finishes second it would otherwise overwrite the newer world with the older
    /// one, stamped as if it were current — a session that started in between then disappears for
    /// a whole extra poll. Pass the moment the value describes, and an out-of-order write is
    /// dropped.
    ///
    /// [`Cached::store`] passes `i64::MAX`, which always wins — the right default for a producer
    /// that is already serialized by the refresh mutex.
    pub fn store_observed(&self, value: T, observed_at_ms: i64) {
        if let Ok(mut guard) = self.slot.lock() {
            if let Some(existing) = guard.as_ref() {
                if observed_at_ms < existing.observed_at_ms {
                    return;
                }
            }
            *guard = Some(Entry {
                value,
                stored_at: Instant::now(),
                observed_at_ms,
            });
        }
    }

    pub fn clear(&self) {
        if let Ok(mut guard) = self.slot.lock() {
            *guard = None;
        }
    }

    /// Return the fresh value, or produce one. Two concurrent callers do one `produce`: the
    /// second waits on the refresh mutex and then finds the first one's result already stored.
    ///
    /// `produce` runs with **no** std lock held, which is the whole point of the two-mutex shape.
    pub async fn get_or_refresh<F, Fut>(&self, force: bool, produce: F) -> T
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = T>,
    {
        if !force {
            if let Some(value) = self.fresh() {
                return value;
            }
        }
        let _serialize = self.refresh.lock().await;
        if !force {
            if let Some(value) = self.fresh() {
                return value;
            }
        }
        let value = produce().await;
        self.store(value.clone());
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    #[test]
    fn a_fresh_value_is_returned_and_an_expired_one_is_not() {
        let cache: Cached<u32> = Cached::new(Duration::from_secs(60));
        assert_eq!(cache.fresh(), None);
        cache.store(7);
        assert_eq!(cache.fresh(), Some(7));

        let expired: Cached<u32> = Cached::new(Duration::from_millis(0));
        expired.store(9);
        std::thread::sleep(Duration::from_millis(2));
        assert_eq!(expired.fresh(), None);
    }

    #[tokio::test]
    async fn concurrent_callers_produce_once() {
        let cache: Arc<Cached<u32>> = Arc::new(Cached::new(Duration::from_secs(60)));
        let calls = Arc::new(AtomicU32::new(0));
        let mut handles = vec![];
        for _ in 0..8 {
            let cache = Arc::clone(&cache);
            let calls = Arc::clone(&calls);
            handles.push(tokio::spawn(async move {
                cache
                    .get_or_refresh(false, || async {
                        calls.fetch_add(1, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_millis(5)).await;
                        42u32
                    })
                    .await
            }));
        }
        for h in handles {
            assert_eq!(h.await.expect("task"), 42);
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1, "the refresh coalesced");
    }

    #[test]
    fn a_value_observed_earlier_cannot_overwrite_a_newer_one() {
        // The status service refreshes concurrently on purpose. When the slower pass lands second
        // it must not replace the newer world with the older one.
        let cache: Cached<&str> = Cached::new(Duration::from_secs(60));
        cache.store_observed("later", 200);
        cache.store_observed("earlier", 100);
        assert_eq!(cache.fresh(), Some("later"));
        cache.store_observed("latest", 300);
        assert_eq!(cache.fresh(), Some("latest"));
    }

    #[tokio::test]
    async fn a_plain_caller_with_a_fresh_value_does_not_wait_behind_someone_elses_force() {
        // Deliberate, and worth pinning: a caller that already has a fresh answer returns it
        // immediately rather than blocking on a refresh it did not ask for. It may therefore hand
        // back a value that is about to be replaced, which is the right trade for a list that has
        // to render now.
        let cache: Arc<Cached<u32>> = Arc::new(Cached::new(Duration::from_secs(60)));
        cache.store(1);
        let calls = Arc::new(AtomicU32::new(0));

        let forced = {
            let cache = Arc::clone(&cache);
            let calls = Arc::clone(&calls);
            tokio::spawn(async move {
                cache
                    .get_or_refresh(true, || async {
                        calls.fetch_add(1, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_millis(20)).await;
                        2u32
                    })
                    .await
            })
        };
        tokio::time::sleep(Duration::from_millis(2)).await;
        let plain = cache.get_or_refresh(false, || async { 99u32 }).await;

        assert_eq!(plain, 1, "the fresh value, not a wait and not its own 99");
        assert_eq!(forced.await.expect("task"), 2);
        assert_eq!(
            cache.fresh(),
            Some(2),
            "and the force's result is what is stored"
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "the plain caller produced nothing"
        );
    }

    #[tokio::test]
    async fn a_plain_caller_with_nothing_cached_waits_for_a_concurrent_force() {
        // The other half: with no fresh value to return, the plain caller queues behind the force
        // on the refresh mutex and then finds its result already stored, rather than producing a
        // second time.
        let cache: Arc<Cached<u32>> = Arc::new(Cached::new(Duration::from_secs(60)));
        let calls = Arc::new(AtomicU32::new(0));

        let forced = {
            let cache = Arc::clone(&cache);
            let calls = Arc::clone(&calls);
            tokio::spawn(async move {
                cache
                    .get_or_refresh(true, || async {
                        calls.fetch_add(1, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_millis(20)).await;
                        7u32
                    })
                    .await
            })
        };
        tokio::time::sleep(Duration::from_millis(2)).await;
        let plain = {
            let cache = Arc::clone(&cache);
            let calls = Arc::clone(&calls);
            tokio::spawn(async move {
                cache
                    .get_or_refresh(false, || async {
                        calls.fetch_add(1, Ordering::SeqCst);
                        99u32
                    })
                    .await
            })
        };

        assert_eq!(forced.await.expect("task"), 7);
        assert_eq!(
            plain.await.expect("task"),
            7,
            "it adopted the force's result"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1, "only the force produced");
    }

    #[tokio::test]
    async fn force_bypasses_a_fresh_value() {
        let cache: Cached<u32> = Cached::new(Duration::from_secs(60));
        cache.store(1);
        let got = cache.get_or_refresh(true, || async { 2 }).await;
        assert_eq!(got, 2);
        assert_eq!(cache.fresh(), Some(2));
    }

    #[test]
    fn clear_empties_the_slot() {
        let cache: Cached<u32> = Cached::new(Duration::from_secs(60));
        cache.store(3);
        assert_eq!(cache.fresh(), Some(3));
        cache.clear();
        assert_eq!(cache.fresh(), None);
    }
}

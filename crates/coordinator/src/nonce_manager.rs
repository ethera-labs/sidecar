//! Per-XT nonce reservations for sidecar-signed `putInbox` transactions.
//!
//! Each reservation is owned by an XT instance and freed only by an explicit
//! lifecycle event (`release_confirmed` on canonical inclusion, `release_aborted`
//! when the builder aborts the XT before its putInbox could land). Aborted
//! ranges are recycled so the next XT can reissue the same nonce instead of
//! stranding the lane behind a permanent gap. The canonical chain nonce is
//! used only as a lower bound - it can never move the cursor backwards under
//! a live reservation, which closes the rollover race where exact-resync could
//! reuse a nonce that an in-flight putInbox tx was about to claim canonically.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;

use tokio::sync::Mutex;

use sidecar_primitives_traits::CoordinatorError;

#[derive(Debug, Default)]
pub(crate) struct DeferredNonceManager {
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    canonical_floor: u64,
    initialized: bool,
    live: BTreeMap<String, ReservedRange>,
    recycled: BTreeSet<u64>,
}

#[derive(Debug, Clone, Copy)]
struct ReservedRange {
    start: u64,
    end: u64,
}

impl DeferredNonceManager {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Reserve a contiguous run of `count` nonces for `instance_id` and return
    /// the starting nonce. `fetch_canonical` is queried under the lock so a
    /// concurrent `reset` (rollback) cannot wipe the floor between the fetch
    /// and the merge. Result raises the canonical floor (never lowers it) and
    /// trims recycled and live entries the chain has already moved past.
    ///
    /// `count` must be non-zero; callers gate on empty dependency lists.
    pub(crate) async fn reserve<F, Fut>(
        &self,
        instance_id: &str,
        count: usize,
        fetch_canonical: F,
    ) -> Result<u64, CoordinatorError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<u64, CoordinatorError>>,
    {
        debug_assert!(count > 0, "reserve must be called with non-zero count");
        let mut inner = self.inner.lock().await;
        let canonical = fetch_canonical().await?;
        inner.canonical_floor = inner.canonical_floor.max(canonical);
        inner.initialized = true;

        let floor = inner.canonical_floor;
        inner.recycled.retain(|&n| n >= floor);
        inner.live.retain(|_, range| range.end > floor);

        let start = if count == 1 {
            inner
                .recycled
                .iter()
                .next()
                .copied()
                .unwrap_or_else(|| live_end(&inner))
        } else {
            live_end(&inner)
        };
        let end = start.saturating_add(count as u64);
        for n in start..end {
            inner.recycled.remove(&n);
        }
        inner
            .live
            .insert(instance_id.to_string(), ReservedRange { start, end });
        Ok(start)
    }

    /// Mark an XT's reservation as canonically included. Its nonces are not
    /// returned to the recycled pool - the chain has consumed them - and the
    /// canonical floor is advanced to the reservation's end so any stragglers
    /// in `live` or `recycled` below that point are dropped on next reserve.
    pub(crate) async fn release_confirmed(&self, instance_id: &str) {
        let mut inner = self.inner.lock().await;
        if let Some(range) = inner.live.remove(instance_id) {
            inner.canonical_floor = inner.canonical_floor.max(range.end);
        }
    }

    /// Return an XT's reservation to the recycled pool so the next XT can
    /// claim those nonces. Only nonces still at or above the canonical floor
    /// are recycled - nonces the chain has already advanced past are dropped.
    pub(crate) async fn release_aborted(&self, instance_id: &str) {
        let mut inner = self.inner.lock().await;
        let Some(range) = inner.live.remove(instance_id) else {
            return;
        };
        let floor = inner.canonical_floor;
        for n in range.start..range.end {
            if n >= floor {
                inner.recycled.insert(n);
            }
        }
    }

    /// Wipe every reservation and the canonical floor. Call only when the
    /// underlying chain state has moved *backwards* (rollback / reorg of the
    /// signer's prior putInbox txs); the next `reserve` re-reads canonical
    /// and starts the lane from scratch. Routine aborts must use
    /// `release_aborted` instead so other live reservations are preserved.
    pub(crate) async fn reset(&self) {
        let mut inner = self.inner.lock().await;
        *inner = Inner::default();
    }
}

fn live_end(inner: &Inner) -> u64 {
    inner
        .live
        .values()
        .map(|range| range.end)
        .chain(std::iter::once(inner.canonical_floor))
        .max()
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reserve_starts_from_canonical_floor() {
        let manager = DeferredNonceManager::new();
        let start = manager
            .reserve("xt-1", 2, || async { Ok(7) })
            .await
            .unwrap();
        assert_eq!(start, 7);
        // Second reserve must come after the first range, never below canonical.
        let next = manager
            .reserve("xt-2", 1, || async { Ok(7) })
            .await
            .unwrap();
        assert_eq!(next, 9);
    }

    #[tokio::test]
    async fn canonical_floor_is_monotonic_under_live_reservation() {
        let manager = DeferredNonceManager::new();
        // Reserve 3 nonces against canonical=7 (in-flight putInbox txs).
        assert_eq!(
            manager
                .reserve("xt-1", 3, || async { Ok(7) })
                .await
                .unwrap(),
            7
        );
        // Period rollover: the builder reports canonical=7 because the
        // in-flight txs have not yet propagated to `pending`. The next reserve
        // must not collide with the live range even though canonical is stale.
        let next = manager
            .reserve("xt-2", 1, || async { Ok(7) })
            .await
            .unwrap();
        assert_eq!(next, 10);
    }

    #[tokio::test]
    async fn aborted_nonces_are_recycled_for_next_reserve() {
        let manager = DeferredNonceManager::new();
        assert_eq!(
            manager
                .reserve("xt-1", 1, || async { Ok(5) })
                .await
                .unwrap(),
            5
        );
        assert_eq!(
            manager
                .reserve("xt-2", 1, || async { Ok(5) })
                .await
                .unwrap(),
            6
        );
        manager.release_aborted("xt-1").await;
        // Recycled nonce 5 is reused before allocating a fresh slot.
        let next = manager
            .reserve("xt-3", 1, || async { Ok(5) })
            .await
            .unwrap();
        assert_eq!(next, 5);
    }

    #[tokio::test]
    async fn confirmed_release_advances_canonical_floor() {
        let manager = DeferredNonceManager::new();
        assert_eq!(
            manager
                .reserve("xt-1", 2, || async { Ok(0) })
                .await
                .unwrap(),
            0
        );
        manager.release_confirmed("xt-1").await;
        // After confirm, even if the builder's canonical view still reports a
        // pre-confirm value, we never hand out a nonce below the confirmed end.
        let next = manager
            .reserve("xt-2", 1, || async { Ok(0) })
            .await
            .unwrap();
        assert_eq!(next, 2);
    }

    #[tokio::test]
    async fn canonical_advance_evicts_stale_recycled_and_live() {
        let manager = DeferredNonceManager::new();
        manager
            .reserve("xt-1", 1, || async { Ok(0) })
            .await
            .unwrap();
        manager
            .reserve("xt-2", 1, || async { Ok(0) })
            .await
            .unwrap();
        manager.release_aborted("xt-1").await;
        // Chain caught up past both nonces - recycled and live entries below
        // the new canonical floor are dropped on the next reserve.
        let next = manager
            .reserve("xt-3", 1, || async { Ok(5) })
            .await
            .unwrap();
        assert_eq!(next, 5);
    }

    #[tokio::test]
    async fn rollover_race_does_not_reuse_inflight_nonce() {
        // Models the user-reported burst-at-rollover scenario:
        //   XT_a reserves nonce 7. Period rolls over while XT_a's putInbox is
        //   in flight on the builder. `fetch_canonical` still reports 7
        //   because the in-flight tx has not landed canonical.
        //   XT_b is decided in the new period and must NOT collide with 7.
        let manager = DeferredNonceManager::new();
        let a_start = manager
            .reserve("xt-a", 1, || async { Ok(7) })
            .await
            .unwrap();
        assert_eq!(a_start, 7);
        let b_start = manager
            .reserve("xt-b", 1, || async { Ok(7) })
            .await
            .unwrap();
        assert_eq!(b_start, 8);
    }

    #[tokio::test]
    async fn reset_wipes_high_floor_so_post_rollback_canonical_takes_effect() {
        // Models the rollback / L1 reorg path: the signer's prior putInbox txs
        // are discarded, on-chain canonical nonce moves *backwards*. Without
        // reset() the monotonic floor would strand the lane above the new
        // canonical view; with reset() the next reserve re-anchors cleanly.
        let manager = DeferredNonceManager::new();
        manager
            .reserve("xt-pre-rollback", 3, || async { Ok(100) })
            .await
            .unwrap();

        manager.reset().await;

        let post = manager
            .reserve("xt-post-rollback", 1, || async { Ok(50) })
            .await
            .unwrap();
        assert_eq!(post, 50);
    }

    #[tokio::test]
    async fn reset_blocked_until_inflight_reserve_completes() {
        // The race codex flagged: an in-flight reserve that fetched canonical
        // outside the lock could overwrite the floor *after* a concurrent
        // reset wiped it. With fetch-under-lock, a reset waiting on the mutex
        // can only run *after* the reserve fully commits, and a subsequent
        // reserve sees only the post-reset (post-rollback) canonical.
        use std::sync::Arc;
        use tokio::sync::Notify;

        let manager = Arc::new(DeferredNonceManager::new());
        let gate = Arc::new(Notify::new());
        let in_fetch = Arc::new(Notify::new());

        let reserver = {
            let manager = Arc::clone(&manager);
            let gate = Arc::clone(&gate);
            let in_fetch = Arc::clone(&in_fetch);
            tokio::spawn(async move {
                manager
                    .reserve("xt-inflight", 1, || async move {
                        in_fetch.notify_one();
                        gate.notified().await;
                        Ok::<u64, CoordinatorError>(100)
                    })
                    .await
                    .unwrap()
            })
        };

        // Wait until the reserver is inside fetch_canonical (and therefore
        // holding the Inner lock). A concurrent reset must block on the lock.
        in_fetch.notified().await;

        let resetter = {
            let manager = Arc::clone(&manager);
            tokio::spawn(async move { manager.reset().await })
        };

        // Release fetch_canonical; the reserver commits canonical_floor=100,
        // then the resetter runs and wipes it.
        gate.notify_one();
        let inflight_start = reserver.await.unwrap();
        resetter.await.unwrap();
        assert_eq!(inflight_start, 100);

        // Subsequent reserve sees only the post-rollback canonical view.
        let post = manager
            .reserve("xt-after-reset", 1, || async { Ok(30) })
            .await
            .unwrap();
        assert_eq!(post, 30);
    }
}

//! Monotonic nonce reservation for release-time putInbox transaction building.

use std::future::Future;

use tokio::sync::Mutex;

use compose_primitives_traits::CoordinatorError;

/// Monotonic nonce manager for coordinator-signed `putInbox` transactions.
#[derive(Debug)]
pub(crate) struct DeferredNonceManager {
    inner: Mutex<Inner>,
}

#[derive(Debug)]
struct Inner {
    next_nonce: u64,
    initialized: bool,
}

impl DeferredNonceManager {
    pub(crate) fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                next_nonce: 0,
                initialized: false,
            }),
        }
    }

    /// Reserve a contiguous nonce range and return the starting nonce.
    pub(crate) async fn reserve<F, Fut>(
        &self,
        count: usize,
        fetch_base: F,
    ) -> Result<u64, CoordinatorError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<u64, CoordinatorError>>,
    {
        if count == 0 {
            return Ok(0);
        }

        let mut inner = self.inner.lock().await;
        if !inner.initialized {
            inner.next_nonce = fetch_base().await?;
            inner.initialized = true;
        }

        let start = inner.next_nonce;
        inner.next_nonce = inner.next_nonce.saturating_add(count as u64);
        Ok(start)
    }

    /// Mark the nonce state as uninitialized so the next `reserve` call
    /// will re-fetch the base nonce from the builder.
    ///
    /// Call this when a `resync` fails so that stale nonces are not reused.
    pub(crate) async fn reset(&self) {
        let mut inner = self.inner.lock().await;
        inner.initialized = false;
        inner.next_nonce = 0;
    }

    /// Reset the local counter from the builder's pending nonce view.
    pub(crate) async fn resync<F, Fut>(&self, fetch_base: F) -> Result<(), CoordinatorError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<u64, CoordinatorError>>,
    {
        let mut inner = self.inner.lock().await;
        inner.next_nonce = fetch_base().await?;
        inner.initialized = true;
        Ok(())
    }
}

impl Default for DeferredNonceManager {
    fn default() -> Self {
        Self::new()
    }
}

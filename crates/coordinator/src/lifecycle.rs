use std::time::Duration;

use sidecar_primitives_traits::CoordinatorError;
use tracing::{info, warn};

use super::{CoordinatorState, DefaultCoordinator};

impl DefaultCoordinator {
    // Bounds `stop()` so a stuck verification HTTP call or unresponsive peer
    // cannot block process shutdown indefinitely.
    const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

    /// Start the coordinator's background tasks (cleanup loop, etc.).
    pub async fn start(&self) -> Result<(), CoordinatorError> {
        info!(chain_id = %self.chain_id, "Starting coordinator");

        let coord = self.clone();
        self.task_tracker.spawn(async move {
            coord.cleanup_loop().await;
        });

        Ok(())
    }

    /// Gracefully shut down, waiting for all spawned tasks to complete.
    ///
    /// Caps the wait so a stuck verification HTTP call or unresponsive peer
    /// cannot block the binary's shutdown indefinitely.
    pub async fn stop(&self) -> Result<(), CoordinatorError> {
        info!("Stopping coordinator");
        self.task_tracker.close();
        if tokio::time::timeout(Self::SHUTDOWN_TIMEOUT, self.task_tracker.wait())
            .await
            .is_err()
        {
            warn!("Coordinator shutdown timed out waiting for background tasks");
        }
        Ok(())
    }

    /// Remove decided XTs older than `max_age`.
    pub async fn cleanup(&self, max_age: Duration) {
        let mut state = self.state.write().await;
        let now = std::time::Instant::now();
        let mut removed_raw_ids = Vec::new();
        state.pending.retain(|_id, xt| {
            let keep = xt
                .confirmed_at
                .or(xt.decided_at)
                .is_none_or(|t| now.duration_since(t) <= max_age);
            if !keep {
                // The XT is dropped by retain, so steal the key instead of cloning.
                removed_raw_ids.push(std::mem::take(&mut xt.instance_id));
            }
            keep
        });
        for raw_id in &removed_raw_ids {
            state.mailbox_index.remove(raw_id);
        }
        // Retain cross-index entries only while their owning XT is still registered.
        // Split field borrows let `retain` consult related maps without key snapshots.
        let CoordinatorState {
            pending,
            submitted_fingerprints,
            mailbox_buffer,
            mailbox_index,
            ..
        } = &mut *state;
        submitted_fingerprints.retain(|_, id| pending.contains_key(id.as_str()));
        mailbox_buffer.retain(|key, _| mailbox_index.contains_key(key.as_slice()));

        // Drop submission channels where the caller already timed out.
        state.pending_submissions.retain(|_, waiters| {
            waiters.retain(|tx| !tx.is_closed());
            !waiters.is_empty()
        });
        if let Some(m) = &self.metrics {
            m.mailbox_buffer_size.set(state.mailbox_buffer.len() as i64);
        }
    }

    async fn cleanup_loop(&self) {
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        loop {
            interval.tick().await;
            self.cleanup(Duration::from_secs(300)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use ethera_spec::ChainId;

    use crate::model::pending_xt::PendingXt;
    use crate::{DefaultCoordinator, VerificationConfig};

    #[tokio::test]
    async fn cleanup_removes_old_decided_xts() {
        let coordinator = DefaultCoordinator::new(
            ChainId(77777),
            None,
            None,
            None,
            None,
            None,
            1000,
            VerificationConfig::default(),
        );

        {
            let mut state = coordinator.state.write().await;
            let mut xt = PendingXt::new("xt-77777-1".to_string(), b"xt-77777-1".to_vec());
            xt.decision = Some(true);
            xt.decided_at = Some(
                std::time::Instant::now()
                    .checked_sub(Duration::from_secs(400))
                    .unwrap(),
            );
            state.pending.insert(xt.id.clone(), xt);
        }

        coordinator.cleanup(Duration::from_secs(300)).await;

        let state = coordinator.state.read().await;
        assert!(state.pending.is_empty());
    }

    #[tokio::test]
    async fn cleanup_removes_old_confirmed_xts() {
        let coordinator = DefaultCoordinator::new(
            ChainId(77777),
            None,
            None,
            None,
            None,
            None,
            1000,
            VerificationConfig::default(),
        );

        {
            let mut state = coordinator.state.write().await;
            let mut xt = PendingXt::new("xt-77777-2".to_string(), b"xt-77777-2".to_vec());
            xt.decision = Some(true);
            xt.decided_at = Some(std::time::Instant::now());
            // confirmed_at is the age reference when set; simulate old confirmation.
            xt.confirmed_at = Some(
                std::time::Instant::now()
                    .checked_sub(Duration::from_secs(400))
                    .unwrap(),
            );
            state.pending.insert(xt.id.clone(), xt);
        }

        coordinator.cleanup(Duration::from_secs(300)).await;

        let state = coordinator.state.read().await;
        assert!(state.pending.is_empty());
    }

    #[tokio::test]
    async fn cleanup_retains_recently_confirmed_xts() {
        let coordinator = DefaultCoordinator::new(
            ChainId(77777),
            None,
            None,
            None,
            None,
            None,
            1000,
            VerificationConfig::default(),
        );

        {
            let mut state = coordinator.state.write().await;
            let mut xt = PendingXt::new("xt-77777-3".to_string(), b"xt-77777-3".to_vec());
            xt.decision = Some(true);
            xt.decided_at = Some(
                std::time::Instant::now()
                    .checked_sub(Duration::from_secs(400))
                    .unwrap(),
            );
            // decided_at is old but confirmed_at is recent - should be retained.
            xt.confirmed_at = Some(std::time::Instant::now());
            state.pending.insert(xt.id.clone(), xt);
        }

        coordinator.cleanup(Duration::from_secs(300)).await;

        let state = coordinator.state.read().await;
        assert_eq!(state.pending.len(), 1);
    }
}

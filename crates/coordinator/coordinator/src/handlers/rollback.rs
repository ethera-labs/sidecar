//! Rollback handling for aborting undecided instances.

use compose_primitives::{PeriodId, SuperblockNumber};
use tracing::warn;

use crate::coordinator::DefaultCoordinator;
use compose_primitives_traits::CoordinatorError;

impl DefaultCoordinator {
    /// Abort all undecided instances and reset period state.
    pub async fn handle_rollback(
        &self,
        period_id: PeriodId,
        last_finalized_superblock_num: u64,
        _last_finalized_superblock_hash: &[u8],
    ) -> Result<(), CoordinatorError> {
        let mut state = self.state.write().await;

        let mut aborted = 0;
        for xt in state.pending.values_mut() {
            if xt.decision.is_some() {
                continue;
            }
            xt.record_decision(false);
            aborted += 1;
        }

        state.period_initialized = false;
        state.current_period_id = period_id;
        state.current_superblock_num = SuperblockNumber(last_finalized_superblock_num + 1);
        state.last_sequence_num = Default::default();
        state.last_known_blocks.clear();
        state.chain_overlay.clear();
        state.mailbox_buffer.clear();
        state.pending_submissions.clear();

        warn!(
            period_id = period_id.0,
            last_finalized_superblock = last_finalized_superblock_num,
            aborted_instances = aborted,
            "Rollback received, all undecided instances aborted"
        );

        drop(state);

        if let Some(builder) = &self.put_inbox_builder {
            let b = builder.clone();
            if let Err(e) = self
                .nonce_manager
                .resync(move || {
                    let b = b.clone();
                    async move { b.pending_nonce_at().await }
                })
                .await
            {
                warn!(error = %e, "Failed to resync putInbox nonce on rollback");
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use compose_primitives::{ChainId, PeriodId, SuperblockNumber};

    use crate::coordinator::DefaultCoordinator;

    #[tokio::test]
    async fn handle_rollback_updates_period_and_superblock() {
        let coordinator =
            DefaultCoordinator::new(ChainId(77777), None, None, None, None, None, None, 1000);

        // Set initial state.
        {
            let mut state = coordinator.state.write().await;
            state.current_period_id = PeriodId(10);
            state.current_superblock_num = SuperblockNumber(100);
            state.period_initialized = true;
        }

        coordinator
            .handle_rollback(PeriodId(8), 50, b"hash")
            .await
            .unwrap();

        let state = coordinator.state.read().await;
        assert_eq!(state.current_period_id, PeriodId(8));
        assert_eq!(state.current_superblock_num, SuperblockNumber(51));
        assert!(!state.period_initialized);
    }
}

//! Start-period handling and period state transitions.

use compose_primitives::{PeriodId, SuperblockNumber};
use tracing::{error, info, warn};

use crate::coordinator::DefaultCoordinator;
use compose_primitives_traits::CoordinatorError;

impl DefaultCoordinator {
    /// Handle a new period from the publisher. Aborts any stale undecided
    /// instances from prior periods and sends abort votes to the publisher
    /// so it can complete the 2PC for those instances.
    pub async fn handle_start_period(
        &self,
        period_id: PeriodId,
        superblock_num: SuperblockNumber,
    ) -> Result<(), CoordinatorError> {
        let aborted_instance_ids: Vec<Vec<u8>> = {
            let mut state = self.state.write().await;

            let mut aborted_ids = Vec::new();
            for xt in state.pending.values_mut() {
                if xt.decision.is_some() || xt.period_id.0 == 0 || xt.period_id >= period_id {
                    continue;
                }
                aborted_ids.push(xt.instance_id.clone());
                xt.record_decision(false);
            }

            state.current_period_id = period_id;
            state.current_superblock_num = superblock_num;
            state.period_initialized = true;
            state.last_sequence_num = Default::default();
            state.last_known_blocks.clear();
            state.chain_overlay.clear();

            info!(
                period_id = period_id.0,
                superblock_num = superblock_num.0,
                aborted_stale = aborted_ids.len(),
                "Started new period"
            );

            aborted_ids
        }; // write lock released before async operations

        // Nonce resync and vote sending are async; they must run after the
        // write lock is released to avoid holding it across await points.
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
                warn!(error = %e, "Failed to resync putInbox nonce on period change");
            }
        }

        // Notify the publisher of the abort for each stale XT so it can
        // complete the 2PC round and unblock the next period's instances.
        if !aborted_instance_ids.is_empty() {
            if let Some(publisher) = &self.publisher {
                if publisher.is_connected() {
                    for instance_id in &aborted_instance_ids {
                        if let Err(e) = publisher.send_vote(instance_id, false).await {
                            error!(error = %e, "Failed to send abort vote for stale XT");
                        }
                    }
                }
            }
        }

        Ok(())
    }
}

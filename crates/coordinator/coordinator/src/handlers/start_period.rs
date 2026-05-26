//! Start-period handling and period state transitions.

use compose_primitives::{PeriodId, SuperblockNumber};
use tracing::{error, info};

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
        let (aborted_instance_ids, builder_abort_ids): (Vec<Vec<u8>>, Vec<String>) = {
            let mut state = self.state.write().await;

            let mut aborted_ids = Vec::new();
            let mut builder_abort_ids = Vec::new();
            for xt in state.pending.values_mut() {
                if xt.period_id.0 == 0 || xt.period_id >= period_id || xt.confirmed_at.is_some() {
                    continue;
                }
                // A prior-period XT that is decided but unconfirmed at rollover
                // is preempted: tell the builder to drop it and let the lifecycle
                // path recycle its reserved putInbox nonces. This avoids the
                // exact-resync race where the canonical nonce read could move
                // the cursor under an in-flight reservation.
                if let Some(super::builder_control::XtBuilderCommand::Abort { instance_id }) =
                    self.local_builder_command(xt, false)
                {
                    builder_abort_ids.push(instance_id);
                }
                if xt.decision.is_none() {
                    aborted_ids.push(xt.instance_id.clone());
                    xt.record_decision(false);
                }
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

            (aborted_ids, builder_abort_ids)
        }; // write lock released before async operations

        for instance_id in &builder_abort_ids {
            self.apply_builder_command(super::builder_control::XtBuilderCommand::Abort {
                instance_id: instance_id.clone(),
            })
            .await?;
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

#[cfg(test)]
mod tests {
    use compose_primitives::ChainId;

    use super::*;
    use crate::coordinator::VerificationConfig;
    use crate::model::pending_xt::PendingXt;

    #[tokio::test]
    async fn stale_committed_xt_recycles_put_inbox_nonce_when_canonical_unchanged() {
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

        let reserved = coordinator
            .nonce_manager
            .reserve("xt-77777-stale", 1, || async { Ok(0) })
            .await
            .unwrap();
        assert_eq!(reserved, 0);

        {
            let mut state = coordinator.state.write().await;
            let mut xt = PendingXt::new("xt-77777-stale".to_string(), b"xt-77777-stale".to_vec());
            xt.period_id = PeriodId(1);
            xt.record_decision(true);
            xt.raw_txs.insert(ChainId(77777), vec![vec![1]]);
            state.pending.insert(xt.id.clone(), xt);
        }

        coordinator
            .handle_start_period(PeriodId(2), SuperblockNumber(10))
            .await
            .unwrap();

        // Stale XT was aborted at rollover; its nonce returned to the recycled
        // pool and the next XT in the new period claims it.
        let next = coordinator
            .nonce_manager
            .reserve("xt-77777-next", 1, || async { Ok(0) })
            .await
            .unwrap();
        assert_eq!(next, 0);
    }

    #[tokio::test]
    async fn rollover_does_not_reuse_nonce_when_canonical_has_advanced() {
        // Regression for the burst-at-rollover race: a prior-period in-flight
        // putInbox tx lands canonically across the rollover boundary, so the
        // sidecar must not hand its nonce out again.
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

        let reserved = coordinator
            .nonce_manager
            .reserve("xt-77777-stale", 1, || async { Ok(0) })
            .await
            .unwrap();
        assert_eq!(reserved, 0);

        {
            let mut state = coordinator.state.write().await;
            let mut xt = PendingXt::new("xt-77777-stale".to_string(), b"xt-77777-stale".to_vec());
            xt.period_id = PeriodId(1);
            xt.record_decision(true);
            xt.raw_txs.insert(ChainId(77777), vec![vec![1]]);
            state.pending.insert(xt.id.clone(), xt);
        }

        coordinator
            .handle_start_period(PeriodId(2), SuperblockNumber(10))
            .await
            .unwrap();

        // Chain has advanced past the recycled nonce by the time the next XT
        // is decided; the recycled value must be dropped and the new XT must
        // start above canonical.
        let next = coordinator
            .nonce_manager
            .reserve("xt-77777-next", 1, || async { Ok(5) })
            .await
            .unwrap();
        assert_eq!(next, 5);
    }

    #[tokio::test]
    async fn undecided_stale_xt_is_aborted_on_new_period() {
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
            xt.period_id = PeriodId(1);
            xt.raw_txs.insert(ChainId(77777), vec![vec![1]]);
            state.pending.insert(xt.id.clone(), xt);
        }

        coordinator
            .handle_start_period(PeriodId(2), SuperblockNumber(5))
            .await
            .unwrap();

        let state = coordinator.state.read().await;
        let xt = state.pending.get("xt-77777-1").unwrap();
        assert_eq!(xt.decision, Some(false));
    }

    #[tokio::test]
    async fn confirmed_xt_is_not_aborted_on_new_period() {
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
            let mut xt = PendingXt::new(
                "xt-77777-confirmed".to_string(),
                b"xt-77777-confirmed".to_vec(),
            );
            xt.period_id = PeriodId(1);
            xt.record_decision(true);
            xt.confirmed_at = Some(std::time::Instant::now());
            xt.raw_txs.insert(ChainId(77777), vec![vec![1]]);
            state.pending.insert(xt.id.clone(), xt);
        }

        coordinator
            .handle_start_period(PeriodId(2), SuperblockNumber(5))
            .await
            .unwrap();

        let state = coordinator.state.read().await;
        let xt = state.pending.get("xt-77777-confirmed").unwrap();
        assert_eq!(xt.decision, Some(true));
        assert!(xt.confirmed_at.is_some());
    }
}

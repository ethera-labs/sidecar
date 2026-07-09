//! Start-period handling and period state transitions.

use ethera_spec::{PeriodId, SuperblockNumber};
use tracing::{error, info};

use crate::DefaultCoordinator;
use sidecar_primitives_traits::CoordinatorError;

impl DefaultCoordinator {
    /// Handle a new period from the publisher. Aborts any stale undecided
    /// instances from prior periods and sends abort votes to the publisher
    /// so it can finish deciding those instances.
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
                // A decided=true XT may have its putInbox tx already executed in
                // an unfinalized flashblock; tearing it out of the builder pool
                // now would strand the chain at the missing nonce and block
                // every later putInbox in the lane. Let the canonical tracker
                // finish the confirm round-trip on its own.
                if xt.decision == Some(true) {
                    continue;
                }
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

            state.publisher_period.start(period_id);
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
        // finish the decision round and unblock the next period's instances.
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
    use ethera_spec::ChainId;

    use super::*;
    use crate::model::pending_xt::PendingXt;
    use crate::VerificationConfig;

    #[tokio::test]
    async fn decided_committed_xt_keeps_reservation_at_rollover() {
        // Regression: tearing a decided=true XT out of the builder at rollover
        // strands the chain at the missing nonce (its putInbox may already be
        // executed in an unfinalized flashblock). The next reserve must
        // advance past the still-live reservation, not reuse it.
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
            .reserve("xt-77777-decided", 1, || async { Ok(0) })
            .await
            .unwrap();
        assert_eq!(reserved, 0);

        {
            let mut state = coordinator.state.write().await;
            let mut xt =
                PendingXt::new("xt-77777-decided".to_string(), b"xt-77777-decided".to_vec());
            xt.period_id = PeriodId(1);
            xt.record_decision(true);
            xt.raw_txs.insert(ChainId(77777), vec![vec![1]]);
            state.pending.insert(xt.id.clone(), xt);
        }

        coordinator
            .handle_start_period(PeriodId(2), SuperblockNumber(10))
            .await
            .unwrap();

        let next = coordinator
            .nonce_manager
            .reserve("xt-77777-next", 1, || async { Ok(0) })
            .await
            .unwrap();
        assert_eq!(next, 1);
    }

    #[tokio::test]
    async fn rollover_advances_past_live_reservation_when_canonical_caught_up() {
        // Canonical overtakes the live reservation across the rollover; the
        // floor moves up and the stale entry is trimmed on the next reserve.
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
            .reserve("xt-77777-decided", 1, || async { Ok(0) })
            .await
            .unwrap();
        assert_eq!(reserved, 0);

        {
            let mut state = coordinator.state.write().await;
            let mut xt =
                PendingXt::new("xt-77777-decided".to_string(), b"xt-77777-decided".to_vec());
            xt.period_id = PeriodId(1);
            xt.record_decision(true);
            xt.raw_txs.insert(ChainId(77777), vec![vec![1]]);
            state.pending.insert(xt.id.clone(), xt);
        }

        coordinator
            .handle_start_period(PeriodId(2), SuperblockNumber(10))
            .await
            .unwrap();

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

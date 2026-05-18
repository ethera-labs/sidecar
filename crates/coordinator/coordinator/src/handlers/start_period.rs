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
        let (aborted_instance_ids, builder_abort_ids, recovered_local_nonce_lane): (
            Vec<Vec<u8>>,
            Vec<String>,
            bool,
        ) = {
            let mut state = self.state.write().await;

            let mut aborted_ids = Vec::new();
            let mut builder_abort_ids = Vec::new();
            let mut recovered_local_nonce_lane = false;
            for xt in state.pending.values_mut() {
                if xt.period_id.0 == 0 || xt.period_id >= period_id || xt.confirmed_at.is_some() {
                    continue;
                }
                // Edge case: an XT can reach decision=true yet never get builder confirmation
                // if its released putInbox txs fail later. If we carry that stale local
                // reservation into the next period, the deferred putInbox nonce manager can
                // stay ahead of canonical chain nonce and poison later bridge XTs.
                if let Some(super::builder_control::XtBuilderCommand::Abort { instance_id }) =
                    self.local_builder_command(xt, false)
                {
                    builder_abort_ids.push(instance_id);
                    recovered_local_nonce_lane = true;
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

            (aborted_ids, builder_abort_ids, recovered_local_nonce_lane)
        }; // write lock released before async operations

        for instance_id in &builder_abort_ids {
            self.apply_builder_command(super::builder_control::XtBuilderCommand::Abort {
                instance_id: instance_id.clone(),
            })
            .await?;
        }

        // When we explicitly recovered a stale local reservation lane above, we need an exact
        // nonce resync. A monotonic resync would preserve the stale in-memory nonce and keep
        // subsequent putInbox txs one step ahead of the builder execution cursor.
        let nonce_resync = if recovered_local_nonce_lane {
            self.resync_put_inbox_nonce().await
        } else {
            self.resync_put_inbox_nonce_monotonic().await
        };
        if let Err(e) = nonce_resync {
            error!(error = %e, "Failed to resync putInbox nonce on period change");
            self.nonce_manager.reset().await;
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
    use std::sync::Arc;

    use alloy::primitives::Address;
    use async_trait::async_trait;
    use compose_primitives::{ChainId, CrossRollupDependency};
    use compose_primitives_traits::PutInboxBuilder;
    use tokio::sync::Mutex;

    use super::*;
    use crate::coordinator::VerificationConfig;
    use crate::model::pending_xt::PendingXt;

    #[derive(Debug)]
    struct TestPutInboxBuilder {
        canonical_nonce: Mutex<u64>,
    }

    impl TestPutInboxBuilder {
        fn new(canonical_nonce: u64) -> Self {
            Self {
                canonical_nonce: Mutex::new(canonical_nonce),
            }
        }
    }

    #[async_trait]
    impl PutInboxBuilder for TestPutInboxBuilder {
        fn signer_address(&self) -> Address {
            Address::ZERO
        }

        async fn canonical_nonce_at(&self) -> Result<u64, CoordinatorError> {
            Ok(*self.canonical_nonce.lock().await)
        }

        async fn build_put_inbox_tx_with_nonce(
            &self,
            _dep: &CrossRollupDependency,
            nonce: u64,
        ) -> Result<Vec<u8>, CoordinatorError> {
            Ok(nonce.to_be_bytes().to_vec())
        }
    }

    #[tokio::test]
    async fn stale_committed_xt_releases_reserved_put_inbox_nonce_on_new_period() {
        let mut coordinator = DefaultCoordinator::new(
            ChainId(77777),
            None,
            None,
            None,
            None,
            None,
            1000,
            VerificationConfig::default(),
        );
        coordinator.set_put_inbox_builder(Arc::new(TestPutInboxBuilder::new(0)));

        let reserved = coordinator
            .nonce_manager
            .reserve(1, || async { Ok(0) })
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

        let next = coordinator
            .nonce_manager
            .reserve(1, || async { Ok(99) })
            .await
            .unwrap();
        assert_eq!(next, 0);
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

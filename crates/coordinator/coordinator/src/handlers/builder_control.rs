//! Helpers for synchronizing XT lifecycle events with the local builder.

use crate::{
    coordinator::{CoordinatorState, DefaultCoordinator},
    model::pending_xt::PendingXt,
    pipeline::delivery::deps_for_chain,
};
use compose_primitives::CrossRollupDependency;
use compose_primitives_traits::CoordinatorError;
use tracing::{info, warn};

#[derive(Debug, Clone)]
pub(crate) struct XtBuilderSubmission {
    pub instance_id: String,
    pub period_id: u64,
    pub sequence_number: u64,
    pub transactions: Vec<Vec<u8>>,
}

#[derive(Debug, Clone)]
pub(crate) enum XtBuilderCommand {
    Release {
        instance_id: String,
        dependencies: Vec<CrossRollupDependency>,
    },
    Abort {
        instance_id: String,
    },
}

impl DefaultCoordinator {
    pub(crate) fn local_builder_submission(&self, xt: &PendingXt) -> Option<XtBuilderSubmission> {
        let transactions = xt.raw_txs.get(&self.chain_id)?.clone();
        if transactions.is_empty() {
            return None;
        }

        let sequence_number = if xt.sequence_num.0 != 0 {
            xt.sequence_num.0
        } else {
            xt.origin_seq.0
        };

        Some(XtBuilderSubmission {
            instance_id: xt.id.to_string(),
            period_id: xt.period_id.0,
            sequence_number,
            transactions,
        })
    }

    pub(crate) fn local_builder_command(
        &self,
        xt: &PendingXt,
        decision: bool,
    ) -> Option<XtBuilderCommand> {
        if xt
            .raw_txs
            .get(&self.chain_id)
            .is_none_or(|transactions| transactions.is_empty())
        {
            return None;
        }

        if decision {
            Some(XtBuilderCommand::Release {
                instance_id: xt.id.to_string(),
                dependencies: deps_for_chain(&xt.fulfilled_deps, self.chain_id),
            })
        } else {
            Some(XtBuilderCommand::Abort {
                instance_id: xt.id.to_string(),
            })
        }
    }

    pub(crate) async fn submit_xt_to_builder(
        &self,
        submission: XtBuilderSubmission,
    ) -> Result<(), CoordinatorError> {
        if let Some(builder) = &self.xt_builder_client {
            builder
                .submit_locked_xt(
                    &submission.instance_id,
                    submission.period_id,
                    submission.sequence_number,
                    submission.transactions,
                )
                .await?;
        }
        Ok(())
    }

    async fn build_put_inbox_transactions(
        &self,
        dependencies: &[CrossRollupDependency],
    ) -> Result<Vec<Vec<u8>>, CoordinatorError> {
        if dependencies.is_empty() {
            return Ok(Vec::new());
        }

        let builder = self
            .put_inbox_builder
            .as_ref()
            .cloned()
            .ok_or(CoordinatorError::PutInboxNotConfigured)?;
        let nonce_builder = builder.clone();
        let start_nonce = self
            .nonce_manager
            .reserve(dependencies.len(), move || {
                let builder = nonce_builder.clone();
                async move { builder.pending_nonce_at().await }
            })
            .await?;

        let mut nonce = start_nonce;
        let mut transactions = Vec::with_capacity(dependencies.len());
        for dependency in dependencies {
            transactions.push(
                builder
                    .build_put_inbox_tx_with_nonce(dependency, nonce)
                    .await?,
            );
            nonce = nonce.saturating_add(1);
        }

        Ok(transactions)
    }

    pub(crate) async fn resync_put_inbox_nonce(&self) -> Result<(), CoordinatorError> {
        let Some(builder) = self.put_inbox_builder.as_ref().cloned() else {
            return Ok(());
        };

        self.nonce_manager
            .resync(move || {
                let builder = builder.clone();
                async move { builder.pending_nonce_at().await }
            })
            .await
    }

    pub(crate) async fn apply_builder_command(
        &self,
        command: XtBuilderCommand,
    ) -> Result<(), CoordinatorError> {
        let Some(builder) = &self.xt_builder_client else {
            return Ok(());
        };

        match command {
            XtBuilderCommand::Release {
                instance_id,
                dependencies,
            } => {
                let put_inbox_transactions =
                    self.build_put_inbox_transactions(&dependencies).await?;
                if let Err(err) = builder
                    .release_xt(&instance_id, put_inbox_transactions)
                    .await
                {
                    if let Err(resync_err) = self.resync_put_inbox_nonce().await {
                        warn!(error = %resync_err, "Failed to resync putInbox nonce after release error");
                    }
                    return Err(err);
                }
            }
            XtBuilderCommand::Abort { instance_id } => {
                builder.abort_xt(&instance_id).await?;
            }
        }

        Ok(())
    }

    pub(crate) async fn remove_pending_xt(&self, instance_id: &str) -> bool {
        let mut state = self.state.write().await;
        Self::remove_pending_xt_from_state(&mut state, instance_id)
    }

    pub async fn confirm_included_xts(
        &self,
        instance_ids: &[String],
    ) -> Result<(), CoordinatorError> {
        let mut state = self.state.write().await;
        let now = std::time::Instant::now();
        for instance_id in instance_ids {
            if let Some(xt) = state.pending.get_mut(instance_id.as_str()) {
                xt.confirmed_at = Some(now);
                info!(instance_id = %instance_id, "XT confirmed included by builder");
            } else {
                warn!(instance_id = %instance_id, "confirm received for unknown XT");
            }
        }
        Ok(())
    }

    fn remove_pending_xt_from_state(state: &mut CoordinatorState, instance_id: &str) -> bool {
        let Some(xt) = state.pending.remove(instance_id) else {
            return false;
        };

        state.mailbox_index.remove(xt.instance_id.as_slice());
        state
            .submitted_fingerprints
            .retain(|_, pending_id| pending_id.as_str() != instance_id);
        state.pending_submissions.retain(|_, waiters| {
            waiters.retain(|sender| !sender.is_closed());
            !waiters.is_empty()
        });

        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use compose_primitives::{ChainId, PeriodId, SequenceNumber};

    #[tokio::test]
    async fn confirm_included_xts_keeps_pending_for_status_polling() {
        let coordinator =
            DefaultCoordinator::new(ChainId(77777), None, None, None, None, None, 1000);

        {
            let mut state = coordinator.state.write().await;
            let mut xt = PendingXt::new("xt-77777-1".to_string(), b"xt-77777-1".to_vec());
            xt.raw_txs.insert(ChainId(77777), vec![vec![1]]);
            state
                .mailbox_index
                .insert(xt.instance_id.clone(), xt.id.clone());
            state
                .submitted_fingerprints
                .insert("fp-1".to_string(), xt.id.clone());
            state.pending.insert(xt.id.clone(), xt);
        }

        coordinator
            .confirm_included_xts(&["xt-77777-1".to_string()])
            .await
            .unwrap();

        // XTs must remain in pending so GET /xt/:id continues to return their
        // committed status while callers poll WaitForDecision. The cleanup loop
        // removes decided XTs after they age out (decided_at + max_age).
        let state = coordinator.state.read().await;
        assert!(state.pending.contains_key("xt-77777-1"));
        assert!(state.mailbox_index.contains_key(b"xt-77777-1".as_slice()));
        assert!(state.submitted_fingerprints.contains_key("fp-1"));
    }

    #[test]
    fn local_builder_submission_prefers_publisher_sequence() {
        let coordinator =
            DefaultCoordinator::new(ChainId(77777), None, None, None, None, None, 1000);
        let mut xt = PendingXt::new("xt-77777-2".to_string(), b"xt-77777-2".to_vec());
        xt.period_id = PeriodId(11);
        xt.sequence_num = SequenceNumber(7);
        xt.origin_seq = SequenceNumber(3);
        xt.raw_txs.insert(ChainId(77777), vec![vec![1], vec![2]]);

        let submission = coordinator.local_builder_submission(&xt).unwrap();
        assert_eq!(submission.period_id, 11);
        assert_eq!(submission.sequence_number, 7);
        assert_eq!(submission.transactions.len(), 2);
    }
}

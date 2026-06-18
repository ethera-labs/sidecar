//! Start-instance handling and sequencing validation.

use std::collections::HashMap;

use ethera_spec::{chains_from_request, ChainId, Instance as SpecInstance};
use ethera_spec_proto::StartInstance;
use sidecar_primitives::InstanceId;
use tracing::{debug, error, info, warn};

use crate::coordinator::DefaultCoordinator;
use crate::model::pending_xt::PendingXt;
use crate::pipeline::delivery::build_sender_nonce_cache;
use crate::pipeline::submission::xt_request_fingerprint;
use crate::MAX_PENDING_XTS;
use sidecar_primitives_traits::CoordinatorError;

impl DefaultCoordinator {
    /// Process a new instance from the publisher. Validates the period and
    /// sequence, decodes transactions, and registers the XT.
    pub async fn handle_start_instance(&self, msg: &StartInstance) -> Result<(), CoordinatorError> {
        let Some(proto_xt_request) = msg.xt_request.as_ref() else {
            return Err(CoordinatorError::Other("missing xt_request".to_string()));
        };

        let xt_request = ethera_spec::XtRequest::from(proto_xt_request);
        let fingerprint = xt_request_fingerprint(&xt_request);
        let spec_instance = match SpecInstance::try_from(msg) {
            Ok(instance) => instance,
            Err(err) => {
                let instance_id = InstanceId::from_publisher_bytes(&msg.instance_id);
                let error = format!("invalid start-instance: {err}");
                self.resolve_pending_submission(&fingerprint, Err(error.clone()))
                    .await;
                warn!(
                    instance_id = %instance_id,
                    period_id = msg.period_id,
                    sequence = msg.sequence_number,
                    error,
                    "StartInstance rejected"
                );
                self.reject_start_instance(&instance_id, msg).await;
                return Ok(());
            }
        };

        let instance_id = InstanceId::from_publisher_bytes(spec_instance.id.as_bytes());
        let participant_chains = chains_from_request(&spec_instance.xt_request);

        // Decode transactions per chain.
        let mut raw_txs: HashMap<ChainId, Vec<Vec<u8>>> = HashMap::new();
        for req in &spec_instance.xt_request.transactions {
            for tx_bytes in &req.transactions {
                raw_txs
                    .entry(req.chain_id)
                    .or_default()
                    .push(tx_bytes.clone());
            }
        }

        let includes_local = participant_chains.contains(&self.chain_id)
            && raw_txs
                .get(&self.chain_id)
                .is_some_and(|txs| !txs.is_empty());
        let sender_nonces = build_sender_nonce_cache(&raw_txs);

        let mut state = self.state.write().await;

        if state.pending.contains_key(&instance_id) {
            return Err(CoordinatorError::InstanceAlreadyPending(
                instance_id.to_string(),
            ));
        }

        let undecided_count = state
            .pending
            .values()
            .filter(|xt| xt.decision.is_none())
            .count();
        if undecided_count >= MAX_PENDING_XTS {
            let error = CoordinatorError::TooManyPendingInstances(MAX_PENDING_XTS).to_string();
            drop(state);
            self.resolve_pending_submission(&fingerprint, Err(error))
                .await;
            if let Some(m) = &self.metrics {
                m.xt_received_total.inc();
            }
            return Err(CoordinatorError::TooManyPendingInstances(MAX_PENDING_XTS));
        }

        let msg_period = spec_instance.period_id;
        let msg_seq = spec_instance.sequence_number;
        if let Err(err) = state
            .publisher_period
            .accept_start_instance(msg_period, msg_seq)
        {
            drop(state);
            self.resolve_pending_submission(&fingerprint, Err(err.to_string()))
                .await;
            warn!(
                instance_id = %instance_id,
                period_id = msg.period_id,
                sequence = msg.sequence_number,
                error = %err,
                "StartInstance rejected"
            );
            self.reject_start_instance(&instance_id, msg).await;
            return Ok(());
        }

        let raw_instance_id = spec_instance.id.as_bytes().to_vec();
        let mut xt = PendingXt::new(instance_id.to_string(), raw_instance_id.clone());
        xt.period_id = msg_period;
        xt.sequence_num = msg_seq;
        xt.raw_txs = raw_txs;
        xt.sender_nonces = sender_nonces;

        // Pre-lock so only one local simulation task claims this XT.
        if includes_local {
            xt.locked_chains.insert(self.chain_id);
        }

        state
            .mailbox_index
            .insert(raw_instance_id.clone(), instance_id.clone());
        state.pending.insert(instance_id.clone(), xt);

        // Drain messages that arrived before the XT was registered (race window).
        let buffered = state.drain_mailbox_buffer(&raw_instance_id);
        if !buffered.is_empty() {
            if let Some(pending_xt) = state.pending.get_mut(&instance_id) {
                debug!(
                    instance_id = %instance_id,
                    count = buffered.len(),
                    "Attaching buffered mailbox messages to new instance"
                );
                pending_xt.pending_mailbox.extend(buffered);
            }
        }

        info!(
            instance_id = %instance_id,
            period_id = msg.period_id,
            sequence = msg.sequence_number,
            chains = state.pending[&instance_id].raw_txs.len(),
            "New instance started"
        );

        if let Some(m) = &self.metrics {
            m.xt_received_total.inc();
            m.xt_pending_count.inc();
        }

        let local_submission = state
            .pending
            .get(&instance_id)
            .and_then(|xt| self.local_builder_submission(xt));

        // Release the write lock before spawning so process_xt can acquire it.
        drop(state);

        if let Some(submission) = local_submission {
            if let Err(err) = self.submit_xt_to_builder(submission).await {
                self.remove_pending_xt(&instance_id).await;
                self.resolve_pending_submission(&fingerprint, Err(err.to_string()))
                    .await;
                self.reject_start_instance(&instance_id, msg).await;
                return Err(err);
            }
        }

        self.resolve_pending_submission(&fingerprint, Ok(instance_id.clone()))
            .await;

        if includes_local {
            let coordinator = self.clone();
            let id = instance_id.clone();
            self.task_tracker.spawn(async move {
                coordinator.process_xt(&id).await;
            });
        }

        Ok(())
    }

    async fn reject_start_instance(&self, instance_id: &str, msg: &StartInstance) {
        warn!(
            instance_id,
            period_id = msg.period_id,
            sequence = msg.sequence_number,
            "Rejecting StartInstance"
        );
        if let Some(m) = &self.metrics {
            m.xt_rejected_total.inc();
        }

        // Send abort vote directly to the publisher, bypassing send_vote()
        // which requires the XT to exist in pending state. In standalone mode
        // this is a no-op - there's no XT to track and no publisher to notify.
        if let Some(publisher) = &self.publisher {
            if publisher.is_connected() {
                if let Err(e) = publisher.send_vote(&msg.instance_id, false).await {
                    error!(instance_id, error = %e, "Failed to send reject vote to publisher");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use ethera_spec::{ChainId, PeriodId};
    use ethera_spec_proto::{StartInstance, TransactionRequest, XtRequest};

    use crate::coordinator::{DefaultCoordinator, VerificationConfig};

    fn start_instance(sequence_number: u64) -> StartInstance {
        let mut instance_id = [0_u8; 32];
        instance_id[24..].copy_from_slice(&sequence_number.to_be_bytes());

        StartInstance {
            instance_id: instance_id.to_vec(),
            period_id: 1,
            sequence_number,
            xt_request: Some(XtRequest {
                transaction_requests: vec![TransactionRequest {
                    chain_id: 77777,
                    transaction: vec![vec![sequence_number as u8]],
                }],
            }),
        }
    }

    fn start_instance_for_period(period_id: u64, sequence_number: u64) -> StartInstance {
        StartInstance {
            period_id,
            ..start_instance(sequence_number)
        }
    }

    #[tokio::test]
    async fn handle_start_instance_allows_multiple_local_xts() {
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
            state.publisher_period.start(PeriodId(1));
        }

        coordinator
            .handle_start_instance(&start_instance(1))
            .await
            .unwrap();
        coordinator
            .handle_start_instance(&start_instance(2))
            .await
            .unwrap();

        let state = coordinator.state.read().await;
        assert_eq!(state.pending.len(), 2);
    }

    #[tokio::test]
    async fn handle_start_instance_rejects_non_advancing_sequence() {
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
            state.publisher_period.start(PeriodId(1));
        }

        coordinator
            .handle_start_instance(&start_instance(2))
            .await
            .unwrap();

        // A fresh instance reusing sequence 2 is rejected by the watermark.
        let mut replay = start_instance(2);
        replay.instance_id = [0xff; 32].to_vec();
        coordinator.handle_start_instance(&replay).await.unwrap();

        let state = coordinator.state.read().await;
        assert_eq!(state.pending.len(), 1);
    }

    #[tokio::test]
    async fn handle_start_instance_rejects_malformed_instance_id() {
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
            state.publisher_period.start(PeriodId(1));
        }

        let mut malformed = start_instance(1);
        malformed.instance_id = b"not-32-bytes".to_vec();

        coordinator.handle_start_instance(&malformed).await.unwrap();

        let state = coordinator.state.read().await;
        assert!(state.pending.is_empty());
    }

    #[tokio::test]
    async fn handle_start_instance_rejects_period_before_start_period() {
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

        coordinator
            .handle_start_instance(&start_instance(1))
            .await
            .unwrap();

        let state = coordinator.state.read().await;
        assert!(state.pending.is_empty());
    }

    #[tokio::test]
    async fn handle_start_instance_rejects_stale_and_future_periods() {
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
            state.publisher_period.start(PeriodId(10));
        }

        coordinator
            .handle_start_instance(&start_instance_for_period(9, 1))
            .await
            .unwrap();
        coordinator
            .handle_start_instance(&start_instance_for_period(11, 2))
            .await
            .unwrap();

        let state = coordinator.state.read().await;
        assert!(state.pending.is_empty());
    }

    #[tokio::test]
    async fn handle_start_instance_resets_sequence_on_new_period() {
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
            state.publisher_period.start(PeriodId(1));
        }

        coordinator
            .handle_start_instance(&start_instance(3))
            .await
            .unwrap();

        {
            let mut state = coordinator.state.write().await;
            state.publisher_period.start(PeriodId(2));
        }

        coordinator
            .handle_start_instance(&start_instance_for_period(2, 1))
            .await
            .unwrap();

        let state = coordinator.state.read().await;
        assert_eq!(state.pending.len(), 2);
    }
}

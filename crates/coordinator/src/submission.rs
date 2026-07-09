use std::collections::HashMap;
use std::time::Duration;

use ethera_spec::{ChainId, SequenceNumber};
use ethera_spec_proto::Payload;
use prost::Message;
use sidecar_primitives::InstanceId;
use sidecar_primitives_traits::CoordinatorError;
use tokio::sync::oneshot;
use tracing::{error, info, warn};

use crate::model::pending_xt::PendingXt;
use crate::pipeline::submission::{build_xt_request, xt_request_fingerprint};
use crate::MAX_PENDING_XTS;

use super::state::{PendingSubmissionResult, PendingSubmissionSender};
use super::DefaultCoordinator;

impl DefaultCoordinator {
    pub(crate) async fn resolve_pending_submission(
        &self,
        fingerprint: &str,
        result: PendingSubmissionResult,
    ) {
        let waiters = self
            .state
            .write()
            .await
            .pending_submissions
            .remove(fingerprint);
        if let Some(waiters) = waiters {
            Self::notify_pending_submission_waiters(waiters, &result);
        }
    }

    pub(crate) fn notify_pending_submission_waiters(
        waiters: Vec<PendingSubmissionSender>,
        result: &PendingSubmissionResult,
    ) {
        for waiter in waiters {
            let _ = waiter.send(result.clone());
        }
    }

    /// Submit a cross-chain transaction.
    ///
    /// In publisher-connected mode, the XT is sent to the publisher, which
    /// assigns the instance ID. In standalone mode, a local ID is generated and
    /// the XT is forwarded to peer sidecars.
    pub async fn submit_xt(
        &self,
        txs: HashMap<ChainId, Vec<Vec<u8>>>,
    ) -> Result<String, CoordinatorError> {
        if txs.is_empty() {
            return Err(CoordinatorError::NoTransactions);
        }
        if txs.len() < 2 {
            return Err(CoordinatorError::Other(
                "cross-chain transaction must span at least 2 chains".to_string(),
            ));
        }

        if self.is_publisher_connected().await {
            self.submit_xt_publisher(txs).await
        } else {
            self.submit_xt_standalone(txs).await
        }
    }

    async fn submit_xt_publisher(
        &self,
        txs: HashMap<ChainId, Vec<Vec<u8>>>,
    ) -> Result<String, CoordinatorError> {
        let publisher = self
            .publisher
            .as_ref()
            .ok_or(CoordinatorError::PublisherNotConnected)?;

        let xt_request = build_xt_request(&txs);
        let fingerprint = xt_request_fingerprint(&xt_request);

        let (tx, rx) = oneshot::channel();
        let should_send = {
            let mut state = self.state.write().await;
            let waiters = state
                .pending_submissions
                .entry(fingerprint.clone())
                .or_default();
            let should_send = waiters.is_empty();
            waiters.push(tx);
            should_send
        };

        if should_send {
            let wire_xt_request = ethera_spec_proto::XtRequest::from(&xt_request);
            let wire = ethera_spec_proto::Message {
                sender_id: String::new(),
                payload: Some(Payload::XtRequest(wire_xt_request)),
            };
            let data = wire.encode_to_vec();

            if let Err(e) = publisher.send_raw(&data).await {
                let message = format!("failed to send XT to publisher: {e}");
                self.resolve_pending_submission(&fingerprint, Err(message.clone()))
                    .await;
                return Err(CoordinatorError::Other(message));
            }
        }

        // Wait for the publisher to respond with StartInstance, which carries
        // the canonical instance_id.
        let instance_id = tokio::time::timeout(Duration::from_secs(10), rx)
            .await
            .map_err(|_| {
                CoordinatorError::Other(
                    "timed out waiting for publisher to assign instance_id".to_string(),
                )
            })?
            .map_err(|_| {
                CoordinatorError::Other(
                    "publisher submission resolution dropped unexpectedly".to_string(),
                )
            })?
            .map_err(CoordinatorError::Other)?;

        info!(instance_id = %instance_id, "Submitted XT to publisher");
        Ok(instance_id.to_string())
    }

    async fn submit_xt_standalone(
        &self,
        txs: HashMap<ChainId, Vec<Vec<u8>>>,
    ) -> Result<String, CoordinatorError> {
        // Compute fingerprint before acquiring the lock to detect duplicates.
        let xt_request = build_xt_request(&txs);
        let fingerprint = xt_request_fingerprint(&xt_request);

        let (instance_id, txs_for_forward, local_submission) = {
            let mut state = self.state.write().await;

            // Return the existing instance ID for duplicate submissions, as long
            // as the original XT is still pending. Once cleaned up, re-submission
            // is allowed (the fingerprint entry is pruned by cleanup).
            if let Some(existing_id) = state.submitted_fingerprints.get(&fingerprint) {
                if state.pending.contains_key(existing_id.as_str()) {
                    let id = existing_id.clone();
                    info!(instance_id = %id, "Duplicate XT submission, returning existing ID");
                    return Ok(id.to_string());
                }
                // Original was cleaned up; remove the stale fingerprint entry.
                state.submitted_fingerprints.remove(&fingerprint);
            }

            let undecided_count = state
                .pending
                .values()
                .filter(|xt| xt.decision.is_none())
                .count();
            if undecided_count >= MAX_PENDING_XTS {
                return Err(CoordinatorError::TooManyPendingInstances(MAX_PENDING_XTS));
            }

            state.origin_seq = SequenceNumber(state.origin_seq.0 + 1);
            let seq = state.origin_seq;
            let id = InstanceId::standalone(self.chain_id, seq.0);

            // Clone only for forwarding when a peer coordinator is configured;
            // `txs` itself is moved into the XT to avoid an unconditional clone.
            let txs_for_forward = self.peer_coordinator.as_ref().map(|_| txs.clone());

            let mut xt = PendingXt::new(id.to_string(), id.as_bytes().to_vec());
            xt.origin_chain = Some(self.chain_id);
            xt.origin_seq = seq;
            xt.raw_txs = txs;
            // Pre-lock so only one local simulation task claims this XT.
            xt.locked_chains.insert(self.chain_id);

            state
                .mailbox_index
                .insert(id.as_bytes().to_vec(), id.clone());
            state.pending.insert(id.clone(), xt);
            state.submitted_fingerprints.insert(fingerprint, id.clone());
            let local_submission = state
                .pending
                .get(&id)
                .and_then(|xt| self.local_builder_submission(xt));
            (id, txs_for_forward, local_submission)
        }; // write lock released here

        if let Some(submission) = local_submission {
            if let Err(err) = self.submit_xt_to_builder(submission).await {
                self.remove_pending_xt(&instance_id).await;
                return Err(err);
            }
        }

        if let Some(m) = &self.metrics {
            m.xt_received_total.inc();
            m.xt_pending_count.inc();
        }
        info!(instance_id = %instance_id, "Submitted XT locally (standalone mode)");

        // Start simulation immediately after local registration.
        {
            let coordinator = self.clone();
            let id = instance_id.clone();
            self.task_tracker.spawn(async move {
                coordinator.process_xt(&id).await;
            });
        }

        if let Some(peer_coordinator) = &self.peer_coordinator {
            let id = instance_id.clone();
            let chain_id = self.chain_id;
            let origin_seq = {
                let state = self.state.read().await;
                state.origin_seq
            };
            let pc = peer_coordinator.clone();
            // txs_for_forward is Some(_) whenever peer_coordinator is Some.
            let txs = txs_for_forward.expect("cloned above when peer_coordinator is set");
            self.task_tracker.spawn(async move {
                if let Err(e) = pc.forward_xt(&id, &txs, chain_id, origin_seq).await {
                    error!(instance_id = %id, error = %e, "Failed to forward XT to peers");
                }
            });
        } else {
            warn!(
                instance_id = %instance_id,
                "No peer coordinator configured, XT will only be processed locally"
            );
        }

        Ok(instance_id.to_string())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use async_trait::async_trait;
    use ethera_spec::{ChainId, PeriodId};
    use sidecar_primitives_traits::{CoordinatorError, PublisherClient};
    use tokio::sync::oneshot;
    use tokio::time::sleep;

    use super::*;
    use crate::{DefaultCoordinator, VerificationConfig};

    #[derive(Debug, Default)]
    struct MockPublisher {
        send_raw_calls: AtomicUsize,
    }

    #[async_trait]
    impl PublisherClient for MockPublisher {
        async fn connect(&self) -> Result<(), CoordinatorError> {
            Ok(())
        }

        async fn connect_with_retry(&self) -> Result<(), CoordinatorError> {
            Ok(())
        }

        async fn disconnect(&self) -> Result<(), CoordinatorError> {
            Ok(())
        }

        async fn send_vote(
            &self,
            _instance_id: &[u8],
            _vote: bool,
        ) -> Result<(), CoordinatorError> {
            Ok(())
        }

        async fn send_raw(&self, _data: &[u8]) -> Result<(), CoordinatorError> {
            self.send_raw_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn is_connected(&self) -> bool {
            true
        }
    }

    #[tokio::test]
    async fn submit_xt_publisher_joins_duplicate_waiters() {
        let publisher = Arc::new(MockPublisher::default());
        let coordinator = DefaultCoordinator::new(
            ChainId(77777),
            None,
            Some(publisher.clone()),
            None,
            None,
            None,
            1000,
            VerificationConfig::default(),
        );

        let mut txs = HashMap::new();
        txs.insert(ChainId(77777), vec![vec![0x01, 0x02, 0x03]]);
        txs.insert(ChainId(88888), vec![vec![0x04, 0x05, 0x06]]);
        let fingerprint = xt_request_fingerprint(&build_xt_request(&txs));

        let coordinator_a = coordinator.clone();
        let txs_a = txs.clone();
        let first = tokio::spawn(async move { coordinator_a.submit_xt(txs_a).await });

        let coordinator_b = coordinator.clone();
        let txs_b = txs.clone();
        let second = tokio::spawn(async move { coordinator_b.submit_xt(txs_b).await });

        // Wait until both tasks are parked in pending_submissions AND the
        // publisher call has been made. The two conditions can briefly diverge:
        // the winning task releases the state lock (incrementing waiter_count)
        // before it calls send_raw(), so checking both together is required to
        // avoid a race.
        for _ in 0..100 {
            let waiter_count = coordinator
                .state
                .read()
                .await
                .pending_submissions
                .get(&fingerprint)
                .map(Vec::len)
                .unwrap_or_default();
            let send_calls = publisher.send_raw_calls.load(Ordering::SeqCst);
            if waiter_count == 2 && send_calls == 1 {
                break;
            }
            sleep(Duration::from_millis(10)).await;
        }

        assert_eq!(publisher.send_raw_calls.load(Ordering::SeqCst), 1);

        coordinator
            .resolve_pending_submission(&fingerprint, Ok(InstanceId::from("xt-77777-1")))
            .await;

        assert_eq!(first.await.unwrap().unwrap(), "xt-77777-1");
        assert_eq!(second.await.unwrap().unwrap(), "xt-77777-1");
    }

    #[tokio::test]
    async fn rollback_resolves_pending_submission_waiters() {
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
        let (tx, rx) = oneshot::channel();

        {
            let mut state = coordinator.state.write().await;
            state
                .pending_submissions
                .insert("fp-1".to_string(), vec![tx]);
        }

        coordinator
            .handle_rollback(PeriodId(1), 5, b"hash")
            .await
            .unwrap();

        let result = rx.await.unwrap();
        assert_eq!(
            result,
            Err("publisher submission aborted by rollback".to_string())
        );
    }
}

use crate::model::pending_xt::PendingXt;
use crate::model::xt_status::{determine_xt_status, XtStatusResponse};
use sidecar_primitives_traits::CoordinatorError;

use super::DefaultCoordinator;

impl DefaultCoordinator {
    /// Query the status of a cross-chain transaction by instance ID.
    pub async fn get_xt_status(
        &self,
        instance_id: &str,
    ) -> Result<XtStatusResponse, CoordinatorError> {
        let state = self.state.read().await;
        let xt = state
            .pending
            .get(instance_id)
            .ok_or_else(|| CoordinatorError::InstanceNotFound(instance_id.to_string()))?;

        Ok(XtStatusResponse {
            instance_id: instance_id.to_string(),
            status: determine_xt_status(xt),
            decision: xt.decision,
        })
    }

    /// Whether the publisher connection is currently active.
    pub(crate) async fn is_publisher_connected(&self) -> bool {
        self.publisher.as_ref().is_some_and(|p| p.is_connected())
    }

    /// In standalone mode, compute whether the instance can be decided from the
    /// currently known local and peer votes.
    ///
    /// Decision rules:
    /// - any `false` vote decides `false` immediately;
    /// - `true` is decided only when all expected votes are collected.
    pub(crate) fn maybe_make_standalone_decision(
        &self,
        xt: &mut PendingXt,
    ) -> Option<(bool, usize, usize)> {
        if xt.decision.is_some() {
            return None;
        }

        let expected = xt.raw_txs.len();
        let mut collected = 0usize;
        let mut has_abort_vote = false;

        if let Some(local) = xt.local_vote {
            collected += 1;
            if !local {
                has_abort_vote = true;
            }
        }

        for (cid, &vote) in &xt.peer_votes {
            if *cid == self.chain_id {
                continue;
            }
            collected += 1;
            if !vote {
                has_abort_vote = true;
            }
        }

        if has_abort_vote {
            xt.record_decision(false);
            return Some((false, collected, expected));
        }

        if expected > 0 && collected >= expected {
            xt.record_decision(true);
            return Some((true, collected, expected));
        }

        None
    }
}

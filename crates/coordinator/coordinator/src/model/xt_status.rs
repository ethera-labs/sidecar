//! Status derivation and API response types for XT state.

use compose_primitives::XtStatus;
use serde::{Deserialize, Serialize};

use crate::model::pending_xt::PendingXt;

/// Response for an XT status query.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct XtStatusResponse {
    pub instance_id: String,
    pub status: XtStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision: Option<bool>,
}

/// Determine the current status of a pending XT.
pub fn determine_xt_status(xt: &PendingXt) -> XtStatus {
    if let Some(decision) = xt.decision {
        return if decision {
            if xt.confirmed_at.is_some() {
                XtStatus::Committed
            } else {
                XtStatus::Voted
            }
        } else {
            XtStatus::Aborted
        };
    }
    if xt.vote_sent {
        return XtStatus::Voted;
    }
    if xt.simulated_at.is_some() {
        if !xt.dependencies.is_empty() && xt.fulfilled_deps.len() < xt.dependencies.len() {
            return XtStatus::WaitingCirc;
        }
        return XtStatus::Simulated;
    }
    if !xt.locked_chains.is_empty() {
        return XtStatus::Simulating;
    }
    XtStatus::Pending
}

#[cfg(test)]
mod tests {
    use compose_primitives::XtStatus;

    use crate::model::pending_xt::PendingXt;
    use crate::model::xt_status::determine_xt_status;

    #[test]
    fn accepted_xt_is_not_committed_before_builder_confirmation() {
        let mut xt = PendingXt::new("xt-1".to_string(), b"xt-1".to_vec());
        xt.record_decision(true);

        assert_eq!(determine_xt_status(&xt), XtStatus::Voted);
    }

    #[test]
    fn accepted_xt_is_committed_after_builder_confirmation() {
        let mut xt = PendingXt::new("xt-2".to_string(), b"xt-2".to_vec());
        xt.record_decision(true);
        xt.confirmed_at = Some(std::time::Instant::now());

        assert_eq!(determine_xt_status(&xt), XtStatus::Committed);
    }
}

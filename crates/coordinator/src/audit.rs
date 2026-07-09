use std::time::{SystemTime, UNIX_EPOCH};

use alloy::primitives::{Address, B256};
use sidecar_metrics::PermissionDenialLabels;
use sidecar_permissions::{DenialAction, DenyReason, PermissionDenial};
use tracing::warn;

use super::DefaultCoordinator;

impl DefaultCoordinator {
    /// Record a denied transaction.
    pub fn record_tx_denial(
        &self,
        from: Address,
        action: DenialAction,
        reason: DenyReason,
        tx_hash: Option<B256>,
    ) {
        self.record_denial(PermissionDenial::transaction(
            self.chain_id.0,
            from,
            action,
            reason,
            tx_hash,
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
        ));
    }

    /// Record a denied cross-rollup instance.
    pub fn record_xt_denial(&self, from: Address, reason: DenyReason, instance_id: String) {
        self.record_denial(PermissionDenial::cross_rollup(
            self.chain_id.0,
            from,
            reason,
            instance_id,
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
        ));
    }

    /// Count the denial and, for policy denials, post it to the audit webhook
    /// (fire-and-forget so admission and voting are never blocked).
    fn record_denial(&self, denial: PermissionDenial) {
        if let Some(metrics) = &self.metrics {
            metrics
                .permission_denied_total
                .get_or_create(&PermissionDenialLabels {
                    action: denial.action.as_str().to_string(),
                    reason: denial.reason.code().to_string(),
                })
                .inc();
        }

        if !denial.reason.is_policy() {
            return;
        }
        let Some(webhook) = self.audit_webhook.clone() else {
            return;
        };
        let metrics = self.metrics.clone();
        self.task_tracker.spawn(async move {
            match webhook.post(&denial).await {
                Ok(()) => {
                    if let Some(m) = &metrics {
                        m.permission_webhook_sent_total.inc();
                    }
                }
                Err(err) => {
                    warn!(error = %err, "Permission denial audit webhook failed");
                    if let Some(m) = &metrics {
                        m.permission_webhook_failed_total.inc();
                    }
                }
            }
        });
    }
}

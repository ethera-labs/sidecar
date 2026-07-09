//! Audit payloads describing permission denials.

use alloy::primitives::{Address, B256};
use serde::Serialize;

use crate::engine::DenyReason;

/// The action a denied transaction or cross-rollup instance attempted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DenialAction {
    Send,
    Deploy,
    Call,
    CrossRollup,
}

impl DenialAction {
    /// Classify a normal transaction from its top-level shape.
    pub fn from_tx(is_create: bool, has_value: bool) -> Self {
        if is_create {
            Self::Deploy
        } else if has_value {
            Self::Send
        } else {
            Self::Call
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Send => "send",
            Self::Deploy => "deploy",
            Self::Call => "call",
            Self::CrossRollup => "cross_rollup",
        }
    }
}

/// A single permission denial recorded for the audit trail.
#[derive(Debug, Clone, Serialize)]
pub struct PermissionDenial {
    pub chain_id: u64,
    pub from: Address,
    pub action: DenialAction,
    pub reason: DenyReason,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tx_hash: Option<B256>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance_id: Option<String>,
    pub timestamp_ms: u64,
}

impl PermissionDenial {
    /// A denied normal transaction.
    pub fn transaction(
        chain_id: u64,
        from: Address,
        action: DenialAction,
        reason: DenyReason,
        tx_hash: Option<B256>,
        timestamp_ms: u64,
    ) -> Self {
        Self {
            chain_id,
            from,
            action,
            reason,
            tx_hash,
            instance_id: None,
            timestamp_ms,
        }
    }

    /// A denied cross-rollup instance.
    pub fn cross_rollup(
        chain_id: u64,
        from: Address,
        reason: DenyReason,
        instance_id: String,
        timestamp_ms: u64,
    ) -> Self {
        Self {
            chain_id,
            from,
            action: DenialAction::CrossRollup,
            reason,
            tx_hash: None,
            instance_id: Some(instance_id),
            timestamp_ms,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_classifies_transaction_shape() {
        assert_eq!(DenialAction::from_tx(true, true), DenialAction::Deploy);
        assert_eq!(DenialAction::from_tx(false, true), DenialAction::Send);
        assert_eq!(DenialAction::from_tx(false, false), DenialAction::Call);
    }

    #[test]
    fn serializes_with_machine_codes() {
        let denial = PermissionDenial::transaction(
            100003,
            Address::repeat_byte(0x11),
            DenialAction::Deploy,
            DenyReason::ContractDeployBlocked,
            Some(B256::repeat_byte(0x22)),
            1_700_000_000_000,
        );
        let json = serde_json::to_value(&denial).unwrap();
        assert_eq!(json["action"], "deploy");
        assert_eq!(json["reason"], "contract_deploy_blocked");
        assert_eq!(json["chain_id"], 100003);
        assert!(json.get("tx_hash").is_some());
        assert!(json.get("instance_id").is_none());
    }

    #[test]
    fn omits_missing_transaction_hash() {
        let denial = PermissionDenial::transaction(
            100003,
            Address::repeat_byte(0x11),
            DenialAction::Send,
            DenyReason::NativeSendBlocked,
            None,
            1_700_000_000_000,
        );
        let json = serde_json::to_value(&denial).unwrap();
        assert!(json.get("tx_hash").is_none());
    }
}

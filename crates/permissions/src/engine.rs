//! Permission engine: lock-free snapshot storage and the pure evaluator.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use alloy::primitives::Address;
use arc_swap::ArcSwapOption;
use ethera_spec::ChainId;

use crate::snapshot::{NetworkScope, PolicySnapshot};

/// Outcome of a permission check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny(DenyReason),
}

/// Why a transaction or cross-rollup instance was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenyReason {
    /// No fresh config snapshot - fail closed.
    ConfigUnavailable,
    EntityInactive,
    NativeSendBlocked,
    ContractDeployBlocked,
    PeerChainNotWhitelisted,
}

impl DenyReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ConfigUnavailable => "config unavailable",
            Self::EntityInactive => "entity inactive",
            Self::NativeSendBlocked => "native send blocked",
            Self::ContractDeployBlocked => "contract deploy blocked",
            Self::PeerChainNotWhitelisted => "peer chain not whitelisted",
        }
    }
}

#[derive(Debug)]
struct Inner {
    enabled: bool,
    connected: AtomicBool,
    snapshot: ArcSwapOption<PolicySnapshot>,
}

/// Cheaply cloneable handle to the shared permission state.
#[derive(Debug, Clone)]
pub struct PermissionEngine {
    inner: Arc<Inner>,
}

impl PermissionEngine {
    pub fn new(enabled: bool) -> Self {
        Self {
            inner: Arc::new(Inner {
                enabled,
                connected: AtomicBool::new(false),
                snapshot: ArcSwapOption::empty(),
            }),
        }
    }

    /// Replace the cached snapshot with a newly received one.
    pub fn store(&self, snapshot: PolicySnapshot) {
        self.inner.snapshot.store(Some(Arc::new(snapshot)));
    }

    /// Record config-stream connection liveness. Enforcement fails closed while
    /// disconnected, since the cached snapshot may no longer be authoritative.
    pub fn set_connected(&self, connected: bool) {
        self.inner.connected.store(connected, Ordering::Release);
    }

    /// Current snapshot version, if any snapshot has been received.
    pub fn version(&self) -> Option<u64> {
        self.inner.snapshot.load().as_ref().map(|s| s.version)
    }

    /// Whether enforcement can serve decisions: disabled engines are always
    /// ready; enabled engines need a live connection and a received snapshot.
    pub fn is_ready(&self) -> bool {
        !self.inner.enabled || self.fresh().is_some()
    }

    fn fresh(&self) -> Option<Arc<PolicySnapshot>> {
        if !self.inner.connected.load(Ordering::Acquire) {
            return None;
        }
        self.inner.snapshot.load_full()
    }

    /// Evaluate a normal transaction (UC1 native send, UC2 contract deploy).
    pub fn evaluate_tx(&self, from: Address, is_create: bool, has_value: bool) -> Decision {
        if !self.inner.enabled {
            return Decision::Allow;
        }
        let Some(snapshot) = self.fresh() else {
            return Decision::Deny(DenyReason::ConfigUnavailable);
        };
        let Some(wallet) = snapshot.wallet(&from) else {
            return Decision::Allow;
        };
        if !wallet.entity_active {
            return Decision::Deny(DenyReason::EntityInactive);
        }
        let Some(group) = &wallet.group else {
            return Decision::Allow;
        };
        if has_value && !group.send_native {
            return Decision::Deny(DenyReason::NativeSendBlocked);
        }
        if is_create && !group.can_deploy_contract {
            return Decision::Deny(DenyReason::ContractDeployBlocked);
        }
        Decision::Allow
    }

    /// Evaluate a cross-rollup instance (UC3 peer chain-id whitelist) for the
    /// initiating `sender`. `involved` is every chain participating in the XT.
    pub fn evaluate_xt(&self, sender: Address, local: ChainId, involved: &[ChainId]) -> Decision {
        if !self.inner.enabled {
            return Decision::Allow;
        }
        let Some(snapshot) = self.fresh() else {
            return Decision::Deny(DenyReason::ConfigUnavailable);
        };
        let Some(wallet) = snapshot.wallet(&sender) else {
            return Decision::Allow;
        };
        if !wallet.entity_active {
            return Decision::Deny(DenyReason::EntityInactive);
        }
        let Some(group) = &wallet.group else {
            return Decision::Allow;
        };
        if group.network_scope == NetworkScope::Restricted
            && involved
                .iter()
                .any(|chain| *chain != local && !group.allowed_peers.contains(chain))
        {
            return Decision::Deny(DenyReason::PeerChainNotWhitelisted);
        }
        Decision::Allow
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::snapshot::{PolicySnapshot, SnapshotData};

    const ENTITY: &str = "0x1111111111111111111111111111111111111111";
    const UNKNOWN: &str = "0x2222222222222222222222222222222222222222";

    fn addr(s: &str) -> Address {
        s.parse().unwrap()
    }

    fn engine_with(json: &str) -> PermissionEngine {
        let data: SnapshotData = serde_json::from_str(json).unwrap();
        let engine = PermissionEngine::new(true);
        engine.store(PolicySnapshot::from_wire(data, Instant::now()).unwrap());
        engine.set_connected(true);
        engine
    }

    fn snapshot_json(extra_group: &str) -> String {
        format!(
            r#"{{
              "version": 7,
              "entities": [
                {{"isActive": true, "ruleGroupId": "g1", "walletAddresses": [{{"address": "{ENTITY}"}}]}}
              ],
              "ruleGroups": [{extra_group}]
            }}"#
        )
    }

    #[test]
    fn disabled_engine_allows_everything() {
        let engine = PermissionEngine::new(false);
        assert_eq!(
            engine.evaluate_tx(addr(ENTITY), true, true),
            Decision::Allow
        );
        assert!(engine.is_ready());
    }

    #[test]
    fn missing_snapshot_fails_closed() {
        let engine = PermissionEngine::new(true);
        engine.set_connected(true);
        assert!(!engine.is_ready());
        assert_eq!(
            engine.evaluate_tx(addr(ENTITY), false, true),
            Decision::Deny(DenyReason::ConfigUnavailable)
        );
    }

    #[test]
    fn disconnected_stream_fails_closed() {
        let group = r#"{"ruleGroupId": "g1", "sendNative": true, "canDeployContract": true, "networkScope": "all", "rollups": []}"#;
        let engine = engine_with(&snapshot_json(group));
        engine.set_connected(false);
        assert!(!engine.is_ready());
        assert_eq!(
            engine.evaluate_tx(addr(ENTITY), false, true),
            Decision::Deny(DenyReason::ConfigUnavailable)
        );
    }

    #[test]
    fn unknown_wallet_is_allowed() {
        let group = r#"{"ruleGroupId": "g1", "sendNative": false, "canDeployContract": false, "networkScope": "all", "rollups": []}"#;
        let engine = engine_with(&snapshot_json(group));
        assert_eq!(
            engine.evaluate_tx(addr(UNKNOWN), true, true),
            Decision::Allow
        );
    }

    #[test]
    fn native_send_blocked_only_with_value() {
        let group = r#"{"ruleGroupId": "g1", "sendNative": false, "canDeployContract": true, "networkScope": "all", "rollups": []}"#;
        let engine = engine_with(&snapshot_json(group));
        assert_eq!(
            engine.evaluate_tx(addr(ENTITY), false, true),
            Decision::Deny(DenyReason::NativeSendBlocked)
        );
        assert_eq!(
            engine.evaluate_tx(addr(ENTITY), false, false),
            Decision::Allow
        );
    }

    #[test]
    fn contract_deploy_blocked_only_for_create() {
        let group = r#"{"ruleGroupId": "g1", "sendNative": true, "canDeployContract": false, "networkScope": "all", "rollups": []}"#;
        let engine = engine_with(&snapshot_json(group));
        assert_eq!(
            engine.evaluate_tx(addr(ENTITY), true, false),
            Decision::Deny(DenyReason::ContractDeployBlocked)
        );
        assert_eq!(
            engine.evaluate_tx(addr(ENTITY), false, false),
            Decision::Allow
        );
    }

    #[test]
    fn send_native_defaults_true_when_field_absent() {
        let group = r#"{"ruleGroupId": "g1", "canDeployContract": true, "networkScope": "all", "rollups": []}"#;
        let engine = engine_with(&snapshot_json(group));
        assert_eq!(
            engine.evaluate_tx(addr(ENTITY), false, true),
            Decision::Allow
        );
    }

    #[test]
    fn inactive_entity_is_denied() {
        let json = format!(
            r#"{{"version": 1, "entities": [{{"isActive": false, "ruleGroupId": "g1", "walletAddresses": [{{"address": "{ENTITY}"}}]}}], "ruleGroups": [{{"ruleGroupId": "g1", "networkScope": "all", "rollups": []}}]}}"#
        );
        let engine = engine_with(&json);
        assert_eq!(
            engine.evaluate_tx(addr(ENTITY), false, false),
            Decision::Deny(DenyReason::EntityInactive)
        );
    }

    #[test]
    fn restricted_network_enforces_peer_whitelist() {
        let group = r#"{"ruleGroupId": "g1", "sendNative": true, "canDeployContract": true, "networkScope": "restricted", "rollups": [{"chainId": 20}]}"#;
        let engine = engine_with(&snapshot_json(group));
        let local = ChainId(10);
        assert_eq!(
            engine.evaluate_xt(addr(ENTITY), local, &[local, ChainId(20)]),
            Decision::Allow
        );
        assert_eq!(
            engine.evaluate_xt(addr(ENTITY), local, &[local, ChainId(30)]),
            Decision::Deny(DenyReason::PeerChainNotWhitelisted)
        );
    }

    #[test]
    fn all_scope_allows_any_peer() {
        let group = r#"{"ruleGroupId": "g1", "sendNative": true, "canDeployContract": true, "networkScope": "all", "rollups": []}"#;
        let engine = engine_with(&snapshot_json(group));
        let local = ChainId(10);
        assert_eq!(
            engine.evaluate_xt(addr(ENTITY), local, &[local, ChainId(99)]),
            Decision::Allow
        );
    }
}

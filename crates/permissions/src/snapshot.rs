//! Permission config snapshot: wire format and the indexed form used for lookups.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use alloy::primitives::Address;
use ethera_spec::ChainId;
use serde::Deserialize;
use thiserror::Error;

/// Rejection reasons when indexing a received snapshot. A malformed snapshot is
/// never partially applied: dropping a single restricted entity would fail open.
#[derive(Debug, Error)]
pub enum SnapshotError {
    #[error("invalid wallet address: {0}")]
    InvalidAddress(String),
    #[error("entity references unknown rule group: {0}")]
    UnknownRuleGroup(String),
}

/// Frame pushed by the admin backend over `/api/v1/config/stream`.
#[derive(Debug, Deserialize)]
pub struct StreamFrame {
    pub r#type: String,
    pub data: SnapshotData,
}

/// Raw config payload as delivered by the admin backend.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotData {
    pub version: u64,
    #[serde(default)]
    pub entities: Vec<WireEntity>,
    #[serde(default)]
    pub rule_groups: Vec<WireRuleGroup>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireEntity {
    #[serde(default)]
    pub is_active: bool,
    #[serde(default)]
    pub rule_group_id: String,
    #[serde(default)]
    pub wallet_addresses: Vec<WireWallet>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireWallet {
    #[serde(default)]
    pub address: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireRuleGroup {
    #[serde(default)]
    pub rule_group_id: String,
    // `send_native` is not yet emitted by the backend; absence means allowed.
    // Accept both the camelCase wire form and the documented snake_case name.
    #[serde(default = "default_true", alias = "send_native")]
    pub send_native: bool,
    #[serde(default = "default_true")]
    pub can_deploy_contract: bool,
    #[serde(default)]
    pub network_scope: String,
    #[serde(default)]
    pub rollups: Vec<WireRollupItem>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireRollupItem {
    pub chain_id: u64,
}

fn default_true() -> bool {
    true
}

/// Whether a rule group may transact with all peers or only an explicit set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkScope {
    All,
    Restricted,
}

impl NetworkScope {
    fn parse(raw: &str) -> Self {
        if raw.eq_ignore_ascii_case("restricted") {
            Self::Restricted
        } else {
            Self::All
        }
    }
}

/// Resolved permission set assigned to an entity.
#[derive(Debug)]
pub struct RuleGroup {
    pub send_native: bool,
    pub can_deploy_contract: bool,
    pub network_scope: NetworkScope,
    pub allowed_peers: HashSet<ChainId>,
}

/// Per-wallet resolution: the owning entity's status and its rule group.
#[derive(Debug)]
pub struct WalletPolicy {
    pub entity_active: bool,
    pub group: Option<Arc<RuleGroup>>,
}

/// Indexed snapshot supporting O(1) wallet lookups.
#[derive(Debug)]
pub struct PolicySnapshot {
    pub version: u64,
    pub received_at: Instant,
    wallets: HashMap<Address, WalletPolicy>,
}

impl PolicySnapshot {
    /// Build the indexed snapshot from a freshly received payload.
    ///
    /// `received_at` records receipt time for diagnostics. Returns an error on any
    /// malformed entry rather than partially applying - a skipped restricted
    /// entity would silently fail open.
    pub fn from_wire(data: SnapshotData, received_at: Instant) -> Result<Self, SnapshotError> {
        let groups: HashMap<String, Arc<RuleGroup>> = data
            .rule_groups
            .into_iter()
            .map(|g| {
                let group = RuleGroup {
                    send_native: g.send_native,
                    can_deploy_contract: g.can_deploy_contract,
                    network_scope: NetworkScope::parse(&g.network_scope),
                    allowed_peers: g.rollups.into_iter().map(|r| ChainId(r.chain_id)).collect(),
                };
                (g.rule_group_id, Arc::new(group))
            })
            .collect();

        let mut wallets = HashMap::new();
        for entity in data.entities {
            let group =
                if entity.rule_group_id.is_empty() {
                    None
                } else {
                    Some(groups.get(&entity.rule_group_id).cloned().ok_or_else(|| {
                        SnapshotError::UnknownRuleGroup(entity.rule_group_id.clone())
                    })?)
                };
            for wallet in entity.wallet_addresses {
                let address = wallet
                    .address
                    .parse::<Address>()
                    .map_err(|_| SnapshotError::InvalidAddress(wallet.address.clone()))?;
                wallets.insert(
                    address,
                    WalletPolicy {
                        entity_active: entity.is_active,
                        group: group.clone(),
                    },
                );
            }
        }

        Ok(Self {
            version: data.version,
            received_at,
            wallets,
        })
    }

    pub fn wallet(&self, address: &Address) -> Option<&WalletPolicy> {
        self.wallets.get(address)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(json: &str) -> Result<PolicySnapshot, SnapshotError> {
        let data: SnapshotData = serde_json::from_str(json).unwrap();
        PolicySnapshot::from_wire(data, Instant::now())
    }

    #[test]
    fn rejects_invalid_wallet_address() {
        let json = r#"{"version":1,"entities":[{"isActive":true,"ruleGroupId":"","walletAddresses":[{"address":"nope"}]}],"ruleGroups":[]}"#;
        assert!(matches!(build(json), Err(SnapshotError::InvalidAddress(_))));
    }

    #[test]
    fn rejects_unknown_rule_group() {
        let json = r#"{"version":1,"entities":[{"isActive":true,"ruleGroupId":"missing","walletAddresses":[]}],"ruleGroups":[]}"#;
        assert!(matches!(
            build(json),
            Err(SnapshotError::UnknownRuleGroup(_))
        ));
    }

    #[test]
    fn accepts_send_native_snake_case_alias() {
        let json = r#"{"version":1,"entities":[{"isActive":true,"ruleGroupId":"g","walletAddresses":[{"address":"0x1111111111111111111111111111111111111111"}]}],"ruleGroups":[{"ruleGroupId":"g","send_native":false,"canDeployContract":true,"networkScope":"all","rollups":[]}]}"#;
        let snapshot = build(json).unwrap();
        let wallet = snapshot
            .wallet(
                &"0x1111111111111111111111111111111111111111"
                    .parse()
                    .unwrap(),
            )
            .unwrap();
        assert!(!wallet.group.as_ref().unwrap().send_native);
    }
}

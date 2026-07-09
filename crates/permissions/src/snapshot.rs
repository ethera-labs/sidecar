//! Permission policy snapshots and indexed lookup structures.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use alloy::primitives::Address;
use ethera_spec::ChainId;
use serde::Deserialize;
use thiserror::Error;

/// Errors returned while indexing a received policy snapshot.
///
/// Malformed snapshots are rejected as a whole so existing policy remains
/// intact.
#[derive(Debug, Error)]
pub enum SnapshotError {
    #[error("invalid wallet address: {0}")]
    InvalidAddress(String),
    #[error("entity references unknown rule group: {0}")]
    UnknownRuleGroup(String),
}

/// Frame received from the policy stream.
#[derive(Debug, Deserialize)]
pub struct StreamFrame {
    pub r#type: String,
    pub data: SnapshotData,
}

/// Raw policy snapshot payload.
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
    /// Unset (absent or null) when the entity belongs to no rule group.
    pub rule_group_id: Option<String>,
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
    // Absence means native transfers are allowed. Accept both camelCase and
    // snake_case wire forms.
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
    /// `received_at` records receipt time for diagnostics. Any malformed entry
    /// rejects the entire snapshot.
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
            let group = match entity.rule_group_id.as_deref().filter(|id| !id.is_empty()) {
                None => None,
                Some(id) => Some(
                    groups
                        .get(id)
                        .cloned()
                        .ok_or_else(|| SnapshotError::UnknownRuleGroup(id.to_string()))?,
                ),
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
    fn tolerates_null_rule_group_id() {
        let json = r#"{"version":1,"entities":[{"isActive":true,"ruleGroupId":null,"walletAddresses":[{"address":"0x1111111111111111111111111111111111111111"}]}],"ruleGroups":[]}"#;
        let snapshot = build(json).unwrap();
        let wallet = snapshot
            .wallet(
                &"0x1111111111111111111111111111111111111111"
                    .parse()
                    .unwrap(),
            )
            .unwrap();
        assert!(wallet.group.is_none());
        assert!(wallet.entity_active);
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

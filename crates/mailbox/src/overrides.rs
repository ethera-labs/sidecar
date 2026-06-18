//! Mailbox state-override construction and merging.

use alloy::primitives::{Address, B256, U256};
use alloy_rpc_types_eth::state::{AccountOverride, StateOverride};
use ethera_spec::ChainId;
use sidecar_primitives::CrossRollupDependency;

use crate::storage::{
    apply_bytes_to_state_diff, mailbox_key, mapping_slot, SlotMap, CREATED_KEYS_MAPPING_SLOT,
    INBOX_MAPPING_SLOT,
};

/// Merge `overlay` into `base`, with `overlay` taking precedence per address.
///
/// For `state` vs `stateDiff`:
/// - An overlay `state` (full replacement) replaces any existing `state` or
///   `stateDiff` for the same address.
/// - An overlay `stateDiff` is merged into an existing `stateDiff`, or applied
///   on top of an existing `state`.
pub fn merge_overrides(base: &mut StateOverride, overlay: &StateOverride) {
    for (addr, overlay_acct) in overlay {
        match base.entry(*addr) {
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                let base_acct = entry.get_mut();

                if let Some(overlay_state) = &overlay_acct.state {
                    base_acct.state = Some(overlay_state.clone());
                    base_acct.state_diff = None;
                } else if let Some(overlay_diff) = &overlay_acct.state_diff {
                    if let Some(base_state) = &mut base_acct.state {
                        for (k, v) in overlay_diff {
                            base_state.insert(*k, *v);
                        }
                    } else {
                        let base_diff = base_acct.state_diff.get_or_insert_default();
                        for (k, v) in overlay_diff {
                            base_diff.insert(*k, *v);
                        }
                    }
                }

                if overlay_acct.nonce.is_some() {
                    base_acct.nonce = overlay_acct.nonce;
                }
                if overlay_acct.balance.is_some() {
                    base_acct.balance = overlay_acct.balance;
                }
                if overlay_acct.code.is_some() {
                    base_acct.code = overlay_acct.code.clone();
                }
            }
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(overlay_acct.clone());
            }
        }
    }
}

/// Same as [`merge_overrides`] but consumes `overlay`, avoiding per-account clones
/// when the caller no longer needs the overlay map.
pub fn merge_overrides_owned(base: &mut StateOverride, overlay: StateOverride) {
    for (addr, overlay_acct) in overlay {
        match base.entry(addr) {
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                let base_acct = entry.get_mut();

                if let Some(overlay_state) = overlay_acct.state {
                    base_acct.state = Some(overlay_state);
                    base_acct.state_diff = None;
                } else if let Some(overlay_diff) = overlay_acct.state_diff {
                    if let Some(base_state) = &mut base_acct.state {
                        for (k, v) in overlay_diff {
                            base_state.insert(k, v);
                        }
                    } else if let Some(base_diff) = base_acct.state_diff.as_mut() {
                        for (k, v) in overlay_diff {
                            base_diff.insert(k, v);
                        }
                    } else {
                        base_acct.state_diff = Some(overlay_diff);
                    }
                }

                if overlay_acct.nonce.is_some() {
                    base_acct.nonce = overlay_acct.nonce;
                }
                if overlay_acct.balance.is_some() {
                    base_acct.balance = overlay_acct.balance;
                }
                if overlay_acct.code.is_some() {
                    base_acct.code = overlay_acct.code;
                }
            }
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(overlay_acct);
            }
        }
    }
}

/// Build mailbox state overrides for fulfilled dependencies.
///
/// Returns a typed `StateOverride` suitable for passing directly to
/// `debug_traceCall`, or `None` if there are no applicable dependencies.
pub fn build_mailbox_state_overrides(
    chain_id: ChainId,
    mailbox_address: Address,
    deps: &[CrossRollupDependency],
) -> Option<StateOverride> {
    let mut state_diff = SlotMap::default();

    for dep in deps {
        if dep.dest_chain_id != chain_id {
            continue;
        }
        let Some(data) = dep.data.as_ref() else {
            continue;
        };
        let key = mailbox_key(chain_id, dep);

        let inbox_slot = mapping_slot(key, INBOX_MAPPING_SLOT);
        let created_slot = mapping_slot(key, CREATED_KEYS_MAPPING_SLOT);
        apply_bytes_to_state_diff(&mut state_diff, inbox_slot, data);
        state_diff.insert(
            created_slot,
            B256::from(U256::from(1u64).to_be_bytes::<32>()),
        );
    }

    if state_diff.is_empty() {
        return None;
    }

    let account = AccountOverride {
        state_diff: Some(state_diff),
        ..Default::default()
    };
    let mut overrides = StateOverride::default();
    overrides.insert(mailbox_address, account);
    Some(overrides)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_overrides_combines_accounts() {
        let addr: Address = "0xaabbccddeeaabbccddeeaabbccddeeaabbccddee"
            .parse()
            .unwrap();
        let mut base = StateOverride::default();
        base.insert(
            addr,
            AccountOverride {
                nonce: Some(1),
                ..Default::default()
            },
        );
        let mut other = StateOverride::default();
        other.insert(
            addr,
            AccountOverride {
                balance: Some(U256::from(0x100u64)),
                ..Default::default()
            },
        );
        merge_overrides(&mut base, &other);
        let acct = base.get(&addr).unwrap();
        assert_eq!(acct.nonce, Some(1));
        assert_eq!(acct.balance, Some(U256::from(0x100u64)));
    }

    #[test]
    fn merge_overrides_merges_state_diff() {
        let addr: Address = "0xaabbccddeeaabbccddeeaabbccddeeaabbccddee"
            .parse()
            .unwrap();
        let slot1 = B256::repeat_byte(0x01);
        let slot2 = B256::repeat_byte(0x02);
        let val1 = B256::repeat_byte(0x10);
        let val2 = B256::repeat_byte(0x20);

        let mut diff1 = SlotMap::default();
        diff1.insert(slot1, val1);
        let mut base = StateOverride::default();
        base.insert(
            addr,
            AccountOverride {
                state_diff: Some(diff1),
                ..Default::default()
            },
        );

        let mut diff2 = SlotMap::default();
        diff2.insert(slot2, val2);
        let mut overlay = StateOverride::default();
        overlay.insert(
            addr,
            AccountOverride {
                state_diff: Some(diff2),
                ..Default::default()
            },
        );

        merge_overrides(&mut base, &overlay);
        let diff = base.get(&addr).unwrap().state_diff.as_ref().unwrap();
        assert_eq!(diff.len(), 2);
        assert_eq!(diff.get(&slot1), Some(&val1));
        assert_eq!(diff.get(&slot2), Some(&val2));
    }

    #[test]
    fn builds_mailbox_overrides_for_fulfilled_dep() {
        let dep = CrossRollupDependency {
            source_chain_id: ChainId(77777),
            dest_chain_id: ChainId(88888),
            sender: Address::repeat_byte(0x11),
            receiver: Address::repeat_byte(0x22),
            label: b"SEND".to_vec(),
            data: Some(vec![1, 2, 3]),
            session_id: U256::from(42u64),
        };

        let mailbox_addr: Address = "0xe5d5d610fb9767df117f4076444b45404201a097"
            .parse()
            .unwrap();
        let overrides =
            build_mailbox_state_overrides(ChainId(88888), mailbox_addr, &[dep]).unwrap();

        let account = overrides.get(&mailbox_addr).unwrap();
        let diff = account.state_diff.as_ref().unwrap();
        assert!(!diff.is_empty());
    }
}

//! `UniversalBridgeMailbox` storage-layout primitives.

use alloy::primitives::{keccak256, map::FbBuildHasher, Keccak256, B256, U256};
use ethera_spec::ChainId;
use sidecar_primitives::CrossRollupDependency;
use std::collections::HashMap;

/// Alloy's state-diff map type: B256 keys with a fixed-bytes hasher.
pub(crate) type SlotMap = HashMap<B256, B256, FbBuildHasher<32>>;

pub(crate) const INBOX_MAPPING_SLOT: u64 = 5;
pub(crate) const CREATED_KEYS_MAPPING_SLOT: u64 = 7;

/// Storage slot of `mapping[key]` declared at base `slot`, per Solidity's
/// layout: `keccak256(key ++ uint256(slot))`.
pub(crate) fn mapping_slot(key: B256, slot: u64) -> B256 {
    let mut hasher = Keccak256::new();
    hasher.update(key);
    hasher.update(U256::from(slot).to_be_bytes::<32>());
    hasher.finalize()
}

/// Mapping key for a mailbox inbox entry, hashed from the dependency fields.
///
/// The preimage matches the Solidity mailbox contract's key derivation:
///
/// ```text
///   offset  bytes  field
///   ──────  ─────  ─────────────────────────
///    0      32     source_chain_id  (uint256)
///   32      32     dest_chain_id    (uint256, = chain_id)
///   64      20     sender           (address)
///   84      20     receiver         (address)
///  104      32     session_id       (uint256)
///  136      var    label            (raw bytes)
/// ```
pub(crate) fn mailbox_key(chain_id: ChainId, dep: &CrossRollupDependency) -> B256 {
    let mut hasher = Keccak256::new();
    hasher.update(U256::from(dep.source_chain_id.0).to_be_bytes::<32>());
    hasher.update(U256::from(chain_id.0).to_be_bytes::<32>());
    hasher.update(dep.sender);
    hasher.update(dep.receiver);
    hasher.update(dep.session_id.to_be_bytes::<32>());
    hasher.update(&dep.label);
    hasher.finalize()
}

/// Write a Solidity `bytes` value at storage `slot` into `state_diff`.
///
/// Values up to 31 bytes are packed inline; longer values store `2 * len + 1`
/// at `slot` and spill the payload across `keccak256(slot) + i`.
pub(crate) fn apply_bytes_to_state_diff(state_diff: &mut SlotMap, slot: B256, data: &[u8]) {
    if data.len() <= 31 {
        state_diff.insert(slot, encode_short_bytes(data));
        return;
    }

    let len_word = U256::from(data.len()) * U256::from(2u64) + U256::from(1u64);
    state_diff.insert(slot, B256::from(len_word.to_be_bytes::<32>()));

    let base_slot = U256::from_be_bytes(keccak256(slot).0);
    for (i, chunk) in data.chunks(32).enumerate() {
        let mut word = [0u8; 32];
        word[..chunk.len()].copy_from_slice(chunk);
        let slot_i = base_slot + U256::from(i);
        state_diff.insert(B256::from(slot_i.to_be_bytes::<32>()), B256::from(word));
    }
}

/// Encode `data` (≤31 bytes) in Solidity's short-`bytes` slot layout: the bytes
/// left-aligned, with `2 * len` in the lowest byte.
fn encode_short_bytes(data: &[u8]) -> B256 {
    let mut word = [0u8; 32];
    let len = data.len().min(31);
    word[..len].copy_from_slice(&data[..len]);
    word[31] = (len as u8) * 2;
    B256::from(word)
}

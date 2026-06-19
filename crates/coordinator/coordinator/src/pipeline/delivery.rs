//! Helpers for deriving sender/nonce metadata from XT transactions.

use alloy::consensus::transaction::SignerRecoverable;
use alloy::consensus::{Transaction, TxEnvelope};
use alloy::primitives::Address;
use ethera_spec::ChainId;
use sidecar_primitives::CrossRollupDependency;

/// Decode the sender address and nonce from a raw RLP-encoded signed transaction.
pub fn decode_sender_nonce(raw_tx: &[u8]) -> Option<(Address, u64)> {
    let signed: TxEnvelope = alloy::rlp::Decodable::decode(&mut &raw_tx[..]).ok()?;
    let from = signed.recover_signer().ok()?;
    Some((from, signed.nonce()))
}

/// Filter dependencies to only those targeting the given chain.
pub fn deps_for_chain(
    deps: &[CrossRollupDependency],
    chain_id: ChainId,
) -> Vec<CrossRollupDependency> {
    deps.iter()
        .filter(|dep| dep.dest_chain_id == chain_id)
        .cloned()
        .collect()
}

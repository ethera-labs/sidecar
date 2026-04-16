//! Helpers for deriving sender/nonce metadata from XT transactions.

use std::collections::HashMap;

use alloy::consensus::transaction::SignerRecoverable;
use alloy::consensus::{Transaction, TxEnvelope};
use alloy::primitives::Address;
use compose_primitives::{ChainId, CrossRollupDependency};

/// Decode the sender address and nonce from a raw RLP-encoded signed transaction.
pub fn decode_sender_nonce(raw_tx: &[u8]) -> Option<(Address, u64)> {
    let signed: TxEnvelope = alloy::rlp::Decodable::decode(&mut &raw_tx[..]).ok()?;
    let from = signed.recover_signer().ok()?;
    Some((from, signed.nonce()))
}

/// Build a sender/nonce cache from each chain's first raw transaction.
///
/// Performs ECDSA recovery once at XT registration so later lifecycle steps
/// do not need to repeat recovery for the first local transaction on a chain.
pub fn build_sender_nonce_cache(
    txs: &HashMap<ChainId, Vec<Vec<u8>>>,
) -> HashMap<ChainId, (Address, u64)> {
    txs.iter()
        .filter_map(|(&chain_id, chain_txs)| {
            chain_txs
                .first()
                .and_then(|tx| decode_sender_nonce(tx))
                .map(|sn| (chain_id, sn))
        })
        .collect()
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

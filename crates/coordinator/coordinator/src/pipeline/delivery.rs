//! Helpers for turning committed XTs into builder payloads.

use alloy::consensus::transaction::SignerRecoverable;
use alloy::consensus::{Transaction, TxEnvelope};
use alloy::primitives::Address;
use compose_primitives::{
    ChainId, CrossRollupDependency, InstanceId, StateOverride, TransactionPayload,
};

/// A committed XT ready to be delivered to the builder.
#[derive(Debug)]
pub struct DeliverableXt {
    pub id: InstanceId,
    pub put_inbox_txs: Vec<Vec<u8>>,
    pub raw_txs: Vec<Vec<u8>>,
    pub deps: Vec<CrossRollupDependency>,
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

/// Decode the sender address and nonce from a raw RLP-encoded signed transaction.
pub fn decode_sender_nonce(raw_tx: &[u8]) -> Option<(Address, u64)> {
    let signed: TxEnvelope = alloy::rlp::Decodable::decode(&mut &raw_tx[..]).ok()?;
    let from = signed.recover_signer().ok()?;
    Some((from, signed.nonce()))
}

/// Look up an account's nonce from the builder's state overrides.
pub fn sender_nonce_from_overrides(overrides: &StateOverride, sender: Address) -> Option<u64> {
    overrides.get(&sender).and_then(|acct| acct.nonce)
}

/// Build transaction payloads for the builder from deliverable XTs.
///
/// This includes both the committed user transactions and any `putInbox`
/// transactions for fulfilled CIRC dependencies.
pub fn build_transaction_payloads(deliverable: &[DeliverableXt]) -> Vec<TransactionPayload> {
    let mut payloads = Vec::new();

    for entry in deliverable {
        for put_inbox_tx in &entry.put_inbox_txs {
            payloads.push(TransactionPayload {
                raw: format!("0x{}", hex::encode(put_inbox_tx)),
                required: true,
                instance_id: entry.id.to_string(),
            });
        }

        for raw_tx in &entry.raw_txs {
            payloads.push(TransactionPayload {
                raw: format!("0x{}", hex::encode(raw_tx)),
                required: true,
                instance_id: entry.id.to_string(),
            });
        }
    }

    payloads
}

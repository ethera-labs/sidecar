//! Matching helpers between dependencies and mailbox messages.

use compose_primitives::{ChainId, CrossRollupDependency};
use compose_proto::MailboxMessage;

/// Check whether a mailbox message satisfies a cross-rollup dependency.
///
/// A message matches if source chain, destination chain, and label all agree.
pub fn matches_dependency(msg: &MailboxMessage, dep: &CrossRollupDependency) -> bool {
    if ChainId(msg.source_chain) != dep.source_chain_id {
        return false;
    }
    if ChainId(msg.destination_chain) != dep.dest_chain_id {
        return false;
    }
    if msg.source.as_slice() != dep.sender.as_slice() {
        return false;
    }
    if msg.receiver.as_slice() != dep.receiver.as_slice() {
        return false;
    }
    let dep_session_id = dep
        .session_id
        .map(|value| value.try_into().unwrap_or(0u64))
        .unwrap_or_default();
    if msg.session_id != dep_session_id {
        return false;
    }

    let label_bytes = dep.label.as_slice();
    if msg.label.as_bytes() != label_bytes {
        return false;
    }

    true
}

/// Generate a deduplication key for a dependency.
pub fn dep_key(dep: &CrossRollupDependency) -> String {
    format!(
        "{}:{}:{}:{}:{}:{}",
        dep.source_chain_id,
        dep.dest_chain_id,
        hex::encode(dep.sender.as_slice()),
        hex::encode(dep.receiver.as_slice()),
        dep.session_id.unwrap_or_default(),
        hex::encode(&dep.label)
    )
}

/// Check if a message list already contains an equivalent outbound message.
pub fn contains_message(
    msgs: &[compose_primitives::CrossRollupMessage],
    msg: &compose_primitives::CrossRollupMessage,
) -> bool {
    msgs.iter().any(|m| {
        m.source_chain_id == msg.source_chain_id
            && m.dest_chain_id == msg.dest_chain_id
            && m.label == msg.label
            && m.sender == msg.sender
            && m.receiver == msg.receiver
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{Address, U256};

    #[test]
    fn matching_message() {
        let msg = MailboxMessage {
            source_chain: 901,
            destination_chain: 902,
            source: vec![0u8; 20],
            receiver: vec![0u8; 20],
            session_id: 42,
            label: "transfer".to_string(),
            ..Default::default()
        };
        let dep = CrossRollupDependency {
            source_chain_id: ChainId(901),
            dest_chain_id: ChainId(902),
            sender: Address::ZERO,
            receiver: Address::ZERO,
            label: b"transfer".to_vec(),
            data: None,
            session_id: Some(U256::from(42u64)),
        };
        assert!(matches_dependency(&msg, &dep));
    }

    #[test]
    fn non_matching_chain() {
        let msg = MailboxMessage {
            source_chain: 901,
            destination_chain: 903,
            source: vec![0u8; 20],
            receiver: vec![0u8; 20],
            session_id: 42,
            label: "transfer".to_string(),
            ..Default::default()
        };
        let dep = CrossRollupDependency {
            source_chain_id: ChainId(901),
            dest_chain_id: ChainId(902),
            sender: Address::ZERO,
            receiver: Address::ZERO,
            label: b"transfer".to_vec(),
            data: None,
            session_id: Some(U256::from(42u64)),
        };
        assert!(!matches_dependency(&msg, &dep));
    }

    #[test]
    fn non_matching_session() {
        let msg = MailboxMessage {
            source_chain: 901,
            destination_chain: 902,
            source: vec![0u8; 20],
            receiver: vec![0u8; 20],
            session_id: 41,
            label: "transfer".to_string(),
            ..Default::default()
        };
        let dep = CrossRollupDependency {
            source_chain_id: ChainId(901),
            dest_chain_id: ChainId(902),
            sender: Address::ZERO,
            receiver: Address::ZERO,
            label: b"transfer".to_vec(),
            data: None,
            session_id: Some(U256::from(42u64)),
        };
        assert!(!matches_dependency(&msg, &dep));
    }
}

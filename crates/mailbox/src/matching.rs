//! Matching helpers between dependencies and mailbox messages.

use alloy::primitives::{Address, U256};
use compose_primitives::{ChainId, CrossRollupDependency, CrossRollupMessage};
use compose_proto::MailboxMessage;

use crate::wire;

/// Hashable key for mailbox dependencies, mirroring the six-field on-chain key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DependencyKey {
    source_chain_id: ChainId,
    dest_chain_id: ChainId,
    sender: Address,
    receiver: Address,
    session_id: U256,
    label: Vec<u8>,
}

impl From<&CrossRollupDependency> for DependencyKey {
    fn from(dep: &CrossRollupDependency) -> Self {
        Self {
            source_chain_id: dep.source_chain_id,
            dest_chain_id: dep.dest_chain_id,
            sender: dep.sender,
            receiver: dep.receiver,
            session_id: dep.session_id,
            label: dep.label.clone(),
        }
    }
}

/// Hashable key for outbound mailbox-message deduplication.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MailboxMessageKey {
    instance_id: Vec<u8>,
    source_chain: u64,
    destination_chain: u64,
    sender: Vec<u8>,
    receiver: Vec<u8>,
    session_id: Vec<u8>,
    label: String,
}

impl From<&MailboxMessage> for MailboxMessageKey {
    fn from(msg: &MailboxMessage) -> Self {
        Self {
            instance_id: msg.instance_id.clone(),
            source_chain: msg.source_chain,
            destination_chain: msg.destination_chain,
            sender: msg.sender.clone(),
            receiver: msg.receiver.clone(),
            session_id: msg.session_id.clone(),
            label: msg.label.clone(),
        }
    }
}

/// Check whether a mailbox message satisfies a cross-rollup dependency.
///
/// All six key fields must agree: source chain, destination chain, sender,
/// receiver, session ID, and label — mirroring the on-chain `getKey` preimage.
pub fn matches_dependency(msg: &MailboxMessage, dep: &CrossRollupDependency) -> bool {
    if ChainId(msg.source_chain) != dep.source_chain_id {
        return false;
    }
    if ChainId(msg.destination_chain) != dep.dest_chain_id {
        return false;
    }
    if msg.label.as_bytes() != dep.label.as_slice() {
        return false;
    }
    if msg.sender.len() != 20 || Address::from_slice(&msg.sender) != dep.sender {
        return false;
    }
    if msg.receiver.len() != 20 || Address::from_slice(&msg.receiver) != dep.receiver {
        return false;
    }

    let Some(msg_session_id) = decode_session_id(&msg.session_id) else {
        return false;
    };
    if msg_session_id != dep.session_id {
        return false;
    }

    true
}

/// Compare the mailbox key fields of two dependencies without allocating.
pub fn dependency_keys_equal(left: &CrossRollupDependency, right: &CrossRollupDependency) -> bool {
    left.source_chain_id == right.source_chain_id
        && left.dest_chain_id == right.dest_chain_id
        && left.sender == right.sender
        && left.receiver == right.receiver
        && left.session_id == right.session_id
        && left.label == right.label
}

/// Check if a message list already contains an equivalent outbound message.
pub fn contains_message(msgs: &[CrossRollupMessage], msg: &CrossRollupMessage) -> bool {
    msgs.iter().any(|m| {
        m.source_chain_id == msg.source_chain_id
            && m.dest_chain_id == msg.dest_chain_id
            && m.label == msg.label
            && m.sender == msg.sender
            && m.receiver == msg.receiver
            && m.session_id == msg.session_id
    })
}

fn decode_session_id(bytes: &[u8]) -> Option<U256> {
    let decoded = wire::decode_session_id(bytes);
    if decoded.is_none() {
        tracing::warn!(
            session_id_len = bytes.len(),
            expected_session_id_len = wire::SESSION_ID_BYTES,
            "Malformed mailbox session_id bytes"
        );
    }
    decoded
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_msg(
        src: u64,
        dst: u64,
        sender: Address,
        receiver: Address,
        session_id: U256,
        label: &str,
    ) -> MailboxMessage {
        MailboxMessage {
            source_chain: src,
            destination_chain: dst,
            sender: sender.as_slice().to_vec(),
            receiver: receiver.as_slice().to_vec(),
            session_id: wire::encode_session_id(session_id),
            label: label.to_string(),
            ..Default::default()
        }
    }

    fn make_dep(
        src: u64,
        dst: u64,
        sender: Address,
        receiver: Address,
        session_id: U256,
        label: &[u8],
    ) -> CrossRollupDependency {
        CrossRollupDependency {
            source_chain_id: ChainId(src),
            dest_chain_id: ChainId(dst),
            sender,
            receiver,
            label: label.to_vec(),
            data: None,
            session_id,
        }
    }

    #[test]
    fn matching_message() {
        let sender = Address::repeat_byte(0x11);
        let receiver = Address::repeat_byte(0x22);
        let session_id = U256::from(42u64);

        let msg = make_msg(901, 902, sender, receiver, session_id, "SEND_TOKENS");
        let dep = make_dep(901, 902, sender, receiver, session_id, b"SEND_TOKENS");
        assert!(matches_dependency(&msg, &dep));
    }

    #[test]
    fn non_matching_chain() {
        let sender = Address::repeat_byte(0x11);
        let receiver = Address::repeat_byte(0x22);
        let session_id = U256::from(1u64);

        let msg = make_msg(901, 903, sender, receiver, session_id, "SEND_TOKENS");
        let dep = make_dep(901, 902, sender, receiver, session_id, b"SEND_TOKENS");
        assert!(!matches_dependency(&msg, &dep));
    }

    #[test]
    fn non_matching_session_id() {
        let sender = Address::repeat_byte(0x11);
        let receiver = Address::repeat_byte(0x22);

        let msg = make_msg(901, 902, sender, receiver, U256::from(1u64), "SEND_TOKENS");
        let dep = make_dep(901, 902, sender, receiver, U256::from(2u64), b"SEND_TOKENS");
        assert!(!matches_dependency(&msg, &dep));
    }

    #[test]
    fn non_matching_sender() {
        let receiver = Address::repeat_byte(0x22);
        let session_id = U256::from(1u64);

        let msg = make_msg(
            901,
            902,
            Address::repeat_byte(0x11),
            receiver,
            session_id,
            "SEND_TOKENS",
        );
        let dep = make_dep(
            901,
            902,
            Address::repeat_byte(0x33),
            receiver,
            session_id,
            b"SEND_TOKENS",
        );
        assert!(!matches_dependency(&msg, &dep));
    }

    #[test]
    fn malformed_session_id_does_not_match() {
        let sender = Address::repeat_byte(0x11);
        let receiver = Address::repeat_byte(0x22);
        let session_id = U256::from(1u64);

        let mut msg = make_msg(901, 902, sender, receiver, session_id, "SEND_TOKENS");
        msg.session_id.pop();

        let dep = make_dep(901, 902, sender, receiver, session_id, b"SEND_TOKENS");
        assert!(!matches_dependency(&msg, &dep));
    }

    #[test]
    fn dependency_key_equality_distinguishes_session_id() {
        let sender = Address::repeat_byte(0x11);
        let receiver = Address::repeat_byte(0x22);
        let left = make_dep(901, 902, sender, receiver, U256::from(7u64), b"SEND_TOKENS");
        let mut right = make_dep(901, 902, sender, receiver, U256::from(7u64), b"SEND_TOKENS");

        assert!(dependency_keys_equal(&left, &right));
        right.session_id = U256::from(8u64);
        assert!(!dependency_keys_equal(&left, &right));
    }

    #[test]
    fn dependency_key_hash_distinguishes_sender() {
        let receiver = Address::repeat_byte(0x22);
        let left = make_dep(
            901,
            902,
            Address::repeat_byte(0x11),
            receiver,
            U256::from(42u64),
            b"SEND_TOKENS",
        );
        let right = make_dep(
            901,
            902,
            Address::repeat_byte(0x33),
            receiver,
            U256::from(42u64),
            b"SEND_TOKENS",
        );

        assert_ne!(DependencyKey::from(&left), DependencyKey::from(&right));
    }

    #[test]
    fn mailbox_message_key_distinguishes_session_id() {
        let sender = Address::repeat_byte(0x11);
        let receiver = Address::repeat_byte(0x22);
        let left = make_msg(901, 902, sender, receiver, U256::from(1u64), "SEND_TOKENS");
        let right = make_msg(901, 902, sender, receiver, U256::from(2u64), "SEND_TOKENS");

        assert_ne!(
            MailboxMessageKey::from(&left),
            MailboxMessageKey::from(&right)
        );
    }
}

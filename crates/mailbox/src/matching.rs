//! Matching helpers between dependencies and mailbox messages.

use alloy::primitives::{Address, U256};
use compose_primitives::{ChainId, CrossRollupDependency, CrossRollupMessage};
use compose_proto::MailboxMessage;

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

    let Some(msg_session_id) = session_id_from_bytes(&msg.session_id) else {
        return false;
    };
    if msg_session_id != dep.session_id {
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
        hex::encode(dep.session_id.to_be_bytes::<32>()),
        hex::encode(&dep.label),
    )
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

/// Decode a proto `session_id` bytes field (up to 32 bytes, big-endian) into `U256`.
fn session_id_from_bytes(bytes: &[u8]) -> Option<U256> {
    if bytes.len() > 32 {
        tracing::warn!(
            session_id_len = bytes.len(),
            max_session_id_len = 32,
            "Malformed mailbox session_id bytes"
        );
        return None;
    }
    let mut buf = [0u8; 32];
    buf[32 - bytes.len()..].copy_from_slice(bytes);
    Some(U256::from_be_bytes(buf))
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
            session_id: session_id.to_be_bytes::<32>().to_vec(),
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
    fn dep_key_includes_all_fields() {
        let dep = make_dep(
            901,
            902,
            Address::repeat_byte(0x11),
            Address::repeat_byte(0x22),
            U256::from(42u64),
            b"SEND_TOKENS",
        );
        let key = dep_key(&dep);
        assert!(key.contains("901"));
        assert!(key.contains("902"));
    }
}

//! Wire-format helpers for mailbox protobuf messages.

use alloy::primitives::U256;

pub const SESSION_ID_BYTES: usize = 32;

#[must_use]
pub fn encode_session_id(session_id: U256) -> Vec<u8> {
    session_id.to_be_bytes::<SESSION_ID_BYTES>().to_vec()
}

#[must_use]
pub fn decode_session_id(bytes: &[u8]) -> Option<U256> {
    if bytes.len() != SESSION_ID_BYTES {
        return None;
    }

    Some(U256::from_be_slice(bytes))
}

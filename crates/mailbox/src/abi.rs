//! Mailbox ABI encoding utilities.

use alloy::primitives::{Address, U256};
use alloy::sol;
use alloy::sol_types::SolCall;

use crate::error::MailboxError;

sol! {
    function putInbox(
        uint256 chainMessageSender,
        address sender,
        address receiver,
        uint256 sessionId,
        string label,
        bytes data
    );
}

/// Encode a `putInbox` call for the `UniversalBridgeMailbox` contract.
pub fn encode_put_inbox(
    source_chain_id: u64,
    sender: Address,
    receiver: Address,
    session_id: U256,
    label: &[u8],
    data: &[u8],
) -> Result<Vec<u8>, MailboxError> {
    let call = putInboxCall {
        chainMessageSender: U256::from(source_chain_id),
        sender,
        receiver,
        sessionId: session_id,
        label: String::from_utf8_lossy(label).to_string(),
        data: data.to_vec().into(),
    };
    Ok(call.abi_encode())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_put_inbox_produces_valid_calldata() {
        let data = encode_put_inbox(
            901,
            Address::ZERO,
            Address::ZERO,
            U256::ZERO,
            b"SEND_TOKENS",
            b"hello",
        )
        .unwrap();
        assert!(data.len() > 4);
    }
}

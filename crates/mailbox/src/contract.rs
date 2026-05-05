//! `UniversalBridgeMailbox` v2 ABI bindings and calldata helpers.

use alloy::sol;

sol! {
    struct MessageHeader {
        uint256 chainSrc;
        uint256 chainDest;
        address sender;
        address receiver;
        uint256 sessionId;
        string label;
    }

    struct Message {
        MessageHeader header;
        bytes payload;
    }

    function putInbox(
        uint256 chainMessageSender,
        address sender,
        address receiver,
        uint256 sessionId,
        string label,
        bytes data
    );

    function writeMessage(Message message);
    function readMessage(MessageHeader header) returns (bytes);
}

pub(crate) fn calldata_selector(input: &str) -> Option<[u8; 4]> {
    let input = input.strip_prefix("0x").unwrap_or(input);
    let selector_hex = input.get(..8)?;

    let mut selector = [0u8; 4];
    hex::decode_to_slice(selector_hex, &mut selector).ok()?;
    Some(selector)
}

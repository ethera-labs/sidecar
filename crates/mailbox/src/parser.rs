//! Mailbox trace parsing from call tracer output.

use alloy::primitives::Address;
use alloy::sol_types::SolCall;
use compose_primitives::ChainId;
use serde_json::Value;
use tracing::debug;

use crate::contract::{calldata_selector, readMessageCall, writeMessageCall};
use crate::types::{MailboxCall, MailboxCallType, SimulationState};

/// Parse mailbox read/write calls from a geth `callTracer` output.
///
/// Recursively walks the call tree, identifying calls to the mailbox contract
/// by matching function selectors for `writeMessage` and `readMessage`.
pub fn parse_call_trace(
    trace: &Value,
    mailbox_address: Address,
    local_chain_id: ChainId,
) -> SimulationState {
    let mut state = SimulationState::default();
    walk_trace(trace, mailbox_address, local_chain_id, &mut state);
    state
}

fn walk_trace(
    node: &Value,
    mailbox_address: Address,
    local_chain_id: ChainId,
    state: &mut SimulationState,
) {
    let from_str = node.get("from").and_then(|v| v.as_str()).unwrap_or("");
    let to_str = node.get("to").and_then(|v| v.as_str()).unwrap_or("");
    let input = node.get("input").and_then(|v| v.as_str()).unwrap_or("");
    let from_addr = from_str.parse::<Address>().ok();

    if let Ok(to_addr) = to_str.parse::<Address>() {
        if to_addr == mailbox_address && input.len() >= 10 {
            let selector = calldata_selector(input);

            if selector == Some(writeMessageCall::SELECTOR) {
                if let Some(caller) = from_addr {
                    if let Some(call) = decode_write(input, caller, local_chain_id) {
                        debug!(label = %call.label, "Parsed mailbox writeMessage call");
                        state.writes.push(call);
                    }
                }
            } else if selector == Some(readMessageCall::SELECTOR) {
                if let Some(call) = decode_read(input, local_chain_id) {
                    debug!(label = %call.label, "Parsed mailbox readMessage call");
                    state.reads.push(call);
                }
            }
        }
    }

    if let Some(calls) = node.get("calls").and_then(|v| v.as_array()) {
        for child in calls {
            walk_trace(child, mailbox_address, local_chain_id, state);
        }
    }
}

/// Decode a `writeMessage(Message)` call.
///
/// The on-chain key uses `msg.sender` (= `caller`) as the sender, not `header.sender`.
fn decode_write(input: &str, caller: Address, local_chain_id: ChainId) -> Option<MailboxCall> {
    let data = hex::decode(input.trim_start_matches("0x")).ok()?;
    let call = writeMessageCall::abi_decode(&data).ok()?;
    let header = &call.message.header;

    Some(MailboxCall {
        call_type: MailboxCallType::Write,
        source_chain: local_chain_id,
        dest_chain: ChainId(u64::try_from(header.chainDest).ok()?),
        sender: caller,
        receiver: header.receiver,
        label: header.label.clone(),
        data: call.message.payload.to_vec(),
        session_id: header.sessionId,
    })
}

/// Decode a `readMessage(MessageHeader)` call.
fn decode_read(input: &str, local_chain_id: ChainId) -> Option<MailboxCall> {
    let data = hex::decode(input.trim_start_matches("0x")).ok()?;
    let call = readMessageCall::abi_decode(&data).ok()?;
    let header = &call.header;

    Some(MailboxCall {
        call_type: MailboxCallType::Read,
        source_chain: ChainId(u64::try_from(header.chainSrc).ok()?),
        dest_chain: local_chain_id,
        sender: header.sender,
        receiver: header.receiver,
        label: header.label.clone(),
        data: Vec::new(),
        session_id: header.sessionId,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{Message, MessageHeader};
    use alloy::primitives::U256;
    use serde_json::json;

    fn make_write_calldata(
        dest_chain: u64,
        caller: Address,
        receiver: Address,
        session_id: U256,
        label: &str,
        payload: &[u8],
    ) -> String {
        let call = writeMessageCall {
            message: Message {
                header: MessageHeader {
                    chainSrc: U256::ZERO,
                    chainDest: U256::from(dest_chain),
                    sender: caller,
                    receiver,
                    sessionId: session_id,
                    label: label.to_string(),
                },
                payload: payload.to_vec().into(),
            },
        };
        format!("0x{}", hex::encode(call.abi_encode()))
    }

    fn make_read_calldata(
        src_chain: u64,
        dest_chain: u64,
        sender: Address,
        receiver: Address,
        session_id: U256,
        label: &str,
    ) -> String {
        let call = readMessageCall {
            header: MessageHeader {
                chainSrc: U256::from(src_chain),
                chainDest: U256::from(dest_chain),
                sender,
                receiver,
                sessionId: session_id,
                label: label.to_string(),
            },
        };
        format!("0x{}", hex::encode(call.abi_encode()))
    }

    #[test]
    fn parses_write_call_trace() {
        let mailbox: Address = "0xe5d5d610fb9767df117f4076444b45404201a097"
            .parse()
            .unwrap();
        let caller: Address = "0xf5fe1b951c5cdf2d4299f8e63444ff621cd2fed9"
            .parse()
            .unwrap();
        let receiver: Address = "0x4bcf3d44f2531497e82be4556f380b0a414aa9ce"
            .parse()
            .unwrap();
        let session_id = U256::from(42u64);

        let input = make_write_calldata(
            88888,
            caller,
            receiver,
            session_id,
            "SEND_TOKENS",
            b"payload",
        );

        let trace = json!({
            "from": format!("{caller:#x}"),
            "to": format!("{mailbox:#x}"),
            "input": input,
        });

        let parsed = parse_call_trace(&trace, mailbox, ChainId(77777));
        assert_eq!(parsed.reads.len(), 0);
        assert_eq!(parsed.writes.len(), 1);

        let w = &parsed.writes[0];
        assert_eq!(w.source_chain, ChainId(77777));
        assert_eq!(w.dest_chain, ChainId(88888));
        assert_eq!(w.sender, caller);
        assert_eq!(w.receiver, receiver);
        assert_eq!(w.label, "SEND_TOKENS");
        assert_eq!(w.session_id, session_id);
        assert_eq!(w.data, b"payload");
    }

    #[test]
    fn parses_read_call_trace() {
        let mailbox: Address = "0xe5d5d610fb9767df117f4076444b45404201a097"
            .parse()
            .unwrap();
        let caller: Address = "0xf5fe1b951c5cdf2d4299f8e63444ff621cd2fed9"
            .parse()
            .unwrap();
        let bridge_src: Address = "0x1111111111111111111111111111111111111111"
            .parse()
            .unwrap();
        let session_id = U256::from(99u64);

        let input = make_read_calldata(77777, 88888, bridge_src, caller, session_id, "SEND_TOKENS");

        let trace = json!({
            "from": format!("{caller:#x}"),
            "to": format!("{mailbox:#x}"),
            "input": input,
        });

        let parsed = parse_call_trace(&trace, mailbox, ChainId(88888));
        assert_eq!(parsed.writes.len(), 0);
        assert_eq!(parsed.reads.len(), 1);

        let r = &parsed.reads[0];
        assert_eq!(r.source_chain, ChainId(77777));
        assert_eq!(r.dest_chain, ChainId(88888));
        assert_eq!(r.sender, bridge_src);
        assert_eq!(r.receiver, caller);
        assert_eq!(r.label, "SEND_TOKENS");
        assert_eq!(r.session_id, session_id);
    }

    #[test]
    fn ignores_unknown_selector() {
        let mailbox: Address = "0xe5d5d610fb9767df117f4076444b45404201a097"
            .parse()
            .unwrap();
        let trace = json!({
            "from": "0x1111111111111111111111111111111111111111",
            "to": format!("{mailbox:#x}"),
            "input": "0xdeadbeef00000000",
        });

        let parsed = parse_call_trace(&trace, mailbox, ChainId(1));
        assert_eq!(parsed.reads.len(), 0);
        assert_eq!(parsed.writes.len(), 0);
    }

    #[test]
    fn ignores_chain_ids_that_do_not_fit_u64() {
        let mailbox: Address = "0xe5d5d610fb9767df117f4076444b45404201a097"
            .parse()
            .unwrap();
        let caller: Address = "0xf5fe1b951c5cdf2d4299f8e63444ff621cd2fed9"
            .parse()
            .unwrap();
        let receiver: Address = "0x4bcf3d44f2531497e82be4556f380b0a414aa9ce"
            .parse()
            .unwrap();

        let call = readMessageCall {
            header: MessageHeader {
                chainSrc: U256::from(u64::MAX) + U256::from(1u64),
                chainDest: U256::from(88888),
                sender: caller,
                receiver,
                sessionId: U256::from(1u64),
                label: "SEND_TOKENS".to_string(),
            },
        };
        let trace = json!({
            "from": format!("{caller:#x}"),
            "to": format!("{mailbox:#x}"),
            "input": format!("0x{}", hex::encode(call.abi_encode())),
        });

        let parsed = parse_call_trace(&trace, mailbox, ChainId(88888));
        assert_eq!(parsed.reads.len(), 0);
        assert_eq!(parsed.writes.len(), 0);
    }
}

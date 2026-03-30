//! RPC-backed transaction simulation implementation.

use std::collections::HashMap;

use alloy::consensus::transaction::SignerRecoverable;
use alloy::consensus::{Transaction, TxEnvelope};
use alloy::primitives::{Address, Bytes, U256};
use alloy::rpc::types::eth::TransactionRequest;
use alloy_rpc_types_eth::state::{AccountOverride, StateOverride};
use alloy_rpc_types_trace::geth::{
    CallConfig, DiffMode, GethDebugTracingCallOptions, GethDebugTracingOptions, PreStateConfig,
    PreStateFrame,
};
use async_trait::async_trait;
use compose_mailbox::overrides::{build_mailbox_state_overrides, merge_overrides};
use compose_primitives::{ChainId, CrossRollupDependency, CrossRollupMessage, SimulationResult};
use reqwest::Client;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::{debug, trace, warn};

use crate::error::SimulationError;
use crate::traits::Simulator;
use crate::types::ChainRpcConfig;

#[derive(Debug, Deserialize)]
struct JsonRpcResponse<T> {
    result: Option<T>,
    error: Option<JsonRpcError>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcError {
    code: i64,
    message: String,
    #[serde(default)]
    data: Option<Value>,
}

impl JsonRpcError {
    fn into_message(self) -> String {
        match self.data {
            Some(data) => format!("code {}: {} ({data})", self.code, self.message),
            None => format!("code {}: {}", self.code, self.message),
        }
    }
}

/// RPC-based simulator that uses `debug_traceCall` to simulate transactions
/// and detect mailbox interactions.
#[derive(Debug)]
pub struct RpcSimulator {
    client: Client,
    chains: HashMap<ChainId, ChainRpcConfig>,
    mailbox_address: Option<Address>,
}

impl RpcSimulator {
    pub fn new(chains: Vec<ChainRpcConfig>) -> Self {
        let map = chains.into_iter().map(|c| (c.chain_id, c)).collect();
        Self {
            client: Client::new(),
            chains: map,
            mailbox_address: None,
        }
    }

    pub fn with_mailbox_address(mut self, addr: Address) -> Self {
        self.mailbox_address = Some(addr);
        self
    }

    fn rpc_url(&self, chain_id: ChainId) -> Result<&str, SimulationError> {
        self.chains
            .get(&chain_id)
            .map(|c| c.rpc_url.as_str())
            .ok_or_else(|| {
                SimulationError::Other(format!("no RPC configured for chain {chain_id}"))
            })
    }

    /// Decode RLP-encoded signed transaction bytes into an RPC call object
    /// suitable for `debug_traceCall`.
    fn decode_tx(tx_bytes: &[u8]) -> Result<TransactionRequest, SimulationError> {
        let signed: TxEnvelope = alloy::rlp::Decodable::decode(&mut &tx_bytes[..])
            .map_err(|e| SimulationError::Other(format!("failed to decode tx: {e}")))?;

        let from = signed
            .recover_signer()
            .map_err(|e| SimulationError::Other(format!("failed to recover signer: {e}")))?;

        let mut tx_request = TransactionRequest::default()
            .from(from)
            .gas_limit(signed.gas_limit());

        if let Some(to) = signed.to() {
            tx_request = tx_request.to(to);
        }
        if !signed.input().is_empty() {
            tx_request.input.data = Some(Bytes::copy_from_slice(signed.input()));
        }
        if !signed.value().is_zero() {
            tx_request = tx_request.value(signed.value());
        }

        Ok(tx_request)
    }

    fn call_tracer_options() -> GethDebugTracingOptions {
        GethDebugTracingOptions::call_tracer(CallConfig::default().with_log())
    }

    fn prestate_tracer_options() -> GethDebugTracingOptions {
        GethDebugTracingOptions::prestate_tracer(PreStateConfig {
            diff_mode: Some(true),
            ..Default::default()
        })
    }

    async fn trace_call<T>(
        &self,
        chain_id: ChainId,
        tx_args: &TransactionRequest,
        state_overrides: &StateOverride,
        tracing_opts: GethDebugTracingOptions,
    ) -> Result<T, SimulationError>
    where
        T: DeserializeOwned,
    {
        let url = self.rpc_url(chain_id)?;

        let mut trace_opts = GethDebugTracingCallOptions::new(tracing_opts);
        if !state_overrides.is_empty() {
            trace_opts = trace_opts.with_state_overrides(state_overrides.clone());
        }

        let params = json!([tx_args, "latest", trace_opts]);

        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "debug_traceCall",
            "params": params,
        });

        debug!(chain_id = %chain_id, body = %serde_json::to_string(&body).unwrap_or_default(), "debug_traceCall request");

        let resp = self
            .client
            .post(url)
            .json(&body)
            .send()
            .await
            .map_err(|e| SimulationError::Rpc(e.to_string()))?;

        let result: JsonRpcResponse<T> = resp
            .json()
            .await
            .map_err(|e| SimulationError::Rpc(e.to_string()))?;

        trace!(chain_id = %chain_id, has_error = result.error.is_some(), "debug_traceCall response");

        if let Some(err) = result.error {
            return Err(SimulationError::Rpc(err.into_message()));
        }

        result
            .result
            .ok_or_else(|| SimulationError::Rpc("missing debug_traceCall result".to_string()))
    }

    fn trace_state_overrides(
        trace: PreStateFrame,
    ) -> Result<Option<StateOverride>, SimulationError> {
        let PreStateFrame::Diff(diff) = trace else {
            return Err(SimulationError::Other(
                "prestate tracer returned non-diff response".to_string(),
            ));
        };

        Ok(Self::state_overrides_from_diff(diff))
    }

    fn state_overrides_from_diff(diff: DiffMode) -> Option<StateOverride> {
        let DiffMode { pre, post } = diff;
        let mut overrides = StateOverride::default();

        for (address, post_state) in &post {
            let pre_state = pre.get(address);
            let mut account = AccountOverride {
                balance: post_state.balance,
                nonce: post_state.nonce,
                code: post_state.code.clone(),
                ..Default::default()
            };

            let mut state_diff = post_state.storage.clone();
            if let Some(pre_state) = pre_state {
                for slot in pre_state.storage.keys() {
                    if !post_state.storage.contains_key(slot) {
                        state_diff.insert(*slot, alloy::primitives::B256::ZERO);
                    }
                }
            }
            if !state_diff.is_empty() {
                account.state_diff = Some(state_diff.into_iter().collect());
            }

            if account.balance.is_some()
                || account.nonce.is_some()
                || account.code.is_some()
                || account.state_diff.is_some()
            {
                overrides.insert(*address, account);
            }
        }

        for address in pre.keys().filter(|address| !post.contains_key(*address)) {
            overrides.insert(
                *address,
                AccountOverride {
                    balance: Some(U256::ZERO),
                    nonce: Some(0),
                    code: Some(Bytes::new()),
                    state: Some(Default::default()),
                    ..Default::default()
                },
            );
        }

        if overrides.is_empty() {
            None
        } else {
            Some(overrides)
        }
    }

    fn optional_trace_state_overrides(
        chain_id: ChainId,
        trace_result: Result<PreStateFrame, SimulationError>,
    ) -> Option<StateOverride> {
        match trace_result {
            Ok(trace) => match Self::trace_state_overrides(trace) {
                Ok(overrides) => overrides,
                Err(error) => {
                    warn!(
                        chain_id = %chain_id,
                        error = %error,
                        "Ignoring invalid prestate trace result"
                    );
                    None
                }
            },
            Err(error) => {
                warn!(
                    chain_id = %chain_id,
                    error = %error,
                    "Ignoring prestate trace failure"
                );
                None
            }
        }
    }

    /// Convert parsed mailbox calls into cross-rollup dependencies and messages.
    fn extract_mailbox_data(
        &self,
        trace: &Value,
        chain_id: ChainId,
    ) -> (Vec<CrossRollupDependency>, Vec<CrossRollupMessage>) {
        let Some(mailbox_addr) = self.mailbox_address else {
            return (Vec::new(), Vec::new());
        };

        let parsed = compose_mailbox::parser::parse_call_trace(trace, mailbox_addr, chain_id);

        let dependencies = parsed
            .reads
            .iter()
            .map(|call| CrossRollupDependency {
                source_chain_id: call.source_chain,
                dest_chain_id: call.dest_chain,
                sender: call.sender,
                receiver: call.receiver,
                label: call.label.as_bytes().to_vec(),
                data: None,
                session_id: call.session_id,
            })
            .collect();

        let outbound_messages = parsed
            .writes
            .iter()
            .map(|call| CrossRollupMessage {
                source_chain_id: call.source_chain,
                dest_chain_id: call.dest_chain,
                sender: call.sender,
                receiver: call.receiver,
                label: call.label.clone(),
                data: call.data.clone(),
                session_id: call.session_id,
            })
            .collect();

        (dependencies, outbound_messages)
    }
}

#[async_trait]
impl Simulator for RpcSimulator {
    async fn simulate(
        &self,
        chain_id: ChainId,
        tx: &[u8],
        state_overrides: &StateOverride,
    ) -> Result<SimulationResult, SimulationError> {
        let tx_args = Self::decode_tx(tx)?;

        debug!(chain_id = %chain_id, "Simulating transaction");

        let (trace, prestate_trace) = tokio::join!(
            self.trace_call::<Value>(
                chain_id,
                &tx_args,
                state_overrides,
                Self::call_tracer_options(),
            ),
            self.trace_call::<PreStateFrame>(
                chain_id,
                &tx_args,
                state_overrides,
                Self::prestate_tracer_options(),
            ),
        );
        let trace = trace?;

        let success = trace
            .get("error")
            .map(|e| e.as_str().unwrap_or("").is_empty())
            .unwrap_or(true);

        let error_msg = trace
            .get("error")
            .and_then(|e| e.as_str())
            .map(String::from);

        let (dependencies, outbound_messages) = self.extract_mailbox_data(&trace, chain_id);

        let state_overrides_result = Self::optional_trace_state_overrides(chain_id, prestate_trace);

        Ok(SimulationResult {
            success,
            error: error_msg,
            state_overrides: state_overrides_result,
            dependencies,
            outbound_messages,
        })
    }

    async fn simulate_with_mailbox(
        &self,
        chain_id: ChainId,
        tx: &[u8],
        state_overrides: &StateOverride,
        fulfilled_deps: &[CrossRollupDependency],
    ) -> Result<SimulationResult, SimulationError> {
        // No mailbox injection needed — skip the clone and delegate directly.
        let Some(mailbox_addr) = self.mailbox_address else {
            return self.simulate(chain_id, tx, state_overrides).await;
        };
        if fulfilled_deps.is_empty() {
            return self.simulate(chain_id, tx, state_overrides).await;
        }

        let mut merged = state_overrides.clone();
        if let Some(mailbox_overrides) =
            build_mailbox_state_overrides(chain_id, mailbox_addr, fulfilled_deps)
        {
            merge_overrides(&mut merged, &mailbox_overrides);
        }

        self.simulate(chain_id, tx, &merged).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::B256;
    use alloy_rpc_types_trace::geth::AccountState;
    use std::collections::BTreeMap;

    #[test]
    fn state_overrides_from_diff_preserves_cleared_slots_and_destroyed_accounts() {
        let updated = Address::repeat_byte(0x11);
        let created = Address::repeat_byte(0x22);
        let destroyed = Address::repeat_byte(0x33);
        let slot_a = B256::repeat_byte(0xaa);
        let slot_b = B256::repeat_byte(0xbb);

        let mut pre = BTreeMap::new();
        pre.insert(
            updated,
            AccountState {
                nonce: Some(1),
                storage: BTreeMap::from([
                    (slot_a, B256::repeat_byte(0x01)),
                    (slot_b, B256::repeat_byte(0x02)),
                ]),
                ..Default::default()
            },
        );
        pre.insert(
            destroyed,
            AccountState {
                balance: Some(U256::from(7)),
                nonce: Some(3),
                storage: BTreeMap::from([(slot_a, B256::repeat_byte(0x03))]),
                ..Default::default()
            },
        );

        let mut post = BTreeMap::new();
        post.insert(
            updated,
            AccountState {
                nonce: Some(2),
                storage: BTreeMap::from([(slot_b, B256::repeat_byte(0x04))]),
                ..Default::default()
            },
        );
        post.insert(
            created,
            AccountState {
                balance: Some(U256::from(9)),
                ..Default::default()
            },
        );

        let overrides = RpcSimulator::state_overrides_from_diff(DiffMode { pre, post }).unwrap();

        let updated_account = overrides.get(&updated).unwrap();
        assert_eq!(updated_account.nonce, Some(2));
        let updated_diff = updated_account.state_diff.as_ref().unwrap();
        assert_eq!(updated_diff.get(&slot_a), Some(&B256::ZERO));
        assert_eq!(updated_diff.get(&slot_b), Some(&B256::repeat_byte(0x04)));

        let created_account = overrides.get(&created).unwrap();
        assert_eq!(created_account.balance, Some(U256::from(9)));

        let destroyed_account = overrides.get(&destroyed).unwrap();
        assert_eq!(destroyed_account.balance, Some(U256::ZERO));
        assert_eq!(destroyed_account.nonce, Some(0));
        assert_eq!(destroyed_account.code, Some(Bytes::new()));
        assert_eq!(destroyed_account.state.as_ref().unwrap().len(), 0);
    }
}

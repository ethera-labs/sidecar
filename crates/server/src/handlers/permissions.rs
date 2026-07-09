//! Permission check endpoint for transaction admission.

use alloy::primitives::{Address, B256};
use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};
use sidecar_permissions::{Decision, DenialAction};

use crate::error::ServerError;
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct CheckTxRequest {
    /// Recovered transaction signer.
    pub from: Address,
    /// Whether the transaction creates a contract (`to == null`).
    pub is_create: bool,
    /// Whether the transaction carries native value (`value > 0`).
    pub has_value: bool,
    /// Transaction hash used in permission-denial audit events.
    #[serde(default)]
    pub tx_hash: Option<B256>,
}

#[derive(Debug, Serialize)]
pub struct CheckTxResponse {
    pub allowed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_version: Option<u64>,
}

/// POST /permissions/check-tx - evaluate transaction permissions.
///
/// The handler returns an error when enforcement is not configured because the
/// request cannot be answered authoritatively.
pub async fn handle_check_tx(
    State(state): State<AppState>,
    Json(req): Json<CheckTxRequest>,
) -> Result<Json<CheckTxResponse>, ServerError> {
    let Some(engine) = state.permission_engine.as_ref() else {
        return Err(ServerError::Internal(
            "permission enforcement not configured".to_string(),
        ));
    };

    let response = match engine.evaluate_tx(req.from, req.is_create, req.has_value) {
        Decision::Allow => CheckTxResponse {
            allowed: true,
            reason: None,
            config_version: engine.version(),
        },
        Decision::Deny(reason) => {
            let action = DenialAction::from_tx(req.is_create, req.has_value);
            state
                .coordinator
                .record_tx_denial(req.from, action, reason, req.tx_hash);
            CheckTxResponse {
                allowed: false,
                reason: Some(reason.as_str().to_string()),
                config_version: engine.version(),
            }
        }
    };

    Ok(Json(response))
}

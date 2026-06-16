//! Permission check endpoint called by the builder before pool admission.

use alloy::primitives::Address;
use axum::extract::State;
use axum::Json;
use ethera_permissions::Decision;
use serde::{Deserialize, Serialize};

use crate::error::ServerError;
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct CheckTxRequest {
    /// Recovered transaction signer.
    pub from: String,
    /// Whether the transaction creates a contract (`to == null`).
    pub is_create: bool,
    /// Whether the transaction carries native value (`value > 0`).
    pub has_value: bool,
}

#[derive(Debug, Serialize)]
pub struct CheckTxResponse {
    pub allowed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_version: Option<u64>,
}

/// POST /permissions/check-tx - evaluate UC1 (native send) and UC2 (deploy)
/// for a single transaction. Fails closed: if this sidecar has no engine the
/// caller asked for a check it cannot answer, so the request errors (the builder
/// treats any error as a denial).
pub async fn handle_check_tx(
    State(state): State<AppState>,
    Json(req): Json<CheckTxRequest>,
) -> Result<Json<CheckTxResponse>, ServerError> {
    let Some(engine) = state.permission_engine.as_ref() else {
        return Err(ServerError::Internal(
            "permission enforcement not configured".to_string(),
        ));
    };

    let from = req
        .from
        .parse::<Address>()
        .map_err(|_| ServerError::BadRequest(format!("invalid from address: {}", req.from)))?;

    let response = match engine.evaluate_tx(from, req.is_create, req.has_value) {
        Decision::Allow => CheckTxResponse {
            allowed: true,
            reason: None,
            config_version: engine.version(),
        },
        Decision::Deny(reason) => CheckTxResponse {
            allowed: false,
            reason: Some(reason.as_str().to_string()),
            config_version: engine.version(),
        },
    };

    Ok(Json(response))
}

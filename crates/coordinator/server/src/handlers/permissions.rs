//! Permission check endpoint for transaction admission.

use alloy::primitives::Address;
use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};
use sidecar_permissions::Decision;

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

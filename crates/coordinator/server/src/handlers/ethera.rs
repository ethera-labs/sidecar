//! Builder-facing Ethera callbacks.

use axum::extract::State;
use axum::Json;
use serde::Deserialize;

use crate::error::ServerError;
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct ConfirmIncludedRequest {
    pub instance_ids: Vec<String>,
}

/// POST /ethera/confirm — confirm included XT instance IDs back to the sidecar.
pub async fn handle_confirm_included(
    State(state): State<AppState>,
    Json(req): Json<ConfirmIncludedRequest>,
) -> Result<Json<serde_json::Value>, ServerError> {
    state
        .coordinator
        .confirm_included_xts(&req.instance_ids)
        .await?;

    Ok(Json(serde_json::json!({ "status": "ok" })))
}

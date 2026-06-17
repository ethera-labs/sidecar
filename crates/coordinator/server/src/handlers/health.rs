//! Health and readiness probe handlers.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use crate::state::AppState;

/// GET /health - liveness probe.
pub async fn handle_health() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

/// GET /ready - readiness probe.
///
/// Permission enforcement requires an active policy snapshot before the service
/// is reported as ready.
pub async fn handle_ready(State(state): State<AppState>) -> Response {
    if state.is_ready() {
        (StatusCode::OK, Json(json!({ "status": "ready" }))).into_response()
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "status": "not ready", "reason": "permission config unavailable" })),
        )
            .into_response()
    }
}

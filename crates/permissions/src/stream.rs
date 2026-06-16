//! Config-stream consumer: the permission-specific handler that turns admin
//! backend `/config/stream` frames into engine snapshots, driven by the generic
//! [`ethera_ws::WsClient`].

use std::sync::Arc;
use std::time::{Duration, Instant};

use ethera_ws::{MessageHandler, WsClient};
use tracing::{debug, warn};

use crate::engine::PermissionEngine;
use crate::snapshot::{PolicySnapshot, StreamFrame};

const RECONNECT_BACKOFF: Duration = Duration::from_secs(3);

/// Applies snapshot frames into the engine, tracks connection liveness, and
/// resumes from the last applied version.
#[derive(Debug)]
struct ConfigStreamHandler {
    engine: PermissionEngine,
}

impl MessageHandler for ConfigStreamHandler {
    fn on_message(&self, text: &str) {
        match serde_json::from_str::<StreamFrame>(text) {
            Ok(frame) if frame.r#type == "snapshot" => {
                match PolicySnapshot::from_wire(frame.data, Instant::now()) {
                    Ok(snapshot) => {
                        let version = snapshot.version;
                        self.engine.store(snapshot);
                        debug!(version, "applied permission config snapshot");
                    }
                    // Keep the prior snapshot rather than apply a partial one.
                    Err(err) => warn!(error = %err, "rejecting malformed permission snapshot"),
                }
            }
            Ok(frame) => debug!(kind = %frame.r#type, "ignoring non-snapshot frame"),
            Err(err) => warn!(error = %err, "failed to parse permission config frame"),
        }
    }

    fn on_connected(&self) {
        self.engine.set_connected(true);
    }

    fn on_disconnected(&self) {
        self.engine.set_connected(false);
    }

    fn resume_query(&self) -> Option<String> {
        self.engine
            .version()
            .map(|version| format!("since={version}"))
    }
}

/// Subscription that keeps a [`PermissionEngine`] fed from the admin backend.
#[derive(Debug)]
pub struct ConfigStream {
    client: WsClient,
    handler: Arc<ConfigStreamHandler>,
}

impl ConfigStream {
    pub fn new(url: String, auth_token: Option<String>, engine: PermissionEngine) -> Self {
        Self {
            client: WsClient::new(url, auth_token, RECONNECT_BACKOFF),
            handler: Arc::new(ConfigStreamHandler { engine }),
        }
    }

    /// Stream snapshots into the engine forever (reconnecting on failure).
    pub async fn run(self) {
        self.client.run(self.handler).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: &str = r#"{"type":"snapshot","data":{"version":7,"entities":[],"ruleGroups":[]}}"#;

    #[test]
    fn handler_applies_snapshot_and_reports_resume() {
        let engine = PermissionEngine::new(true);
        let handler = ConfigStreamHandler {
            engine: engine.clone(),
        };

        assert_eq!(handler.resume_query(), None);
        handler.on_message(FRAME);
        assert_eq!(engine.version(), Some(7));
        assert_eq!(handler.resume_query(), Some("since=7".to_string()));
    }

    #[test]
    fn handler_ignores_malformed_frame() {
        let engine = PermissionEngine::new(true);
        let handler = ConfigStreamHandler {
            engine: engine.clone(),
        };

        handler.on_message("not json");
        assert_eq!(engine.version(), None);
    }
}

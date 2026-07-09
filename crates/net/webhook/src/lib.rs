//! Generic JSON webhook client with bounded retries.

use std::time::Duration;

use reqwest::Client;
use serde::Serialize;
use thiserror::Error;

const RETRY_BACKOFF: Duration = Duration::from_millis(100);

#[derive(Debug, Error)]
pub enum WebhookError {
    #[error("webhook request failed: {0}")]
    Request(#[from] reqwest::Error),
}

/// Posts JSON payloads to a fixed endpoint with bounded retries.
#[derive(Debug, Clone)]
pub struct WebhookClient {
    client: Client,
    url: String,
    auth_token: Option<String>,
    max_retries: u32,
}

impl WebhookClient {
    pub fn new(
        url: String,
        auth_token: Option<String>,
        timeout: Duration,
        max_retries: u32,
    ) -> Self {
        let client = Client::builder()
            .timeout(timeout)
            .build()
            .expect("webhook client configuration should be valid");
        Self {
            client,
            url,
            auth_token,
            max_retries,
        }
    }

    /// Post `payload` as JSON, retrying up to `max_retries` times on failure.
    pub async fn post<T: Serialize + ?Sized>(&self, payload: &T) -> Result<(), WebhookError> {
        let mut last_error = None;
        for attempt in 0..=self.max_retries {
            let mut request = self.client.post(&self.url).json(payload);
            if let Some(token) = &self.auth_token {
                request = request.bearer_auth(token);
            }
            match request.send().await.and_then(|r| r.error_for_status()) {
                Ok(_) => return Ok(()),
                Err(err) => last_error = Some(err),
            }
            if attempt < self.max_retries {
                tokio::time::sleep(RETRY_BACKOFF).await;
            }
        }
        Err(WebhookError::Request(last_error.expect(
            "retry loop records the last error before returning",
        )))
    }
}

#[cfg(test)]
mod tests {
    use std::io::ErrorKind;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use serde::Serialize;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;

    #[derive(Serialize)]
    struct Payload {
        value: u64,
    }

    #[tokio::test]
    async fn posts_payload_to_endpoint() {
        let listener = match TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener,
            Err(err) if err.kind() == ErrorKind::PermissionDenied => return,
            Err(err) => panic!("failed to bind test webhook server: {err}"),
        };
        let addr = listener.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));

        let server_hits = hits.clone();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf).await.unwrap();
            server_hits.fetch_add(1, Ordering::SeqCst);
            stream
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n")
                .await
                .unwrap();
        });

        let client = WebhookClient::new(format!("http://{addr}/"), None, Duration::from_secs(2), 0);
        client.post(&Payload { value: 7 }).await.unwrap();
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }
}

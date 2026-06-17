//! Generic reconnecting websocket subscriber.
//!
//! Owns transport concerns such as TLS, authorization, reconnect backoff, and
//! idle-timeout liveness. Message handling stays with [`MessageHandler`].

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::{HeaderValue, AUTHORIZATION};
use tokio_tungstenite::tungstenite::Message;
use tracing::{info, warn};

type WsResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Receives messages and connection lifecycle events from a [`WsClient`].
pub trait MessageHandler: Send + Sync + 'static {
    /// Invoked for each text (or utf-8 binary) frame received from the server.
    fn on_message(&self, text: &str);

    /// Invoked after a connection is established.
    fn on_connected(&self) {}

    /// Invoked after a connection ends (cleanly or with an error), before the
    /// reconnect backoff. Subscribers relying on liveness should treat the
    /// stream as unavailable until the next [`Self::on_connected`].
    fn on_disconnected(&self) {}

    /// Optional query string (without a leading `?`/`&`) appended on each
    /// (re)connect - e.g. `"since=42"` - enabling resumable subscriptions.
    fn resume_query(&self) -> Option<String> {
        None
    }
}

/// Generic websocket client that streams frames to a [`MessageHandler`],
/// reconnecting indefinitely with a fixed backoff.
#[derive(Debug, Clone)]
pub struct WsClient {
    base_url: String,
    auth_token: Option<String>,
    reconnect_backoff: Duration,
    read_idle_timeout: Duration,
}

impl WsClient {
    /// Creates a websocket client with reconnect and read-idle timeouts.
    ///
    /// `read_idle_timeout` must be longer than the server keep-alive interval.
    pub fn new(
        base_url: String,
        auth_token: Option<String>,
        reconnect_backoff: Duration,
        read_idle_timeout: Duration,
    ) -> Self {
        Self {
            base_url,
            auth_token,
            reconnect_backoff,
            read_idle_timeout,
        }
    }

    /// Stream forever, reconnecting with backoff after any disconnect or error.
    pub async fn run<H: MessageHandler>(self, handler: Arc<H>) {
        loop {
            if let Err(err) = self.stream_once(handler.as_ref()).await {
                warn!(error = %err, url = %self.base_url, "websocket subscription dropped, retrying");
            }
            handler.on_disconnected();
            tokio::time::sleep(self.reconnect_backoff).await;
        }
    }

    async fn stream_once(&self, handler: &dyn MessageHandler) -> WsResult<()> {
        let url = self.connect_url(handler.resume_query());
        let request = self.build_request(&url)?;
        let (mut ws, _) = tokio_tungstenite::connect_async(request).await?;
        info!(url = %self.base_url, "websocket connected");
        handler.on_connected();

        loop {
            let message = match tokio::time::timeout(self.read_idle_timeout, ws.next()).await {
                Ok(Some(message)) => message?,
                Ok(None) => break,
                Err(_) => {
                    warn!(url = %self.base_url, "websocket idle timeout, reconnecting");
                    break;
                }
            };
            match message {
                Message::Text(text) => handler.on_message(text.as_str()),
                Message::Binary(bytes) => match std::str::from_utf8(&bytes) {
                    Ok(text) => handler.on_message(text),
                    Err(_) => warn!("ignoring non-utf8 binary frame"),
                },
                // tungstenite queues pong replies for incoming pings.
                Message::Ping(_) | Message::Pong(_) => {}
                Message::Close(_) => {
                    info!("websocket closed by server");
                    break;
                }
                Message::Frame(_) => {}
            }
        }
        Ok(())
    }

    fn connect_url(&self, resume: Option<String>) -> String {
        match resume {
            Some(query) => {
                let separator = if self.base_url.contains('?') {
                    '&'
                } else {
                    '?'
                };
                format!("{}{separator}{query}", self.base_url)
            }
            None => self.base_url.clone(),
        }
    }

    fn build_request(
        &self,
        url: &str,
    ) -> WsResult<tokio_tungstenite::tungstenite::handshake::client::Request> {
        let mut request = url.into_client_request()?;
        if let Some(token) = &self.auth_token {
            if let Ok(value) = HeaderValue::from_str(&format!("Bearer {token}")) {
                request.headers_mut().insert(AUTHORIZATION, value);
            }
        }
        Ok(request)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    use std::time::Instant;

    use super::*;
    use futures::SinkExt;
    use tokio::net::TcpListener;

    #[derive(Default)]
    struct RecordingHandler {
        messages: Mutex<Vec<String>>,
        disconnects: AtomicUsize,
    }

    impl MessageHandler for RecordingHandler {
        fn on_message(&self, text: &str) {
            self.messages.lock().unwrap().push(text.to_string());
        }

        fn on_disconnected(&self) {
            self.disconnects.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn delivers_frames_to_handler() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            ws.send(Message::text("hello")).await.unwrap();
            tokio::time::sleep(Duration::from_millis(200)).await;
        });

        let handler = Arc::new(RecordingHandler::default());
        let client = WsClient::new(
            format!("ws://{addr}/"),
            None,
            Duration::from_secs(3),
            Duration::from_secs(5),
        );
        tokio::spawn(client.run(handler.clone()));

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if !handler.messages.lock().unwrap().is_empty() {
                break;
            }
            assert!(Instant::now() < deadline, "no frame delivered");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(handler.messages.lock().unwrap().as_slice(), ["hello"]);
    }

    #[tokio::test]
    async fn idle_connection_triggers_disconnect() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // Accept the connection and leave it idle until the client timeout
        // reports the disconnect.
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let _ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            tokio::time::sleep(Duration::from_secs(2)).await;
        });

        let handler = Arc::new(RecordingHandler::default());
        let client = WsClient::new(
            format!("ws://{addr}/"),
            None,
            Duration::from_secs(3),
            Duration::from_millis(150),
        );
        tokio::spawn(client.run(handler.clone()));

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if handler.disconnects.load(Ordering::SeqCst) > 0 {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "idle timeout did not trigger on_disconnected"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

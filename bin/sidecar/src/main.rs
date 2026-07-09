//! Sidecar binary entrypoint.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use clap::Parser;
use prometheus_client::registry::Registry;
use sidecar_config::SidecarArgs;
use sidecar_coordinator::builder::CoordinatorBuilder;
use sidecar_coordinator::builder_client::HttpXtBuilderClient;
use sidecar_coordinator::{DefaultCoordinator, VerificationConfig};
use sidecar_mailbox::put_inbox::PutInboxTxBuilder;
use sidecar_mailbox::queue::InMemoryQueue;
use sidecar_metrics::SidecarMetrics;
use sidecar_peer::coordinator::{HttpPeerCoordinator, PeerEntry as RuntimePeerEntry};
use sidecar_peer::sender::PeerMailboxSender;
use sidecar_permissions::{ConfigStream, PermissionEngine};
use sidecar_publisher::PublisherConnection;
use sidecar_server::handlers::publisher::handle_publisher_message;
use sidecar_server::router::build_router;
use sidecar_server::state::AppState;
use sidecar_simulation::rpc::RpcSimulator;
use sidecar_simulation::types::ChainRpcConfig;
use sidecar_transport::client::QuicClient;
use sidecar_transport::config::ClientConfig;
use sidecar_transport::traits::Transport;
use sidecar_webhook::WebhookClient;
use tokio::net::TcpListener;
use tracing::{error, info, warn};

#[tokio::main]
async fn main() -> Result<()> {
    let args = SidecarArgs::parse();

    sidecar_tracing::init(&args.log.level, &args.log.format);

    info!("Starting sidecar");

    let mut registry = Registry::default();
    let metrics = Arc::new(SidecarMetrics::new(&mut registry));

    let permission_engine = build_permission_engine(&args)?;

    let (coordinator, quic_client) = build_coordinator(&args, metrics, permission_engine.clone())?;

    coordinator.start().await?;

    let coordinator_arc = Arc::new(coordinator);

    if let Some(client) = quic_client {
        spawn_publisher_connection(coordinator_arc.clone(), client);
    }

    if let Some(engine) = &permission_engine {
        let stream = ConfigStream::new(
            args.permissions.config_ws_url.clone(),
            args.permissions.auth_token(),
            engine.clone(),
        );
        info!(url = %args.permissions.config_ws_url, "Starting permission config stream");
        tokio::spawn(stream.run());
    }

    let mut state = AppState::from_arc(coordinator_arc).with_registry(registry);
    if let Some(engine) = permission_engine {
        state = state.with_permission_engine(engine);
    }
    let router = build_router(state);

    let listener = TcpListener::bind(&args.server.listen_addr).await?;
    info!(addr = %args.server.listen_addr, "HTTP server listening");

    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    info!("Shutting down");
    Ok(())
}

/// Builds the permission engine when enforcement is enabled.
///
/// A stream URL is required so the engine can receive policy snapshots before
/// serving permission decisions.
fn build_permission_engine(args: &SidecarArgs) -> Result<Option<PermissionEngine>> {
    if !args.permissions.enabled {
        return Ok(None);
    }
    if args.permissions.config_ws_url.trim().is_empty() {
        anyhow::bail!("permissions.enabled requires permissions.config-ws-url");
    }
    Ok(Some(PermissionEngine::new(true)))
}

const AUDIT_WEBHOOK_TIMEOUT: Duration = Duration::from_secs(2);
const AUDIT_WEBHOOK_MAX_RETRIES: u32 = 3;

/// Builds the permission-denial audit webhook client when a URL is configured.
fn build_audit_webhook(args: &SidecarArgs) -> Option<WebhookClient> {
    let url = args.permissions.audit_webhook_url.trim();
    if url.is_empty() {
        return None;
    }
    Some(WebhookClient::new(
        url.to_string(),
        args.permissions.audit_webhook_auth_token(),
        AUDIT_WEBHOOK_TIMEOUT,
        AUDIT_WEBHOOK_MAX_RETRIES,
    ))
}

fn build_coordinator(
    args: &SidecarArgs,
    metrics: Arc<SidecarMetrics>,
    permission_engine: Option<PermissionEngine>,
) -> Result<(DefaultCoordinator, Option<Arc<QuicClient>>)> {
    let chain_id = args.chain.chain_id();

    let mut builder = CoordinatorBuilder::new(chain_id).metrics(metrics);
    if let Some(engine) = permission_engine {
        builder = builder.permission_engine(engine);
    }
    if let Some(webhook) = build_audit_webhook(args) {
        builder = builder.audit_webhook(webhook);
    }
    let chain_rpc = &args.chain.rpc;
    let builder_rpc = args.chain.builder_rpc_url();
    if !builder_rpc.is_empty() {
        match HttpXtBuilderClient::new(builder_rpc.to_string()) {
            Ok(client) => {
                builder = builder.xt_builder_client(Arc::new(client));
            }
            Err(e) => {
                warn!(error = %e, endpoint = builder_rpc, "Failed to configure builder control client");
            }
        }
    }

    let has_rpc = !chain_rpc.is_empty();
    let universal_bridge_mailbox_address = &args.chain.universal_bridge_mailbox_address;
    let has_mailbox = !universal_bridge_mailbox_address.is_empty();
    let has_key = !args.chain.coordinator_key.is_empty();
    if has_rpc && has_mailbox && has_key {
        match PutInboxTxBuilder::new(
            chain_id,
            chain_rpc,
            universal_bridge_mailbox_address,
            args.chain.coordinator_key.clone(),
        ) {
            Ok(put_inbox) => {
                builder = builder.put_inbox_builder(Arc::new(put_inbox));
            }
            Err(e) => {
                warn!(error = %e, endpoint = chain_rpc, "Failed to configure putInbox builder");
            }
        }
    } else if has_mailbox || has_key {
        warn!(
            has_rpc,
            has_mailbox,
            has_coordinator_key = has_key,
            "putInbox builder disabled due to incomplete chain config"
        );
    }

    if !builder_rpc.is_empty() {
        let rpc_chains = vec![ChainRpcConfig {
            chain_id,
            rpc_url: builder_rpc.to_string(),
        }];
        let mut sim = RpcSimulator::new(rpc_chains);
        if !universal_bridge_mailbox_address.is_empty() {
            if let Ok(addr) = universal_bridge_mailbox_address.parse() {
                sim = sim.with_mailbox_address(addr);
            }
        }
        builder = builder.simulator(Arc::new(sim));
    }

    builder = builder.mailbox_queue(Arc::new(InMemoryQueue::new()));

    builder = builder.verification_config(VerificationConfig {
        enabled: args.verification.enabled,
        url: args.verification.url.clone(),
        timeout_ms: args.verification.timeout_ms,
    });

    let peer_entries = args.peers.entries()?;
    if !peer_entries.is_empty() {
        let peers: Vec<RuntimePeerEntry> = peer_entries
            .iter()
            .map(|p| RuntimePeerEntry {
                chain_id: p.chain_id,
                addr: p.addr.clone(),
            })
            .collect();
        let pc = Arc::new(HttpPeerCoordinator::new(peers));
        builder = builder.peer_coordinator(pc);

        let mailbox_peers: Vec<RuntimePeerEntry> = peer_entries
            .iter()
            .map(|p| RuntimePeerEntry {
                chain_id: p.chain_id,
                addr: p.addr.clone(),
            })
            .collect();
        builder = builder.mailbox_sender(Arc::new(PeerMailboxSender::with_peer_entries(
            &mailbox_peers,
        )));
    }

    let quic_client = if args.publisher.enabled && !args.publisher.addr.is_empty() {
        let client_config = ClientConfig {
            addr: args.publisher.addr.clone(),
            client_id: chain_id.0.to_string(),
            reconnect_delay: Duration::from_secs(args.publisher.reconnect_delay_secs),
            max_retries: args.publisher.max_retries,
            ..Default::default()
        };
        match QuicClient::new(client_config) {
            Ok(client) => {
                let conn = PublisherConnection::new(client.clone(), chain_id);
                builder = builder.publisher(Arc::new(conn));
                Some(client)
            }
            Err(e) => {
                warn!(error = %e, "Failed to create QUIC client, running without publisher");
                None
            }
        }
    } else {
        None
    };

    Ok((builder.build()?, quic_client))
}

fn spawn_publisher_connection(coordinator: Arc<DefaultCoordinator>, client: Arc<QuicClient>) {
    tokio::spawn(async move {
        info!("Connecting to publisher");
        if let Err(e) = client.connect_with_retry().await {
            error!(error = %e, "Failed to connect to publisher after retries");
            return;
        }
        info!("Connected to publisher, starting receive loop");

        loop {
            match client.recv().await {
                Ok(data) => {
                    let coord = coordinator.clone();
                    tokio::spawn(async move {
                        handle_publisher_message(coord, data).await;
                    });
                }
                Err(e) => {
                    warn!(error = %e, "Publisher receive error, connection may be lost");
                    break;
                }
            }
        }

        warn!("Publisher receive loop ended");
    });
}

async fn shutdown_signal() {
    tokio::signal::ctrl_c()
        .await
        .expect("failed to install CTRL+C handler");
    info!("Received shutdown signal");
}

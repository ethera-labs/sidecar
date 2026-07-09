//! Primary coordinator type and shared mutable state.

use std::sync::Arc;
use std::time::Duration;

use ethera_spec::ChainId;
use reqwest::Client;
use sidecar_mailbox::traits::MailboxQueue;
use sidecar_peer::traits::PeerCoordinator;
use sidecar_simulation::traits::Simulator;
use tokio::sync::{Notify, RwLock};
use tokio_util::task::TaskTracker;

use sidecar_metrics::SidecarMetrics;
use sidecar_permissions::PermissionEngine;
use sidecar_primitives_traits::{MailboxSender, PublisherClient, PutInboxBuilder, XtBuilderClient};
use sidecar_webhook::WebhookClient;

use crate::nonce_manager::DeferredNonceManager;
use crate::state::CoordinatorState;

/// Inbound verification hook configuration.
#[derive(Debug, Clone, Default)]
pub struct VerificationConfig {
    pub enabled: bool,
    pub url: String,
    pub timeout_ms: u64,
}

/// The default coordinator implementation.
///
/// This struct is cheaply cloneable (all shared state is behind `Arc`).
#[derive(Clone)]
pub struct DefaultCoordinator {
    pub(crate) chain_id: ChainId,
    pub(crate) state: Arc<RwLock<CoordinatorState>>,
    /// Shared with [`CoordinatorState::mailbox_notify`]; held here so the
    /// dependency-wait loop can register interest without taking the state lock.
    pub(crate) mailbox_notify: Arc<Notify>,
    pub(crate) nonce_manager: Arc<DeferredNonceManager>,
    pub(crate) simulator: Option<Arc<dyn Simulator>>,
    pub(crate) publisher: Option<Arc<dyn PublisherClient>>,
    pub(crate) mailbox_sender: Option<Arc<dyn MailboxSender>>,
    pub(crate) mailbox_queue: Option<Arc<dyn MailboxQueue>>,
    pub(crate) peer_coordinator: Option<Arc<dyn PeerCoordinator>>,
    pub(crate) put_inbox_builder: Option<Arc<dyn PutInboxBuilder>>,
    pub(crate) xt_builder_client: Option<Arc<dyn XtBuilderClient>>,
    pub(crate) circ_timeout_ms: u64,
    pub(crate) task_tracker: TaskTracker,
    pub(crate) metrics: Option<Arc<SidecarMetrics>>,
    pub(crate) verification: VerificationConfig,
    pub(crate) verification_client: Option<Client>,
    pub(crate) permission_engine: Option<PermissionEngine>,
    pub(crate) audit_webhook: Option<WebhookClient>,
}

impl std::fmt::Debug for DefaultCoordinator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DefaultCoordinator")
            .field("chain_id", &self.chain_id)
            .field("circ_timeout_ms", &self.circ_timeout_ms)
            .finish()
    }
}

impl DefaultCoordinator {
    #[expect(
        clippy::too_many_arguments,
        reason = "wires all collaborators; CoordinatorBuilder is the ergonomic entry point"
    )]
    pub fn new(
        chain_id: ChainId,
        simulator: Option<Arc<dyn Simulator>>,
        publisher: Option<Arc<dyn PublisherClient>>,
        mailbox_sender: Option<Arc<dyn MailboxSender>>,
        mailbox_queue: Option<Arc<dyn MailboxQueue>>,
        peer_coordinator: Option<Arc<dyn PeerCoordinator>>,
        circ_timeout_ms: u64,
        verification: VerificationConfig,
    ) -> Self {
        let mailbox_notify = Arc::new(Notify::new());
        Self {
            chain_id,
            state: Arc::new(RwLock::new(CoordinatorState::new(mailbox_notify.clone()))),
            mailbox_notify,
            nonce_manager: Arc::new(DeferredNonceManager::new()),
            simulator,
            publisher,
            mailbox_sender,
            mailbox_queue,
            peer_coordinator,
            put_inbox_builder: None,
            xt_builder_client: None,
            circ_timeout_ms,
            task_tracker: TaskTracker::new(),
            metrics: None,
            verification_client: Self::build_verification_client(&verification),
            verification,
            permission_engine: None,
            audit_webhook: None,
        }
    }

    /// Attach a metrics instance to this coordinator.
    pub fn set_metrics(&mut self, metrics: Arc<SidecarMetrics>) {
        self.metrics = Some(metrics);
    }

    /// Attach the permission engine used by cross-rollup validation.
    pub fn set_permission_engine(&mut self, engine: PermissionEngine) {
        self.permission_engine = Some(engine);
    }

    /// Attach the webhook that receives permission-denial audit events.
    pub fn set_audit_webhook(&mut self, webhook: WebhookClient) {
        self.audit_webhook = Some(webhook);
    }

    /// Attach a putInbox signer used for local dependency fulfillment.
    pub fn set_put_inbox_builder(&mut self, builder: Arc<dyn PutInboxBuilder>) {
        self.put_inbox_builder = Some(builder);
    }

    /// Attach a builder-control client for XT reservation lifecycle events.
    pub fn set_xt_builder_client(&mut self, client: Arc<dyn XtBuilderClient>) {
        self.xt_builder_client = Some(client);
    }

    fn build_verification_client(verification: &VerificationConfig) -> Option<Client> {
        if !verification.enabled {
            return None;
        }

        Some(
            Client::builder()
                .timeout(Duration::from_millis(verification.timeout_ms))
                .build()
                .expect("verification client configuration should be valid"),
        )
    }
}

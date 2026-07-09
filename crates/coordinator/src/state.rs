use std::collections::HashMap;
use std::sync::Arc;

use ethera_spec::{ChainId, SequenceNumber};
use ethera_spec_proto::MailboxMessage;
use sidecar_primitives::InstanceId;
use tokio::sync::{oneshot, Notify};

use crate::model::chain_overlay::ChainOverlay;
use crate::model::pending_xt::PendingXt;
use crate::model::publisher_period::PublisherPeriod;

pub(super) type PendingSubmissionResult = Result<InstanceId, String>;
pub(super) type PendingSubmissionSender = oneshot::Sender<PendingSubmissionResult>;

/// Shared coordinator state protected by a `RwLock`.
#[derive(Debug)]
pub(crate) struct CoordinatorState {
    pub pending: HashMap<InstanceId, PendingXt>,
    /// Publisher period and per-period `StartInstance` ordering state.
    pub publisher_period: PublisherPeriod,
    pub last_known_blocks: HashMap<ChainId, u64>,
    /// Monotonic counter for locally-originated XTs in standalone mode.
    pub origin_seq: SequenceNumber,
    /// Per-chain overlay of post-simulation state diffs. Lets XT-B see the
    /// state produced by XT-A within the current coordinator window.
    pub chain_overlay: HashMap<ChainId, ChainOverlay>,
    /// Notified whenever a mailbox message arrives, waking waiting simulations.
    pub mailbox_notify: Arc<Notify>,
    /// Maps XT fingerprints to instance IDs for standalone-mode deduplication.
    pub submitted_fingerprints: HashMap<String, InstanceId>,
    /// Oneshot channels waiting for the publisher to assign an instance ID
    /// after an `XtRequest` is submitted. Keyed by fingerprint.
    pub pending_submissions: HashMap<String, Vec<PendingSubmissionSender>>,
    /// Index from raw `instance_id` bytes → XT id for mailbox routing (O(1)).
    pub mailbox_index: HashMap<Vec<u8>, InstanceId>,
    /// Mailbox messages that arrived before the XT was registered (race buffer).
    ///
    /// When sidecar-a's simulation completes very fast, it may send outbound
    /// mailbox messages to sidecar-b before forwarding the XT.  Messages that
    /// arrive while the XT is unknown are stored here keyed by raw `instance_id`
    /// bytes and drained into `PendingXt::pending_mailbox` the moment the XT
    /// is registered.  Entries are cleared on rollback when the period resets.
    pub mailbox_buffer: HashMap<Vec<u8>, Vec<MailboxMessage>>,
}

impl CoordinatorState {
    // Defensive caps on the orphan mailbox buffer (see `buffer_orphan_mailbox`).
    // Limits total memory a peer can pin between cleanup ticks while still
    // covering legitimate race windows where many CIRC messages arrive ahead of
    // a single XT forward.
    pub(crate) const MAILBOX_BUFFER_MAX_KEYS: usize = 1024;
    pub(crate) const MAILBOX_BUFFER_MAX_PER_KEY: usize = 256;

    pub(super) fn new(mailbox_notify: Arc<Notify>) -> Self {
        Self {
            pending: HashMap::new(),
            publisher_period: PublisherPeriod::default(),
            last_known_blocks: HashMap::new(),
            origin_seq: SequenceNumber(0),
            chain_overlay: HashMap::new(),
            mailbox_notify,
            submitted_fingerprints: HashMap::new(),
            pending_submissions: HashMap::new(),
            mailbox_index: HashMap::new(),
            mailbox_buffer: HashMap::new(),
        }
    }

    /// Buffer a mailbox message for an XT that has not yet been registered.
    ///
    /// Called when a CIRC message arrives before the forwarded XT, which can
    /// happen when sidecar-a's simulation completes in <1 ms and the outbound
    /// message reaches sidecar-b before the XT forward does.
    pub(crate) fn buffer_orphan_mailbox(&mut self, msg: MailboxMessage) {
        let known = self.mailbox_buffer.contains_key(&msg.instance_id);
        if !known && self.mailbox_buffer.len() >= Self::MAILBOX_BUFFER_MAX_KEYS {
            return;
        }
        let bucket = self
            .mailbox_buffer
            .entry(msg.instance_id.clone())
            .or_default();
        if bucket.len() >= Self::MAILBOX_BUFFER_MAX_PER_KEY {
            return;
        }
        bucket.push(msg);
    }

    /// Drain any buffered mailbox messages for the given raw `instance_id` key
    /// and return them so the caller can attach them to the newly registered XT.
    pub(crate) fn drain_mailbox_buffer(&mut self, raw_id: &[u8]) -> Vec<MailboxMessage> {
        self.mailbox_buffer.remove(raw_id).unwrap_or_default()
    }
}

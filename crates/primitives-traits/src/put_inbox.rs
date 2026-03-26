//! Trait for constructing signed putInbox transactions.

use alloy::primitives::Address;
use async_trait::async_trait;
use compose_primitives::CrossRollupDependency;

use crate::error::CoordinatorError;

/// Builder for signed `putInbox` transactions.
#[async_trait]
pub trait PutInboxBuilder: Send + Sync + 'static {
    /// Address of the coordinator signer used for `putInbox`.
    fn signer_address(&self) -> Address;

    /// Return the builder's current pending nonce for the coordinator signer.
    async fn pending_nonce_at(&self) -> Result<u64, CoordinatorError>;

    /// Build a signed `putInbox` transaction with the provided nonce.
    async fn build_put_inbox_tx_with_nonce(
        &self,
        dep: &CrossRollupDependency,
        nonce: u64,
    ) -> Result<Vec<u8>, CoordinatorError>;
}

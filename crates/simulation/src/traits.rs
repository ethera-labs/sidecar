//! Simulation trait definitions used by the coordinator.

use async_trait::async_trait;
use compose_primitives::{ChainId, CrossRollupDependency, SimulationResult, StateOverride};

use crate::error::SimulationError;

/// Simulator runs EVM transaction simulations against an RPC node.
#[async_trait]
pub trait Simulator: Send + Sync + 'static {
    /// Simulate a single transaction with optional state overrides.
    async fn simulate(
        &self,
        chain_id: ChainId,
        tx: &[u8],
        state_overrides: &StateOverride,
    ) -> Result<SimulationResult, SimulationError>;

    /// Simulate a transaction with mailbox context from fulfilled dependencies.
    async fn simulate_with_mailbox(
        &self,
        chain_id: ChainId,
        tx: &[u8],
        state_overrides: &StateOverride,
        fulfilled_deps: &[CrossRollupDependency],
    ) -> Result<SimulationResult, SimulationError>;
}

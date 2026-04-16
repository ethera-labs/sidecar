//! Per-chain state overlay tracking for local XT simulation.

use compose_primitives::StateOverride;

/// Accumulated state overlay for a chain. Gives subsequent simulations the
/// post-state of previously committed local XTs until the overlay is cleared
/// on period change or rollback.
#[derive(Debug, Clone)]
pub struct ChainOverlay {
    pub overlay: StateOverride,
}

impl Default for ChainOverlay {
    fn default() -> Self {
        Self::new()
    }
}

impl ChainOverlay {
    pub fn new() -> Self {
        Self {
            overlay: StateOverride::default(),
        }
    }
}

//! Peer sidecar configuration.

use std::collections::HashSet;
use std::str::FromStr;

use compose_primitives::ChainId;
use thiserror::Error;

/// Peer sidecar addresses for CIRC message delivery.
///
/// Peers are keyed by destination chain ID because mailbox routing is driven by
/// `MailboxMessage.destination_chain`.
#[derive(Debug, Clone, clap::Args)]
pub struct PeerArgs {
    /// Peer sidecars as `CHAIN_ID=URL`, repeatable or comma-delimited via `SIDECAR_PEERS`.
    #[arg(
        long = "peer",
        env = "SIDECAR_PEERS",
        value_name = "CHAIN_ID=URL",
        value_delimiter = ','
    )]
    peers: Vec<PeerEntry>,
}

/// A configured peer sidecar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerEntry {
    pub chain_id: ChainId,
    pub addr: String,
}

/// Invalid peer configuration.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum PeerConfigError {
    #[error("peer must use CHAIN_ID=URL")]
    MissingSeparator,
    #[error("peer chain id is empty")]
    EmptyChainId,
    #[error("invalid peer chain id: {0}")]
    InvalidChainId(String),
    #[error("peer address is empty")]
    EmptyAddress,
    #[error("duplicate peer chain id: {0}")]
    DuplicateChainId(ChainId),
}

impl FromStr for PeerEntry {
    type Err = PeerConfigError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (chain_id, addr) = value
            .split_once('=')
            .ok_or(PeerConfigError::MissingSeparator)?;
        let chain_id = chain_id.trim();
        if chain_id.is_empty() {
            return Err(PeerConfigError::EmptyChainId);
        }

        let chain_id = chain_id
            .parse::<u64>()
            .map_err(|_| PeerConfigError::InvalidChainId(chain_id.to_string()))?;
        let addr = addr.trim();
        if addr.is_empty() {
            return Err(PeerConfigError::EmptyAddress);
        }

        Ok(Self {
            chain_id: ChainId(chain_id),
            addr: addr.to_string(),
        })
    }
}

impl PeerArgs {
    /// Return configured peers after validating chain IDs are unique.
    pub fn entries(&self) -> Result<&[PeerEntry], PeerConfigError> {
        let mut chain_ids = HashSet::with_capacity(self.peers.len());
        for peer in &self.peers {
            if !chain_ids.insert(peer.chain_id) {
                return Err(PeerConfigError::DuplicateChainId(peer.chain_id));
            }
        }

        Ok(&self.peers)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SidecarArgs;
    use clap::Parser;

    #[test]
    fn peer_entries_from_repeated_cli_args() {
        let args = SidecarArgs::parse_from([
            "sidecar",
            "--peer",
            "77777=http://sidecar-a:8090",
            "--peer",
            "88888=http://sidecar-b:8090",
        ]);
        let entries = args.peers.entries().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].chain_id, ChainId(77777));
        assert_eq!(entries[0].addr, "http://sidecar-a:8090");
        assert_eq!(entries[1].chain_id, ChainId(88888));
        assert_eq!(entries[1].addr, "http://sidecar-b:8090");
    }

    #[test]
    fn no_peers_when_none_configured() {
        let args = SidecarArgs::parse_from(["sidecar"]);
        assert!(args.peers.entries().unwrap().is_empty());
    }

    #[test]
    fn peer_entry_requires_chain_id_and_addr() {
        assert_eq!(
            "http://sidecar-a:8090".parse::<PeerEntry>().unwrap_err(),
            PeerConfigError::MissingSeparator
        );
        assert_eq!(
            "=http://sidecar-a:8090".parse::<PeerEntry>().unwrap_err(),
            PeerConfigError::EmptyChainId
        );
        assert_eq!(
            "abc=http://sidecar-a:8090"
                .parse::<PeerEntry>()
                .unwrap_err(),
            PeerConfigError::InvalidChainId("abc".to_string())
        );
        assert_eq!(
            "77777=".parse::<PeerEntry>().unwrap_err(),
            PeerConfigError::EmptyAddress
        );
    }

    #[test]
    fn peer_entries_support_comma_delimited_values() {
        let args = SidecarArgs::parse_from([
            "sidecar",
            "--peer",
            "77777=http://sidecar-a:8090,88888=http://sidecar-b:8090",
        ]);
        let entries = args.peers.entries().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].chain_id, ChainId(77777));
        assert_eq!(entries[1].chain_id, ChainId(88888));
    }

    #[test]
    fn duplicate_peer_chain_ids_are_rejected() {
        let args = SidecarArgs::parse_from([
            "sidecar",
            "--peer",
            "77777=http://sidecar-a:8090",
            "--peer",
            "77777=http://sidecar-b:8090",
        ]);
        let error = args.peers.entries().unwrap_err();
        assert_eq!(error, PeerConfigError::DuplicateChainId(ChainId(77777)));
    }
}

//! QUIC transport primitives used by sidecar networking.

pub mod client;
pub mod config;
pub mod error;
pub mod framing;
pub mod server;
pub(crate) mod socket;
pub mod tls;
pub mod traits;

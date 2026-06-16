//! Entity permission enforcement for the Ethera sidecar.
//!
//! The sidecar is the single authority for permission rules: it consumes the
//! admin backend config stream into an in-memory snapshot and evaluates it for
//! both local transactions (via the builder choke point) and cross-rollup
//! instances (via the 2PC vote path).

pub mod engine;
pub mod snapshot;
pub mod stream;

pub use engine::{Decision, DenyReason, PermissionEngine};
pub use snapshot::{PolicySnapshot, SnapshotData, SnapshotError, StreamFrame};
pub use stream::ConfigStream;

//! Entity permission enforcement for the Ethera sidecar.
//!
//! The sidecar stores policy snapshots in memory and evaluates them for local
//! transaction admission and cross-rollup validation.

pub mod engine;
pub mod snapshot;
pub mod stream;

pub use engine::{Decision, DenyReason, PermissionEngine};
pub use snapshot::{PolicySnapshot, SnapshotData, SnapshotError, StreamFrame};
pub use stream::ConfigStream;

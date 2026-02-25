//! Publisher-backed decision logic.
//!
//! In publisher mode, the publisher collects votes and broadcasts a `Decided`
//! message via QUIC. The sidecar records the decision in
//! `DefaultCoordinator::on_decision` (see `handlers/decision.rs`).
//! No separate handler type is needed.

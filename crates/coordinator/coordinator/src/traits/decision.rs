//! Decision handler trait definitions.
//!
//! The `DecisionHandler` trait was part of an earlier design that separated
//! vote collection from the coordinator. Decision logic is now handled
//! directly by `DefaultCoordinator` methods (`maybe_make_standalone_decision`
//! for standalone mode, `on_decision` for publisher mode). These modules
//! are retained for documentation purposes.

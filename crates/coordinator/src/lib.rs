//! Core sidecar coordinator implementation.
//!
//! It manages the XT lifecycle from start-instance intake through simulation,
//! voting, decision handling, and builder lifecycle control.

mod audit;
pub mod builder;
pub mod builder_client;
mod core;
pub mod handlers;
mod lifecycle;
pub mod model;
mod nonce_manager;
pub mod pipeline;
mod query;
mod state;
mod submission;

/// Maximum number of pending XTs before new submissions are rejected.
pub(crate) const MAX_PENDING_XTS: usize = 100;

pub(crate) use state::CoordinatorState;

pub use core::{DefaultCoordinator, VerificationConfig};
pub use sidecar_primitives_traits::{
    CoordinatorError, MailboxSender, PublisherClient, PutInboxBuilder, XtBuilderClient,
};

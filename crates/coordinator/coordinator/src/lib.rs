//! Core sidecar coordinator implementation.
//!
//! It manages the XT lifecycle from start-instance intake through simulation,
//! voting, decision handling, and builder lifecycle control.

pub mod builder;
pub mod builder_client;
pub mod coordinator;
pub mod handlers;
pub mod model;
mod nonce_manager;
pub mod pipeline;
pub mod traits;

pub use compose_primitives_traits::{
    CoordinatorError, MailboxSender, PublisherClient, PutInboxBuilder, XtBuilderClient,
};

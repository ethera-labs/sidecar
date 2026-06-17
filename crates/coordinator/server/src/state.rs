//! Shared server state container.

use std::sync::Arc;

use compose_coordinator::coordinator::DefaultCoordinator;
use ethera_permissions::PermissionEngine;
use prometheus_client::registry::Registry;
use tokio::sync::Mutex;

/// Shared application state passed to all HTTP handlers.
#[derive(Debug, Clone)]
pub struct AppState {
    pub coordinator: Arc<DefaultCoordinator>,
    /// Prometheus metrics registry (None when running without metrics).
    pub registry: Option<Arc<Mutex<Registry>>>,
    /// Permission engine used by transaction and cross-rollup checks.
    pub permission_engine: Option<PermissionEngine>,
}

impl AppState {
    pub fn new(coordinator: DefaultCoordinator) -> Self {
        Self {
            coordinator: Arc::new(coordinator),
            registry: None,
            permission_engine: None,
        }
    }

    pub fn from_arc(coordinator: Arc<DefaultCoordinator>) -> Self {
        Self {
            coordinator,
            registry: None,
            permission_engine: None,
        }
    }

    pub fn with_registry(mut self, registry: Registry) -> Self {
        self.registry = Some(Arc::new(Mutex::new(registry)));
        self
    }

    pub fn with_permission_engine(mut self, engine: PermissionEngine) -> Self {
        self.permission_engine = Some(engine);
        self
    }

    /// Whether all configured subsystems are ready to serve traffic.
    pub fn is_ready(&self) -> bool {
        self.permission_engine
            .as_ref()
            .is_none_or(|engine| engine.is_ready())
    }
}

//! Service identity and daemon resource definitions.
//!
//! This module contains the core data structures that form the "plumbing"
//! between `ServiceDaemon` and individual service tasks:
//! - `DaemonResources`: Shared state containers (StatusPlane, Shelf, Signals)
//! - `ServiceIdentity`: Per-task lightweight handle for lifecycle management
//! - Task-local bindings (`CURRENT_SERVICE`, `CURRENT_RESOURCES`)

use dashmap::DashMap;
use std::any::Any;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use tokio::task_local;
use tokio_util::sync::CancellationToken;

use crate::models::{ServiceId, ServiceStatus};

// Type aliases for the Shelf
pub(crate) type ShelfValue = Box<dyn Any + Send + Sync>;
pub(crate) type ServiceShelf = DashMap<String, ShelfValue>;
pub(crate) type GlobalShelfMapping = DashMap<String, ServiceShelf>;

/// Shared daemon resources that are owned by `ServiceDaemon` and plumbed to services.
///
/// This struct holds the references to daemon-managed resources. It is passed
/// to each service via the `CURRENT_SERVICE` task-local, enabling services to
/// interact with the daemon's state plane, shelf, and signaling mechanisms
/// without polluting the global namespace.
#[derive(Clone)]
pub struct DaemonResources {
    /// The unified Status Plane: stores the current lifecycle status for each service.
    /// Indexed by `ServiceId` (strong identity) instead of String for safety and performance.
    pub status_plane: Arc<DashMap<ServiceId, ServiceStatus>>,
    /// Shelf for cross-generational state persistence (managed values).
    /// Structure: DashMap<ServiceName, DashMap<Key, Value>>
    /// Kept as String-keyed because shelf data is user-facing and persists across restarts.
    pub shelf: Arc<GlobalShelfMapping>,
    /// Signals for services to reload, indexed by `ServiceId`.
    pub reload_signals: Arc<DashMap<ServiceId, Arc<tokio::sync::Notify>>>,
    /// Global notification for any status change in the STATUS_PLANE.
    pub status_changed: Arc<tokio::sync::Notify>,
}

impl DaemonResources {
    /// Creates a new set of daemon resources.
    pub fn new() -> Self {
        Self {
            status_plane: Arc::new(DashMap::new()),
            shelf: Arc::new(DashMap::new()),
            reload_signals: Arc::new(DashMap::new()),
            status_changed: Arc::new(tokio::sync::Notify::new()),
        }
    }
}

impl Default for DaemonResources {
    fn default() -> Self {
        Self::new()
    }
}

/// Internal identity of a service used to link task-local calls to the daemon's management.
///
/// This is a lightweight handle containing only the lifecycle tokens and a local
/// "handshake done" flag. The actual daemon resources are stored separately in
/// `CURRENT_RESOURCES` (internal, not exposed to users).
#[derive(Clone)]
pub struct ServiceIdentity {
    /// Unique runtime ID — the strong identity for resource lookups.
    pub service_id: ServiceId,
    /// Human-readable name — the weak identity for logging only.
    pub name: String,
    pub cancellation_token: CancellationToken,
    pub reload_token: CancellationToken,
    /// Shared flag: true means the auto-handshake (Initializing->Healthy) has been performed.
    /// Uses Arc to persist the state across TLS clones within the same task generation.
    pub(crate) is_handshake_done: Arc<AtomicBool>,
}

impl ServiceIdentity {
    /// Creates a new ServiceIdentity with the handshake flag set to false.
    pub fn new(
        service_id: ServiceId,
        name: String,
        cancellation_token: CancellationToken,
        reload_token: CancellationToken,
    ) -> Self {
        Self {
            service_id,
            name,
            cancellation_token,
            reload_token,
            is_handshake_done: Arc::new(AtomicBool::new(false)),
        }
    }
}

task_local! {
    /// Internal: The identity of the currently running service task.
    pub(crate) static CURRENT_SERVICE: ServiceIdentity;
    /// Internal: The daemon resources for the current service task.
    pub(crate) static CURRENT_RESOURCES: DaemonResources;
}

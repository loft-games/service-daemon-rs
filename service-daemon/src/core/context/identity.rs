//! Service identity and daemon resource definitions.
//!
//! This module contains the core data structures that form the "plumbing"
//! between `ServiceDaemon` and individual service tasks:
//! - `DaemonResources`: Shared state containers (StatusPlane, Shelf, Signals)
//! - `ServiceIdentity`: Per-task lightweight handle for lifecycle management
//! - Task-local bindings (`CURRENT_SERVICE`, `CURRENT_RESOURCES`)

use dashmap::DashMap;
use std::any::{Any, TypeId};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, OnceLock};
use tokio::task_local;
use tokio_util::sync::CancellationToken;

use crate::models::{ServiceId, ServiceStatus};

// ---------------------------------------------------------------------------
// Process-Level Cancellation Token -- shared by ALL ServiceDaemon instances
// ---------------------------------------------------------------------------

/// Process-wide shutdown token shared by all `ServiceDaemon` instances.
///
/// A single token avoids redundant signal monitoring when multiple daemons
/// coexist (e.g. tag-filtered groups). Also serves as the `wait_shutdown()`
/// fallback when `tokio::task_local` context is unavailable.
static PROCESS_TOKEN: OnceLock<CancellationToken> = OnceLock::new();

pub(crate) fn process_token() -> &'static CancellationToken {
    PROCESS_TOKEN.get_or_init(CancellationToken::new)
}

// Type aliases for the Shelf
pub(crate) type ShelfValue = Box<dyn Any + Send + Sync>;
pub(crate) type ServiceShelf = DashMap<String, ShelfValue>;
pub(crate) type GlobalShelfMapping = DashMap<&'static str, ServiceShelf>;

/// Identity and resource container for a running service daemon.
///
/// Holds the shared state, lifecycle controls, and diagnostics infrastructure.
/// Optimized for minimum atomic overhead.
///
/// **Not `Clone`** -- callers share ownership via `Arc<DaemonResources>`.
pub struct DaemonResources {
    /// The current lifecycle status of all services in the registry.
    /// Indexed by `ServiceId` for safety and performance.
    pub status_plane: DashMap<ServiceId, ServiceStatus>,
    /// Global storage for service-owned arbitrary data (the shelf).
    /// Keyed by service name to support persistence and user-facing inspection.
    pub shelf: GlobalShelfMapping,
    /// Signals for services to reload, indexed by `ServiceId`.
    pub reload_signals: DashMap<ServiceId, Arc<tokio::sync::Notify>>,
    /// Global notification for any status change in the STATUS_PLANE.
    pub status_changed: tokio::sync::Notify,
    /// Type-erased registry for trigger-specific configurations.
    /// Users register configs via `ServiceDaemonBuilder::with_trigger_config<C>`.
    /// Templates read them via `context::trigger_config::<C>()`.
    pub trigger_configs: DashMap<TypeId, Box<dyn Any + Send + Sync>>,
}

impl DaemonResources {
    /// Creates a new set of daemon resources wrapped in `Arc` for shared ownership.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            status_plane: DashMap::new(),
            shelf: DashMap::new(),
            reload_signals: DashMap::new(),
            status_changed: tokio::sync::Notify::new(),
            trigger_configs: DashMap::new(),
        })
    }
}

/// Internal identity of a service used to link task-local calls to the daemon's management.
///
/// This is a lightweight handle containing only the lifecycle tokens and a local
/// "handshake done" flag. The actual daemon resources are stored separately in
/// `CURRENT_RESOURCES` (internal, not exposed to users).
#[derive(Clone)]
pub struct ServiceIdentity {
    /// Unique runtime ID -- the strong identity for resource lookups.
    pub service_id: ServiceId,
    /// Human-readable name -- the weak identity for logging only.
    ///
    /// Points to the static name in `ServiceEntry`, zero-cost to clone.
    pub name: &'static str,
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
        name: &'static str,
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
    pub(crate) static CURRENT_RESOURCES: Arc<DaemonResources>;
}

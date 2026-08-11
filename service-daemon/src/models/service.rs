use crate::ProviderDependencyWatchSet;
use crate::models::{ProviderInitError, RestartPolicy};
use dashmap::DashMap;
use futures::future::BoxFuture;
use linkme::distributed_slice;
use std::any::TypeId;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, OnceLock};
use tokio_util::sync::CancellationToken;
use tracing::warn;
use uuid::Uuid;

pub type ServiceFn = fn(CancellationToken) -> BoxFuture<'static, anyhow::Result<()>>;

// ---------------------------------------------------------------------------
// ServiceEntryId: static registry identity.
// ---------------------------------------------------------------------------

/// A stable identifier for an entry in the link-time `SERVICE_REGISTRY`.
///
/// This identifies the static service or trigger definition, not a running
/// service instance. It is assigned from the entry's position in the distributed
/// slice when `Registry::build()` materializes selected entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "file-logging", derive(serde::Serialize, serde::Deserialize))]
pub struct ServiceEntryId(pub(crate) usize);

impl ServiceEntryId {
    /// Explicitly construct a `ServiceEntryId`.
    #[inline]
    pub const fn new(id: usize) -> Self {
        Self(id)
    }

    /// Get the underlying numeric value.
    #[inline]
    pub const fn value(&self) -> usize {
        self.0
    }
}

impl fmt::Display for ServiceEntryId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "entry#{}", self.0)
    }
}

// ---------------------------------------------------------------------------
// ServiceInstanceId: daemon-local runtime identity.
// ---------------------------------------------------------------------------

/// A unique identifier for a managed service instance within a daemon.
///
/// `ServiceInstanceId` serves as the **strong identity** for all runtime resource
/// lookups (StatusPlane, Shelf, reload signals, running task handles).
/// The human-readable `name` field on `ServiceDescription` is retained only
/// for logging / tracing purposes ("weak identity").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "file-logging", derive(serde::Serialize, serde::Deserialize))]
pub struct ServiceInstanceId(pub(crate) Uuid);

impl ServiceInstanceId {
    /// Explicitly construct a `ServiceInstanceId`.
    #[inline]
    pub const fn new(id: Uuid) -> Self {
        Self(id)
    }

    /// Generate a fresh UUIDv7-backed service instance ID.
    #[inline]
    pub fn new_v7() -> Self {
        Self(Uuid::now_v7())
    }

    /// Get the underlying UUID value.
    #[inline]
    pub const fn as_uuid(&self) -> Uuid {
        self.0
    }
}

impl Default for ServiceInstanceId {
    /// Returns the nil UUID for system/background tasks outside a managed service scope.
    fn default() -> Self {
        Self(Uuid::nil())
    }
}

impl std::str::FromStr for ServiceInstanceId {
    type Err = uuid::Error;

    /// Parses a ServiceInstanceId from a string like "svcinst#UUID" or a bare UUID.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let uuid_part = s.strip_prefix("svcinst#").unwrap_or(s);
        uuid_part.parse::<Uuid>().map(Self::new)
    }
}

impl fmt::Display for ServiceInstanceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "svcinst#{}", self.0)
    }
}

// ---------------------------------------------------------------------------
// TriggerInstanceId: trigger invocation identifier.
// Combines ServiceInstanceId + monotonic sequence for unique instance identification.
// ---------------------------------------------------------------------------

/// A unique identifier for a specific trigger invocation within a service.
///
/// Combines the owning service's [`ServiceInstanceId`] with a monotonically increasing
/// sequence number to produce a globally unique, human-readable instance tag.
///
/// # Performance
///
/// `TriggerInstanceId` is stack-allocated and implements `Copy`. It
/// replaces the previous `format!("{}:{}", service_instance_id, seq)` pattern that
/// required a heap allocation on every trigger dispatch cycle.
///
/// # Display Format
///
/// Formats as `svcinst#UUID:SEQ`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "file-logging", derive(serde::Serialize, serde::Deserialize))]
pub struct TriggerInstanceId {
    /// The service that owns this trigger instance.
    pub service_instance_id: ServiceInstanceId,
    /// Monotonically increasing sequence within this service's lifetime.
    pub seq: u64,
}

impl TriggerInstanceId {
    #[inline]
    pub const fn new(service_instance_id: ServiceInstanceId, seq: u64) -> Self {
        Self {
            service_instance_id,
            seq,
        }
    }
}

impl std::str::FromStr for TriggerInstanceId {
    type Err = anyhow::Error;

    /// Parses a `TriggerInstanceId` from a string like "svcinst#UUID:42".
    /// Support both with and without "svcinst#" prefix on the service component.
    fn from_str(s: &str) -> anyhow::Result<Self> {
        let parts: Vec<&str> = s.splitn(2, ':').collect();
        if parts.len() != 2 {
            return Err(anyhow::anyhow!(
                "invalid trigger_instance_id format: '{}' (expected svcinst#N:SEQ)",
                s
            ));
        }

        let service_instance_id = parts[0]
            .parse::<ServiceInstanceId>()
            .map_err(|e| anyhow::anyhow!("failed to parse service_instance_id component: {}", e))?;
        let seq = parts[1]
            .parse::<u64>()
            .map_err(|e| anyhow::anyhow!("failed to parse sequence component: {}", e))?;

        Ok(Self::new(service_instance_id, seq))
    }
}

impl fmt::Display for TriggerInstanceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.service_instance_id, self.seq)
    }
}

// ---------------------------------------------------------------------------
// ServiceParam / ServicePriority
// ---------------------------------------------------------------------------

/// Describes a dependency parameter for the service registry.
///
/// Each parameter records the argument name, type name (for diagnostics),
/// and a `TypeId` that enables compile-time-safe dependency graph
/// construction at startup.
#[derive(Debug, Clone, Copy)]
pub struct ServiceParam {
    /// The parameter name as declared in the function signature.
    pub name: &'static str,
    /// The inner type name (e.g. "Config"), used for diagnostic output.
    pub type_name: &'static str,
    /// Compiler-assigned type identity for dependency graph edges.
    pub type_id: TypeId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ServicePriority;

impl ServicePriority {
    /// Lowest Priority: External gateways (API, HTTP servers).
    /// Shutdown 1st, Startup Last.
    pub const EXTERNAL: u8 = 0;
    /// Middle Priority: General business logic and triggers.
    pub const DEFAULT: u8 = 50;
    /// Higher Priority: Data providers and storage managers.
    pub const STORAGE: u8 = 80;
    /// Highest Priority: Core system services (Logging, Metrics).
    /// Shutdown Last, Startup 1st.
    pub const SYSTEM: u8 = 100;
}

// ---------------------------------------------------------------------------
// ServiceScheduling: Execution and isolation policy
// ---------------------------------------------------------------------------

/// Defines the static execution mode for a service or trigger body.
///
/// This value is generated into the registry by `#[service]` or `#[trigger]`.
/// It is a declared execution contract, not a runtime policy hint: the daemon
/// does not override it to move a service across scheduling modes.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "file-logging", derive(serde::Serialize, serde::Deserialize))]
pub enum ServiceScheduling {
    /// The default host-runtime integration mode.
    ///
    /// The body runs on the Tokio runtime that calls the daemon handle's `run()`.
    /// Best for most services that do not need a dedicated framework-owned lane.
    #[default]
    Standard,
    /// Runs the body on the daemon-owned low-contention high-priority runtime lane.
    ///
    /// The runtime is created lazily by the daemon handle's `run()` only when the
    /// final registry contains at least one high-priority service or trigger.
    /// This is an explicit declaration, not an overflow target for `Standard`.
    HighPriority,
    /// Runs each generation body in a dedicated OS thread with a private Tokio runtime.
    ///
    /// Supervision, reload, restart, and shutdown coordination remain daemon-managed.
    /// Use this for deterministic responsiveness or strong runtime isolation.
    Isolated,
}

// ---------------------------------------------------------------------------
// ServiceEntry (static, compile-time) -- now includes `tags`
// ---------------------------------------------------------------------------

/// A static entry in the service registry (generated by `#[service]` macro).
///
/// Each entry carries an optional `tags` slice that enables tag-based
/// filtering when constructing a `Registry` instance.
pub struct ServiceEntry {
    pub name: &'static str,
    pub module: &'static str,
    pub params: &'static [ServiceParam],
    pub wrapper: fn(CancellationToken) -> BoxFuture<'static, anyhow::Result<()>>,
    pub watcher: Option<fn() -> ProviderDependencyWatchSet>,
    pub priority: u8,
    /// Execution scheduling and isolation policy.
    pub scheduling: ServiceScheduling,
    /// Compile-time tags assigned via `#[service(tags = ["core", "infra"])]`.
    /// Defaults to an empty slice when no tags are specified.
    pub tags: &'static [&'static str],
}

// ---------------------------------------------------------------------------
// ServiceHandle: daemon-local static service entry capability.
// ---------------------------------------------------------------------------

/// A handle to a service entry selected by the current daemon registry.
///
/// The handle identifies the static service definition, not a running instance.
/// Runtime instances use [`ServiceInstanceId`] and are allocated when a service
/// is materialized by the daemon.
#[derive(Clone, Copy)]
pub struct ServiceHandle {
    entry_id: ServiceEntryId,
    entry: &'static ServiceEntry,
}

impl ServiceHandle {
    #[inline]
    pub(crate) const fn new(entry_id: ServiceEntryId, entry: &'static ServiceEntry) -> Self {
        Self { entry_id, entry }
    }

    /// Static registry entry ID for this service definition.
    #[inline]
    pub const fn entry_id(&self) -> ServiceEntryId {
        self.entry_id
    }

    /// Static registry entry metadata for this service definition.
    #[inline]
    pub const fn entry(&self) -> &'static ServiceEntry {
        self.entry
    }

    /// Human-readable service function name.
    #[inline]
    pub const fn name(&self) -> &'static str {
        self.entry.name
    }

    /// Module path where the service was registered.
    #[inline]
    pub const fn module(&self) -> &'static str {
        self.entry.module
    }
}

impl PartialEq for ServiceHandle {
    fn eq(&self, other: &Self) -> bool {
        self.entry_id == other.entry_id
    }
}

impl Eq for ServiceHandle {}

impl Hash for ServiceHandle {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.entry_id.hash(state);
    }
}

impl fmt::Debug for ServiceHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ServiceHandle")
            .field("entry_id", &self.entry_id)
            .field("name", &self.entry.name)
            .field("module", &self.entry.module)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// ServiceInstanceHandle: daemon-local runtime service instance capability.
// ---------------------------------------------------------------------------

/// A handle to a daemon-local runtime service instance.
///
/// This handle identifies one materialized service instance. It carries both
/// the runtime [`ServiceInstanceId`] and the static [`ServiceEntryId`] so callers
/// can group instances back to their service definition without treating the two
/// IDs as interchangeable.
#[derive(Clone, Copy)]
pub struct ServiceInstanceHandle {
    instance_id: ServiceInstanceId,
    entry_id: ServiceEntryId,
    entry: &'static ServiceEntry,
}

impl ServiceInstanceHandle {
    #[inline]
    pub(crate) const fn new(
        instance_id: ServiceInstanceId,
        entry_id: ServiceEntryId,
        entry: &'static ServiceEntry,
    ) -> Self {
        Self {
            instance_id,
            entry_id,
            entry,
        }
    }

    /// Runtime instance ID for this service instance.
    #[inline]
    pub const fn instance_id(&self) -> ServiceInstanceId {
        self.instance_id
    }

    /// Static registry entry ID for this service definition.
    #[inline]
    pub const fn entry_id(&self) -> ServiceEntryId {
        self.entry_id
    }

    /// Static registry entry metadata for this service definition.
    #[inline]
    pub const fn entry(&self) -> &'static ServiceEntry {
        self.entry
    }

    /// Human-readable service function name.
    #[inline]
    pub const fn name(&self) -> &'static str {
        self.entry.name
    }

    /// Module path where the service was registered.
    #[inline]
    pub const fn module(&self) -> &'static str {
        self.entry.module
    }
}

impl PartialEq for ServiceInstanceHandle {
    fn eq(&self, other: &Self) -> bool {
        self.instance_id == other.instance_id && self.entry_id == other.entry_id
    }
}

impl Eq for ServiceInstanceHandle {}

impl Hash for ServiceInstanceHandle {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.instance_id.hash(state);
        self.entry_id.hash(state);
    }
}

impl fmt::Debug for ServiceInstanceHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ServiceInstanceHandle")
            .field("instance_id", &self.instance_id)
            .field("entry_id", &self.entry_id)
            .field("name", &self.entry.name)
            .field("module", &self.entry.module)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// ServiceInstanceRegistry: daemon-local runtime instance registry.
// ---------------------------------------------------------------------------

/// A daemon-local runtime record for one materialized service instance.
#[derive(Clone)]
pub(crate) struct ServiceInstanceRecord {
    handle: ServiceInstanceHandle,
    cancellation_token: CancellationToken,
}

impl ServiceInstanceRecord {
    #[inline]
    pub(crate) fn new(
        handle: ServiceInstanceHandle,
        cancellation_token: CancellationToken,
    ) -> Self {
        Self {
            handle,
            cancellation_token,
        }
    }

    #[inline]
    pub(crate) fn handle(&self) -> ServiceInstanceHandle {
        self.handle
    }

    #[inline]
    pub(crate) fn instance_id(&self) -> ServiceInstanceId {
        self.handle.instance_id()
    }

    #[inline]
    pub(crate) fn entry_id(&self) -> ServiceEntryId {
        self.handle.entry_id()
    }

    #[inline]
    pub(crate) fn entry(&self) -> &'static ServiceEntry {
        self.handle.entry()
    }

    #[inline]
    pub(crate) fn name(&self) -> &'static str {
        self.handle.name()
    }

    #[inline]
    pub(crate) fn priority(&self) -> u8 {
        self.entry().priority
    }

    #[inline]
    #[allow(dead_code)]
    pub(crate) fn params(&self) -> &'static [ServiceParam] {
        self.entry().params
    }

    #[inline]
    pub(crate) fn scheduling(&self) -> ServiceScheduling {
        self.entry().scheduling
    }

    #[inline]
    pub(crate) fn cancellation_token(&self) -> CancellationToken {
        self.cancellation_token.clone()
    }
}

/// Daemon-local registry of materialized service instances.
///
/// Static service definitions remain in the process-wide service catalog. This
/// registry tracks only instances owned by one daemon.
#[derive(Default)]
pub(crate) struct ServiceInstanceRegistry {
    by_instance: DashMap<ServiceInstanceId, ServiceInstanceRecord>,
    by_entry: DashMap<ServiceEntryId, HashSet<ServiceInstanceId>>,
}

impl ServiceInstanceRegistry {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn insert(&self, record: ServiceInstanceRecord) {
        let instance_id = record.instance_id();
        let entry_id = record.entry_id();
        self.by_instance.insert(instance_id, record);
        self.by_entry
            .entry(entry_id)
            .or_default()
            .insert(instance_id);
    }

    pub(crate) fn get(&self, instance_id: ServiceInstanceId) -> Option<ServiceInstanceRecord> {
        self.by_instance
            .get(&instance_id)
            .map(|record| record.clone())
    }

    pub(crate) fn handles_for_entry(&self, entry_id: ServiceEntryId) -> Vec<ServiceInstanceHandle> {
        let mut handles = self
            .by_entry
            .get(&entry_id)
            .map(|entry_instances| {
                entry_instances
                    .iter()
                    .filter_map(|instance_id| self.get(*instance_id).map(|record| record.handle()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        handles.sort_by_key(|handle| handle.instance_id());
        handles
    }

    pub(crate) fn records(&self) -> Vec<ServiceInstanceRecord> {
        let mut records = self
            .by_instance
            .iter()
            .map(|entry| entry.value().clone())
            .collect::<Vec<_>>();
        records.sort_by_key(|record| record.instance_id());
        records
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.by_instance.len()
    }
}

#[derive(Clone, Copy)]
pub(crate) struct GlobalServiceEntryRecord {
    pub entry_id: ServiceEntryId,
    pub entry: &'static ServiceEntry,
}

/// Process-wide catalog built from the link-time service registry.
pub(crate) struct ServiceCatalog {
    records: Vec<GlobalServiceEntryRecord>,
    by_wrapper: HashMap<ServiceFn, ServiceEntryId>,
    by_tag: HashMap<&'static str, Vec<ServiceEntryId>>,
}

impl ServiceCatalog {
    fn build() -> Self {
        let mut records = Vec::new();
        let mut by_wrapper = HashMap::new();
        let mut by_tag: HashMap<&'static str, Vec<ServiceEntryId>> = HashMap::new();

        for (idx, entry) in SERVICE_REGISTRY.iter().enumerate() {
            let entry_id = ServiceEntryId::new(idx);
            records.push(GlobalServiceEntryRecord { entry_id, entry });
            by_wrapper.insert(entry.wrapper, entry_id);
            for tag in entry.tags {
                by_tag.entry(*tag).or_default().push(entry_id);
            }
        }

        Self {
            records,
            by_wrapper,
            by_tag,
        }
    }

    #[inline]
    pub(crate) fn get(entry_id: ServiceEntryId) -> Option<GlobalServiceEntryRecord> {
        global_service_catalog()
            .records
            .get(entry_id.value())
            .copied()
            .filter(|record| record.entry_id == entry_id)
    }

    #[inline]
    pub(crate) fn entry_id_for_wrapper(wrapper: ServiceFn) -> Option<ServiceEntryId> {
        global_service_catalog().by_wrapper.get(&wrapper).copied()
    }

    #[inline]
    pub(crate) fn tag_exists(tag: &'static str) -> bool {
        global_service_catalog().by_tag.contains_key(tag)
    }

    pub(crate) fn project(
        include_tags: &[&'static str],
        exclude_tags: &[&'static str],
    ) -> ServiceCatalogProjection {
        let catalog = global_service_catalog();
        let mut selected_ids = HashSet::new();

        if include_tags.is_empty() {
            selected_ids.extend(catalog.records.iter().map(|record| record.entry_id));
        } else {
            for tag in include_tags {
                if let Some(entry_ids) = catalog.by_tag.get(tag) {
                    selected_ids.extend(entry_ids.iter().copied());
                }
            }
        }

        for tag in exclude_tags {
            if let Some(entry_ids) = catalog.by_tag.get(tag) {
                for entry_id in entry_ids {
                    selected_ids.remove(entry_id);
                }
            }
        }

        let entry_ids = catalog
            .records
            .iter()
            .filter_map(|record| {
                selected_ids
                    .contains(&record.entry_id)
                    .then_some(record.entry_id)
            })
            .collect();

        ServiceCatalogProjection::new(entry_ids)
    }
}

static SERVICE_CATALOG: OnceLock<ServiceCatalog> = OnceLock::new();

pub(crate) fn global_service_catalog() -> &'static ServiceCatalog {
    SERVICE_CATALOG.get_or_init(ServiceCatalog::build)
}

/// Daemon-local view of service entries selected from the global catalog.
pub(crate) struct ServiceCatalogProjection {
    entry_ids: Vec<ServiceEntryId>,
    selected: HashSet<ServiceEntryId>,
}

impl ServiceCatalogProjection {
    fn new(entry_ids: Vec<ServiceEntryId>) -> Self {
        let selected = entry_ids.iter().copied().collect();
        Self {
            entry_ids,
            selected,
        }
    }

    pub(crate) fn entry_ids(&self) -> &[ServiceEntryId] {
        &self.entry_ids
    }

    pub(crate) fn contains(&self, entry_id: ServiceEntryId) -> bool {
        self.selected.contains(&entry_id)
    }

    pub(crate) fn merge(&self, other: &Self) -> Arc<Self> {
        let mut entry_ids = self.entry_ids.clone();
        let mut selected = self.selected.clone();

        for entry_id in &other.entry_ids {
            if selected.insert(*entry_id) {
                entry_ids.push(*entry_id);
            }
        }

        Arc::new(Self {
            entry_ids,
            selected,
        })
    }

    pub(crate) fn resolve_handle(&self, entry_id: ServiceEntryId) -> Option<ServiceHandle> {
        self.contains(entry_id)
            .then(|| ServiceCatalog::get(entry_id))
            .flatten()
            .map(|record| ServiceHandle::new(record.entry_id, record.entry))
    }
}

// ---------------------------------------------------------------------------
// ServiceDescription (daemon-local): static entry plus runtime instances.
// ---------------------------------------------------------------------------

/// Daemon-local description of a selected service definition.
///
/// Holds a reference to the underlying static `ServiceEntry` from the
/// `SERVICE_REGISTRY` and exposes the materialized runtime instances owned by
/// the current daemon for this entry.
///
/// Use accessor methods (`name()`, `priority()`, etc.) to read static
/// metadata without field duplication.
pub struct ServiceDescription {
    /// Static registry entry ID assigned from `SERVICE_REGISTRY`.
    pub entry_id: ServiceEntryId,
    /// Reference to the static entry that registered this service.
    pub entry: &'static ServiceEntry,
    /// Daemon-local runtime instance registry.
    pub(crate) instance_registry: Arc<ServiceInstanceRegistry>,
}

impl ServiceDescription {
    /// Human-readable name for logging.
    #[inline]
    pub fn name(&self) -> &'static str {
        self.entry.name
    }

    /// Priority level (higher = started earlier).
    #[inline]
    pub fn priority(&self) -> u8 {
        self.entry.priority
    }

    /// Dependency parameters with `TypeId` for graph analysis.
    #[inline]
    pub fn params(&self) -> &'static [ServiceParam] {
        self.entry.params
    }

    /// Execution scheduling and isolation policy.
    #[inline]
    pub fn scheduling(&self) -> ServiceScheduling {
        self.entry.scheduling
    }

    /// Runtime instance handles materialized for this selected service entry.
    #[inline]
    pub fn instances(&self) -> Vec<ServiceInstanceHandle> {
        self.instance_registry.handles_for_entry(self.entry_id)
    }
}

// ---------------------------------------------------------------------------
// ServiceStatus (unchanged)
// ---------------------------------------------------------------------------

/// Represents the unified lifecycle status of a service.
///
/// This is the single source of truth for all service status, combining
/// both the external (daemon-observed) and internal (service-perceived) views.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceStatus {
    /// The service is starting for the first time in this process session.
    Initializing,
    /// The service has been restarted after a configuration or dependency change
    /// and is ready to restore state from the shelf.
    Restoring,
    /// The service is recovering from a previous crash (panic or error).
    /// Contains the error message from the previous generation.
    Recovering(String),
    /// The service is running normally following a successful `done()` call.
    Healthy,
    /// A dependency changed; the service should save its state and call `done()`.
    NeedReload,
    /// The daemon is shutting down; the service should save its state and call `done()`.
    ShuttingDown,
    /// The service has completed its clean exit handshake and is ready for collection.
    Terminated,
}

/// The global service registry -- services register themselves here via `#[service]` macro.
// `linkme` expands to `#[link_section]`, which edition 2024 considers unsafe.
#[allow(unsafe_code)]
#[distributed_slice]
pub static SERVICE_REGISTRY: [ServiceEntry];

/// The link-time provider registry -- providers register themselves here via `#[provider]` macro.
///
/// Each entry records the provider's type identity and its dependency parameters,
/// enabling full dependency graph construction (including Provider->Provider edges)
/// at startup for cycle detection.
// Same: `linkme` `#[link_section]` in edition 2024.
#[allow(unsafe_code)]
#[distributed_slice]
pub static PROVIDER_REGISTRY: [ProviderEntry];

// ---------------------------------------------------------------------------
// ProviderEntry (static, compile-time) -- provider dependency metadata
// ---------------------------------------------------------------------------

/// A static entry in the provider registry (generated by `#[provider]` macro).
///
/// Unlike `ServiceEntry`, providers do not carry a wrapper function or priority.
/// Their primary purpose is to expose dependency metadata for graph analysis.
///
/// Additionally, providers may opt into eager initialization via `eager = true`.
pub struct ProviderEntry {
    /// Provider type name (e.g. "ConnectionString").
    pub name: &'static str,
    /// Module path where the provider is defined.
    pub module: &'static str,
    /// `TypeId` of the provided type itself (used as graph node identity).
    pub type_id: TypeId,
    /// Dependencies this provider requires (other provider types).
    pub params: &'static [ServiceParam],
    /// Whether this provider should be initialized during daemon startup
    /// (when reachable from the selected service set).
    pub eager: bool,
    /// Type-erased initializer that seeds the effective provider slot.
    ///
    /// Implementations are macro-generated and are expected to call into the
    /// scoped provider bridge so daemon startup initializes the current daemon's
    /// effective slot, falling back to the generated root slot when no daemon
    /// scope is active.
    pub init: fn(
        RestartPolicy,
        tokio_util::sync::CancellationToken,
    ) -> futures::future::BoxFuture<'static, Result<(), ProviderInitError>>,
}

// ---------------------------------------------------------------------------
// Registry: Tag-filtered, ID-allocating service container.
// ---------------------------------------------------------------------------

/// A filtered, ID-allocated collection of services ready for a `ServiceDaemon`.
///
/// Built via `Registry::builder()`, which lazily references the static
/// `SERVICE_REGISTRY` and only materializes matching entries on `.build()`.
///
/// # Examples
/// ```rust,ignore
/// // All services (default)
/// let reg = Registry::builder().build();
///
/// // Only services tagged "infra"
/// let reg = Registry::builder().with_tag("infra").build();
///
/// // Multiple tags, excluding experimental
/// let reg = Registry::builder()
///     .with_tags(["core", "io"])
///     .exclude_tag("experimental")
///     .build();
/// ```
pub struct Registry {
    /// The materialised, ID-bearing service descriptions.
    pub(crate) services: Vec<ServiceDescription>,
    /// Daemon-local projection of static registry entries selected for this registry.
    pub(crate) projection: Arc<ServiceCatalogProjection>,
    /// Daemon-local registry of materialized service instances.
    pub(crate) instance_registry: Arc<ServiceInstanceRegistry>,
}

impl Registry {
    /// Start building a new `Registry` from the global static pool.
    #[must_use]
    pub fn builder() -> RegistryBuilder {
        RegistryBuilder::new()
    }

    /// Return the number of services in this registry.
    #[must_use]
    pub fn len(&self) -> usize {
        self.services.len()
    }

    /// Returns `true` if this registry contains no services.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.services.is_empty()
    }

    /// Borrow the materialized service descriptions in this registry.
    #[must_use]
    pub fn services(&self) -> &[ServiceDescription] {
        &self.services
    }

    pub(crate) fn into_parts(
        self,
    ) -> (
        Vec<ServiceDescription>,
        Arc<ServiceCatalogProjection>,
        Arc<ServiceInstanceRegistry>,
    ) {
        (self.services, self.projection, self.instance_registry)
    }
}

// ---------------------------------------------------------------------------
// RegistryBuilder -- lazy, zero-copy until .build()
// ---------------------------------------------------------------------------

/// Builder for constructing a `Registry` with optional tag filters.
///
/// Holds only lightweight filter state until `.build()` is called.
/// The `.build()` method is **infallible** -- it always succeeds.
pub struct RegistryBuilder {
    /// Include-filter: only entries matching at least one of these tags.
    /// Empty means "include all".
    include_tags: Vec<&'static str>,
    /// Exclude-filter: entries matching any of these tags are removed.
    exclude_tags: Vec<&'static str>,
}

impl RegistryBuilder {
    fn new() -> Self {
        Self {
            include_tags: Vec::new(),
            exclude_tags: Vec::new(),
        }
    }

    /// Only include services that carry the given tag.
    ///
    /// Multiple calls to `with_tag` are additive (OR semantics).
    #[must_use]
    pub fn with_tag(mut self, tag: &'static str) -> Self {
        self.include_tags.push(tag);
        self
    }

    /// Only include services that carry at least one of the given tags.
    #[must_use]
    pub fn with_tags<I, S>(mut self, tags: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<&'static str>,
    {
        for tag in tags {
            self.include_tags.push(tag.into());
        }
        self
    }

    /// Exclude services carrying the given tag, even if they match an include filter.
    #[must_use]
    pub fn exclude_tag(mut self, tag: &'static str) -> Self {
        self.exclude_tags.push(tag);
        self
    }

    /// Materialise the registry by filtering the global static pool.
    ///
    /// This method is **infallible** -- it always returns a valid `Registry`,
    /// even if zero services match. Tags that match nothing produce a `WARN`
    /// log but do not cause errors.
    #[must_use]
    pub fn build(self) -> Registry {
        // Warn for include tags that match nothing
        for tag in &self.include_tags {
            if !ServiceCatalog::tag_exists(tag) {
                warn!(
                    "Registry::build() -- tag '{}' did not match any registered service",
                    tag
                );
            }
        }

        let projection = Arc::new(ServiceCatalog::project(
            &self.include_tags,
            &self.exclude_tags,
        ));
        let instance_registry = Arc::new(ServiceInstanceRegistry::new());
        let mut services = Vec::new();

        for entry_id in projection.entry_ids() {
            let record =
                ServiceCatalog::get(*entry_id).expect("projected service entry must exist");
            let instance_id = ServiceInstanceId::new_v7();
            let handle = ServiceInstanceHandle::new(instance_id, record.entry_id, record.entry);
            instance_registry.insert(ServiceInstanceRecord::new(handle, CancellationToken::new()));
            let service = ServiceDescription {
                entry_id: record.entry_id,
                entry: record.entry,
                instance_registry: instance_registry.clone(),
            };
            services.push(service);
        }

        Registry {
            services,
            projection,
            instance_registry,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_registry_entry_id_first_wrapper(
        _: CancellationToken,
    ) -> BoxFuture<'static, anyhow::Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn test_registry_entry_id_second_wrapper(
        _: CancellationToken,
    ) -> BoxFuture<'static, anyhow::Result<()>> {
        Box::pin(async { Ok(()) })
    }

    #[allow(unsafe_code)]
    #[distributed_slice(SERVICE_REGISTRY)]
    static TEST_REGISTRY_ENTRY_ID_FIRST: ServiceEntry = ServiceEntry {
        name: "test_registry_entry_id_first",
        module: "models::service::tests",
        params: &[],
        wrapper: test_registry_entry_id_first_wrapper,
        watcher: None,
        priority: 50,
        scheduling: ServiceScheduling::Standard,
        tags: &["__test_registry_entry_id_first__"],
    };

    #[allow(unsafe_code)]
    #[distributed_slice(SERVICE_REGISTRY)]
    static TEST_REGISTRY_ENTRY_ID_SECOND: ServiceEntry = ServiceEntry {
        name: "test_registry_entry_id_second",
        module: "models::service::tests",
        params: &[],
        wrapper: test_registry_entry_id_second_wrapper,
        watcher: None,
        priority: 50,
        scheduling: ServiceScheduling::Standard,
        tags: &["__test_registry_entry_id_second__"],
    };

    #[test]
    fn test_service_scheduling_default() {
        assert_eq!(ServiceScheduling::default(), ServiceScheduling::Standard);
    }

    #[test]
    fn test_service_entry_with_scheduling() {
        let entry = ServiceEntry {
            name: "test",
            module: "test_mod",
            params: &[],
            wrapper: |_| Box::pin(async { Ok(()) }),
            watcher: None,
            priority: 50,
            scheduling: ServiceScheduling::Isolated,
            tags: &[],
        };
        assert_eq!(entry.scheduling, ServiceScheduling::Isolated);
    }

    #[test]
    fn tag_filtered_registry_preserves_original_entry_id() {
        let registry = Registry::builder()
            .with_tag("__test_registry_entry_id_second__")
            .build();
        let service = registry
            .services
            .iter()
            .find(|service| service.name() == "test_registry_entry_id_second")
            .expect("test service should be selected by tag");
        let original_index = SERVICE_REGISTRY
            .iter()
            .enumerate()
            .find_map(|(idx, entry)| (entry.name == "test_registry_entry_id_second").then_some(idx))
            .expect("test service should exist in SERVICE_REGISTRY");

        assert_eq!(service.entry_id, ServiceEntryId::new(original_index));
        let instance = service
            .instances()
            .first()
            .copied()
            .expect("auto-start service should have one instance handle");
        assert_eq!(
            instance.instance_id().as_uuid().get_version_num(),
            7,
            "auto-start singleton service instances should receive UUIDv7 IDs"
        );
    }

    #[test]
    fn global_catalog_projects_tags_in_registry_order() {
        let projection = ServiceCatalog::project(
            &[
                "__test_registry_entry_id_second__",
                "__test_registry_entry_id_first__",
            ],
            &[],
        );
        let entry_ids = projection.entry_ids();
        let first_index = SERVICE_REGISTRY
            .iter()
            .enumerate()
            .find_map(|(idx, entry)| (entry.name == "test_registry_entry_id_first").then_some(idx))
            .expect("first test service should exist in SERVICE_REGISTRY");
        let second_index = SERVICE_REGISTRY
            .iter()
            .enumerate()
            .find_map(|(idx, entry)| (entry.name == "test_registry_entry_id_second").then_some(idx))
            .expect("second test service should exist in SERVICE_REGISTRY");
        let expected = if first_index < second_index {
            vec![
                ServiceEntryId::new(first_index),
                ServiceEntryId::new(second_index),
            ]
        } else {
            vec![
                ServiceEntryId::new(second_index),
                ServiceEntryId::new(first_index),
            ]
        };

        assert_eq!(entry_ids, expected.as_slice());
    }

    #[test]
    fn global_catalog_is_process_singleton() {
        assert!(std::ptr::eq(
            global_service_catalog(),
            global_service_catalog()
        ));
    }

    #[test]
    fn global_catalog_resolves_wrapper_to_entry_id() {
        let entry_id = ServiceCatalog::entry_id_for_wrapper(test_registry_entry_id_second_wrapper)
            .expect("wrapper should be indexed by global catalog");
        let record = ServiceCatalog::get(entry_id).expect("entry ID should resolve to record");

        assert_eq!(record.entry.name, "test_registry_entry_id_second");
    }

    #[test]
    fn service_instance_handle_exposes_runtime_and_entry_identity() {
        let registry = Registry::builder()
            .with_tag("__test_registry_entry_id_second__")
            .build();
        let service = registry
            .services()
            .iter()
            .find(|service| service.name() == "test_registry_entry_id_second")
            .expect("test service should be selected by tag");
        let handle = service
            .instances()
            .first()
            .copied()
            .expect("auto-start service should have one instance handle");

        assert_eq!(handle.entry_id(), service.entry_id);
        assert!(std::ptr::eq(handle.entry(), service.entry));
        assert_eq!(handle.name(), "test_registry_entry_id_second");
        assert_eq!(handle.module(), "models::service::tests");
    }

    #[test]
    fn service_instance_registry_indexes_by_instance_and_entry() {
        let entry_id = ServiceCatalog::entry_id_for_wrapper(test_registry_entry_id_second_wrapper)
            .expect("wrapper should be indexed by global catalog");
        let record = ServiceCatalog::get(entry_id).expect("entry ID should resolve to record");
        let registry = ServiceInstanceRegistry::new();
        let first_handle = ServiceInstanceHandle::new(
            ServiceInstanceId::new(Uuid::from_u128(100)),
            entry_id,
            record.entry,
        );
        let second_handle = ServiceInstanceHandle::new(
            ServiceInstanceId::new(Uuid::from_u128(101)),
            entry_id,
            record.entry,
        );

        registry.insert(ServiceInstanceRecord::new(
            first_handle,
            CancellationToken::new(),
        ));
        registry.insert(ServiceInstanceRecord::new(
            second_handle,
            CancellationToken::new(),
        ));

        assert_eq!(registry.len(), 2);
        assert_eq!(
            registry
                .get(first_handle.instance_id())
                .map(|record| record.handle()),
            Some(first_handle)
        );
        assert_eq!(
            registry.handles_for_entry(entry_id),
            vec![first_handle, second_handle]
        );
    }

    #[test]
    fn service_description_instances_are_scoped_to_own_entry() {
        let first_entry_id =
            ServiceCatalog::entry_id_for_wrapper(test_registry_entry_id_first_wrapper)
                .expect("first wrapper should be indexed by global catalog");
        let second_entry_id =
            ServiceCatalog::entry_id_for_wrapper(test_registry_entry_id_second_wrapper)
                .expect("second wrapper should be indexed by global catalog");
        let first_record =
            ServiceCatalog::get(first_entry_id).expect("first entry ID should resolve to record");
        let second_record =
            ServiceCatalog::get(second_entry_id).expect("second entry ID should resolve to record");
        let instance_registry = Arc::new(ServiceInstanceRegistry::new());
        let first_handle = ServiceInstanceHandle::new(
            ServiceInstanceId::new(Uuid::from_u128(200)),
            first_entry_id,
            first_record.entry,
        );
        let second_handle = ServiceInstanceHandle::new(
            ServiceInstanceId::new(Uuid::from_u128(201)),
            first_entry_id,
            first_record.entry,
        );
        let other_entry_handle = ServiceInstanceHandle::new(
            ServiceInstanceId::new(Uuid::from_u128(202)),
            second_entry_id,
            second_record.entry,
        );
        instance_registry.insert(ServiceInstanceRecord::new(
            first_handle,
            CancellationToken::new(),
        ));
        instance_registry.insert(ServiceInstanceRecord::new(
            second_handle,
            CancellationToken::new(),
        ));
        instance_registry.insert(ServiceInstanceRecord::new(
            other_entry_handle,
            CancellationToken::new(),
        ));

        let service = ServiceDescription {
            entry_id: first_entry_id,
            entry: first_record.entry,
            instance_registry,
        };

        assert_eq!(service.instances(), vec![first_handle, second_handle]);
    }

    #[test]
    fn registry_build_records_auto_start_instance_handles() {
        let registry = Registry::builder()
            .with_tag("__test_registry_entry_id_second__")
            .build();
        let service = registry
            .services()
            .iter()
            .find(|service| service.name() == "test_registry_entry_id_second")
            .expect("test service should be selected by tag");
        let instance = service
            .instances()
            .first()
            .copied()
            .expect("auto-start service should have one instance handle");
        let record = registry
            .instance_registry
            .get(instance.instance_id())
            .expect("auto-start service should have an instance record");

        assert_eq!(registry.instance_registry.len(), registry.services().len());
        assert_eq!(record.handle(), instance);
        assert_eq!(record.entry_id(), service.entry_id);
        assert_eq!(record.name(), service.name());
        assert_eq!(
            record.instance_id().as_uuid().get_version_num(),
            7,
            "auto-start singleton service instances should receive UUIDv7 IDs"
        );
        assert_eq!(
            registry
                .instance_registry
                .handles_for_entry(service.entry_id),
            vec![instance]
        );
    }

    #[test]
    fn service_instance_id_display_and_parse_use_uuid_format() {
        let uuid = Uuid::parse_str("019fe746-6158-7403-82c9-ac72cad515ec").unwrap();
        let id = ServiceInstanceId::new(uuid);

        assert_eq!(
            id.to_string(),
            "svcinst#019fe746-6158-7403-82c9-ac72cad515ec"
        );
        assert_eq!(
            "svcinst#019fe746-6158-7403-82c9-ac72cad515ec"
                .parse::<ServiceInstanceId>()
                .unwrap(),
            id
        );
        assert_eq!(
            "019fe746-6158-7403-82c9-ac72cad515ec"
                .parse::<ServiceInstanceId>()
                .unwrap(),
            id
        );
        assert!(
            "1".parse::<ServiceInstanceId>().is_err(),
            "numeric instance IDs should no longer be accepted"
        );
    }

    #[test]
    fn registry_builds_allocate_distinct_uuidv7_instance_ids_for_same_entry() {
        let first_registry = Registry::builder()
            .with_tag("__test_registry_entry_id_second__")
            .build();
        let second_registry = Registry::builder()
            .with_tag("__test_registry_entry_id_second__")
            .build();
        let first_service = first_registry
            .services()
            .iter()
            .find(|service| service.name() == "test_registry_entry_id_second")
            .expect("test service should be selected by first registry");
        let second_service = second_registry
            .services()
            .iter()
            .find(|service| service.name() == "test_registry_entry_id_second")
            .expect("test service should be selected by second registry");

        assert_eq!(first_service.entry_id, second_service.entry_id);
        let first_instance = first_service
            .instances()
            .first()
            .copied()
            .expect("first auto-start service should have one instance handle");
        let second_instance = second_service
            .instances()
            .first()
            .copied()
            .expect("second auto-start service should have one instance handle");
        assert_eq!(first_instance.instance_id().as_uuid().get_version_num(), 7);
        assert_eq!(second_instance.instance_id().as_uuid().get_version_num(), 7);
        assert_ne!(
            first_instance.instance_id(),
            second_instance.instance_id(),
            "materializing the same registry entry twice should not reuse a runtime instance ID"
        );
    }
}

use futures::future::BoxFuture;
use linkme::distributed_slice;
use std::any::TypeId;
use std::fmt;
use tokio_util::sync::CancellationToken;
use tracing::warn;

pub type ServiceFn = fn(CancellationToken) -> BoxFuture<'static, anyhow::Result<()>>;

// ---------------------------------------------------------------------------
// ServiceId: Unique, ID-based identity for runtime indexing.
// Replaces String-based StatusPlane/Signal keys for safety and performance.
// ---------------------------------------------------------------------------

/// A unique identifier for a service instance within a `Registry`.
///
/// `ServiceId` is assigned by `Registry::build()` and serves as the **strong
/// identity** for all runtime resource lookups (StatusPlane, reload signals).
/// The human-readable `name` field on `ServiceDescription` is retained only
/// for logging / tracing purposes ("weak identity").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "file-logging", derive(serde::Serialize, serde::Deserialize))]
pub struct ServiceId(pub(crate) usize);

impl ServiceId {
    /// Explicitly construct a `ServiceId`.
    ///
    /// In production, IDs are assigned automatically by `Registry::build()`.
    /// This constructor exists for testing scenarios where ad-hoc services
    /// need to be created outside the Registry pipeline.
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

impl Default for ServiceId {
    /// Returns ServiceId(0), which is the default for system/background tasks.
    fn default() -> Self {
        Self(0)
    }
}

impl std::str::FromStr for ServiceId {
    type Err = std::num::ParseIntError;

    /// Parses a ServiceId from a string like "svc#1" or "1".
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let numeric_part = s.strip_prefix("svc#").unwrap_or(s);
        numeric_part.parse::<usize>().map(Self::new)
    }
}

impl fmt::Display for ServiceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "svc#{}", self.0)
    }
}

// ---------------------------------------------------------------------------
// InstanceId: Zero-allocation trigger instance identifier.
// Combines ServiceId + monotonic sequence for unique instance identification.
// ---------------------------------------------------------------------------

/// A unique identifier for a specific trigger invocation within a service.
///
/// Combines the owning service's [`ServiceId`] with a monotonically increasing
/// sequence number to produce a globally unique, human-readable instance tag.
///
/// # Performance
///
/// `InstanceId` is 16 bytes, stack-allocated, and implements `Copy`. It
/// replaces the previous `format!("{}:{}", service_id, seq)` pattern that
/// required a heap allocation on every trigger dispatch cycle.
///
/// # Display Format
///
/// Formats as `svc#N:SEQ` (e.g. `svc#1:42`), matching the legacy string
/// format for backward compatibility in log output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "file-logging", derive(serde::Serialize, serde::Deserialize))]
pub struct InstanceId {
    /// The service that owns this trigger instance.
    pub service_id: ServiceId,
    /// Monotonically increasing sequence within this service's lifetime.
    pub seq: u64,
}

impl InstanceId {
    /// Construct a new `InstanceId`.
    #[inline]
    pub const fn new(service_id: ServiceId, seq: u64) -> Self {
        Self { service_id, seq }
    }
}

impl fmt::Display for InstanceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.service_id, self.seq)
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
    pub watcher: Option<fn() -> BoxFuture<'static, ()>>,
    pub priority: u8,
    /// Compile-time tags assigned via `#[service(tags = ["core", "infra"])]`.
    /// Defaults to an empty slice when no tags are specified.
    pub tags: &'static [&'static str],
}

// ---------------------------------------------------------------------------
// ServiceDescription (runtime) -- now includes `ServiceId` and `tags`
// ---------------------------------------------------------------------------

/// Runtime description of a managed service instance.
///
/// Holds a reference to the underlying static `ServiceEntry` from the
/// `SERVICE_REGISTRY`, plus runtime-only state (`id`, `cancellation_token`)
/// and Arc-wrapped variants of the entry's function pointers.
///
/// Use accessor methods (`name()`, `priority()`, etc.) to read static
/// metadata without field duplication.
pub struct ServiceDescription {
    /// Unique ID assigned by `Registry::build()` -- the strong identity.
    pub id: ServiceId,
    /// Reference to the static entry that registered this service.
    pub entry: &'static ServiceEntry,
    /// Per-instance cancellation token for lifecycle management.
    pub cancellation_token: CancellationToken,
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

    /// Compile-time tags for filtering.
    #[inline]
    pub fn tags(&self) -> &'static [&'static str] {
        self.entry.tags
    }

    /// Dependency parameters with `TypeId` for graph analysis.
    #[inline]
    pub fn params(&self) -> &'static [ServiceParam] {
        self.entry.params
    }

    /// Module path where the service is defined.
    #[inline]
    pub fn module(&self) -> &'static str {
        self.entry.module
    }
}

// ---------------------------------------------------------------------------
// ServiceStatus (unchanged)
// ---------------------------------------------------------------------------

/// Represents the unified lifecycle status of a service.
///
/// This is the single source of truth for all service status, combining
/// both the external (daemon-observed) and internal (service-perceived) views.
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

/// The global provider registry -- providers register themselves here via `#[provider]` macro.
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
    /// Type-erased initializer that seeds the provider singleton.
    ///
    /// Implementations are macro-generated and are expected to call into the
    /// provider's `StateManager` to populate the snapshot cache.
    pub init: fn(
        crate::models::RestartPolicy,
        tokio_util::sync::CancellationToken,
    ) -> futures::future::BoxFuture<'static, ()>,
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

    /// Consume the registry and yield the service descriptions.
    pub(crate) fn into_services(self) -> Vec<ServiceDescription> {
        self.services
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
            let has_match = SERVICE_REGISTRY.iter().any(|e| e.tags.contains(tag));
            if !has_match {
                warn!(
                    "Registry::build() -- tag '{}' did not match any registered service",
                    tag
                );
            }
        }

        let mut services = Vec::new();

        for (idx, entry) in SERVICE_REGISTRY.iter().enumerate() {
            // --- Include filter ---
            if !self.include_tags.is_empty()
                && !entry.tags.iter().any(|t| self.include_tags.contains(t))
            {
                continue;
            }

            // --- Exclude filter ---
            if entry.tags.iter().any(|t| self.exclude_tags.contains(t)) {
                continue;
            }

            services.push(ServiceDescription {
                id: ServiceId(idx),
                entry,
                cancellation_token: CancellationToken::new(),
            });
        }

        Registry { services }
    }
}

#![deny(unsafe_code)]
//! A declarative Rust framework for automatic service management, event-driven triggers,
//! and type-based dependency injection.
//!
//! ## Quick Start
//!
//! ```rust,no_run
//! use service_daemon::prelude::*;
//! use std::sync::Arc;
//! use std::time::Duration;
//!
//! use service_daemon::{ServiceDaemon, provider, service, sleep};
//! use tracing::info;
//!
//! // 1. Define an injectable provider with a default value
//! #[derive(Clone)]
//! #[provider(8080)]
//! pub struct Port(pub i32);
//!
//! // 2. Define a managed service using proc-macros
//! #[service]
//! pub async fn heartbeat_service(port: Arc<Port>) -> anyhow::Result<()> {
//!     while !is_shutdown() {
//!         info!("Service is running on port {}", port);
//!         // Interruptible sleep: returns false if shutdown is requested
//!         if !sleep(Duration::from_secs(1)).await {
//!             break;
//!         }
//!     }
//!     Ok(())
//! }
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     // 3. Build and run the daemon
//!     let daemon = ServiceDaemon::builder().build();
//!     daemon.run().await;
//!     daemon.wait().await?;
//!     Ok(())
//! }
//! ```
//!
//! ## Documentation & Tutorials
//!
//! For the full guide and advanced patterns, visit our components on GitHub:
//!
//! - [**Quick Start Guide**](https://github.com/loft-games/service-daemon-rs/blob/master/docs/guide/tutorial/quick-start.md) - Complete step-by-step tutorial.
//! - [**Architecture Overview**](https://github.com/loft-games/service-daemon-rs/blob/master/docs/architecture/internal-overview.md) - DI and registry internals.

extern crate self as service_daemon;

mod core;
mod models;

// Re-export commonly used items
pub use core::context::{
    current_service_instance_id, done, is_shutdown, shelve, shelve_clone, sleep,
    spawn_with_context, state, trigger_config, unshelve, wait_shutdown,
};
pub use core::di::{
    ManagedProvided, Provided, ProviderDependencyChange, ProviderDependencyChangeReason,
    ProviderDependencyWatch, ProviderDependencyWatchSet, WatchableProvided,
};
pub use core::ipc::IpcStream;
pub use core::logging::{
    DaemonLayer, LogBatchSizeError, MAX_LOG_BATCH_SIZE, init_logging, set_log_batch_size,
    try_init_logging,
};
pub use core::managed_state::{Mutex, RwLock, TrackedNotify, TrackedSender};
pub use core::service_daemon::{
    DaemonInstanceHandle, RestartPolicy, RestartPolicyBuilder, ServiceDaemon, ServiceDaemonBuilder,
};
pub use models::service::{ServicePriority, ServiceScheduling, TriggerInstanceId};
pub use models::trigger::TriggerTransition;
pub use models::{
    BackoffController, DaemonDiagnosticsSnapshot, DaemonInstanceId, DaemonRuntimeSnapshot,
    DiagnosticAggregateStats, DiagnosticConfidence, DiagnosticGenerationExitKind,
    DiagnosticHighPriorityPlacementDecision, DiagnosticHighPriorityPlacementDecisionKind,
    DiagnosticHighPriorityPlacementReason, DiagnosticInterpretation, DiagnosticInterpretationLabel,
    DiagnosticLifecycleStats, DiagnosticObservationStats, DiagnosticProviderFailure,
    DiagnosticProviderFailureBoundaryKind, DiagnosticProviderFailureKind,
    DiagnosticProviderFailureRetry, DiagnosticProviderFailureRuntimePhase,
    DiagnosticProviderFailureSourceKind, DiagnosticProviderFailureStats,
    DiagnosticRecommendationHint, DiagnosticRestartDecisionKind, DiagnosticRuntimeLane,
    DiagnosticShutdownBoundaryKind, DiagnosticShutdownBoundaryOutcome,
    DiagnosticShutdownBoundaryResultKind, DiagnosticShutdownBoundaryStats,
    DiagnosticShutdownResidualActionKind, GenerationDiagnosticsSnapshot,
    HighPriorityRuntimeShardSnapshot, HighPriorityShardDiagnosticsSnapshot, HighPriorityShardId,
    HighPriorityShardPressureState, ProviderError, ProviderInitError, ReadinessServiceError,
    ReadinessSnapshot, Registry, RegistryBuilder, Result, RuntimeLaneDiagnosticsSnapshot,
    ScalingPolicy, ScalingPolicyBuilder, ScalingPolicyError, SchedulingAdvisoryProfile,
    ServiceDiagnosticsSnapshot, ServiceEntryId, ServiceError, ServiceHandle,
    ServiceInputDescriptor, ServiceInstanceHandle, ServiceInstanceId, ServiceInvocationContext,
    ServiceRuntimeSnapshot, ServiceStatus, TT, TriggerContext, TriggerHandler, TriggerHost,
    TriggerMessage, TriggerPolicyOverlay, TriggerPolicyOverlayBuilder, TriggerPolicyOverlayError,
    TriggerPressureSnapshot, TriggerRuntimeSnapshot,
};

// Re-export simulation utilities (feature-gated toolbox)
#[cfg(feature = "simulation")]
pub use core::context::{MockContext, MockContextBuilder, SimulationHandle};

// Conditionally re-export file logging utilities
#[cfg(feature = "file-logging")]
pub use core::logging::{FileLogConfig, RotationPolicy, enable_file_logging};

// Re-export diagnostics API (Behavioral Topology)
#[cfg(feature = "diagnostics")]
pub use core::topology_collector::{export_mermaid, reset_topology, start_topology_collector};

#[doc(hidden)]
pub mod __private {
    pub use std::sync::Arc;

    pub use crate::ProviderDependencyWatchSet;
    pub use crate::core::context::{__resolve_service_handle, current_cancellation_token};
    pub use crate::core::managed_state::{
        StateManager, TrackedMutex as Mutex, TrackedNotify, TrackedRwLock as RwLock, TrackedSender,
    };
    pub use crate::core::provider_init::{
        ProviderInitBoundaryContext, ProviderInitBoundaryKind, ProviderInitFailure,
        ProviderInitSourceKind, ProviderRuntimePhase, catch_init_panic, init_fallible,
        init_fallible_with_source, provider_init_boundary, provider_init_failure_boundary,
        provider_init_failure_into_error, with_provider_runtime_phase,
    };
    pub use crate::core::provider_scope::{
        ProviderCacheScope, provider_changed, provider_dependency_watch, resolve_provider_managed,
        resolve_provider_managed_with_scope, resolve_provider_mutex,
        resolve_provider_mutex_with_scope, resolve_provider_rwlock,
        resolve_provider_rwlock_with_scope, resolve_provider_snapshot,
        resolve_provider_snapshot_with_scope,
    };
    pub use crate::models::trigger::trigger_clone_payload;
    pub use crate::models::{
        PROVIDER_REGISTRY, ProviderEntry, SERVICE_REGISTRY, ServiceEntry, ServiceEntryId,
        ServiceFn, ServiceHandle, ServiceInputDescriptor, ServiceInstanceHandle, ServiceInstanceId,
        ServiceInvocationContext, ServiceParam,
    };

    pub use futures;
    pub use linkme;
    pub use tokio;
    #[cfg(feature = "cron")]
    pub use tokio_cron_scheduler;
    pub use tokio_util;
    pub use uuid;
}

// Re-export macros for unified user experience
pub use service_daemon_macro::{provider, service, service_handle, trigger};

/// A prelude module for commonly used items and trigger templates.
///
/// Importing this allows using short variant names like `Cron` or `Watch` and
/// provides IDE autocompletion for `#[trigger]` attributes.
pub mod prelude {
    pub use crate::TT::*;
    pub use crate::{
        DaemonDiagnosticsSnapshot, DaemonInstanceHandle, DaemonInstanceId, DaemonRuntimeSnapshot,
        DiagnosticHighPriorityPlacementDecision, DiagnosticHighPriorityPlacementDecisionKind,
        DiagnosticHighPriorityPlacementReason, DiagnosticRuntimeLane,
        HighPriorityRuntimeShardSnapshot, HighPriorityShardDiagnosticsSnapshot,
        HighPriorityShardId, HighPriorityShardPressureState, IpcStream, ManagedProvided, Provided,
        ReadinessSnapshot, SchedulingAdvisoryProfile, ServiceDaemon, ServiceError, ServiceHandle,
        ServiceInputDescriptor, ServiceInstanceHandle, ServicePriority, ServiceRuntimeSnapshot,
        ServiceScheduling, ServiceStatus, TT, TriggerPolicyOverlay, TriggerPolicyOverlayError,
        TriggerPressureSnapshot, TriggerRuntimeSnapshot, WatchableProvided,
        current_service_instance_id, done, is_shutdown, provider, service, service_handle, shelve,
        shelve_clone, sleep, spawn_with_context, state, trigger, trigger_config, unshelve,
        wait_shutdown,
    };
}

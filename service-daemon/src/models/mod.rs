pub mod diagnostics;
pub mod error;
pub mod policy;
pub mod provider_error;
pub mod runtime;
pub mod service;
pub mod trigger;

pub use diagnostics::{
    DaemonDiagnosticsSnapshot, DiagnosticAggregateStats, DiagnosticConfidence,
    DiagnosticGenerationExitKind, DiagnosticInterpretation, DiagnosticInterpretationLabel,
    DiagnosticLifecycleStats, DiagnosticObservationStats, DiagnosticProviderFailure,
    DiagnosticProviderFailureBoundaryKind, DiagnosticProviderFailureKind,
    DiagnosticProviderFailureRetry, DiagnosticProviderFailureRuntimePhase,
    DiagnosticProviderFailureSourceKind, DiagnosticProviderFailureStats,
    DiagnosticRecommendationHint, DiagnosticRestartDecisionKind, DiagnosticRuntimeLane,
    DiagnosticShutdownBoundaryKind, DiagnosticShutdownBoundaryOutcome,
    DiagnosticShutdownBoundaryResultKind, DiagnosticShutdownBoundaryStats,
    DiagnosticShutdownResidualActionKind, GenerationDiagnosticsSnapshot,
    RuntimeLaneDiagnosticsSnapshot, ServiceDiagnosticsSnapshot,
};
#[cfg(feature = "high-priority")]
pub use diagnostics::{
    DiagnosticHighPriorityPlacementDecision, DiagnosticHighPriorityPlacementDecisionKind,
    DiagnosticHighPriorityPlacementReason, HighPriorityShardDiagnosticsSnapshot,
};
pub use error::{ProviderInitError, Result, ServiceError};
#[cfg(feature = "high-priority")]
pub use policy::SchedulingAdvisoryProfile;
pub use policy::{
    BackoffController, RestartPolicy, ScalingPolicy, ScalingPolicyBuilder, ScalingPolicyError,
    TriggerPolicyOverlay, TriggerPolicyOverlayBuilder, TriggerPolicyOverlayError,
};
pub use provider_error::ProviderError;
pub use runtime::{
    DaemonInstanceId, DaemonRuntimeSnapshot, ReadinessServiceError, ReadinessSnapshot,
    ServiceRuntimeSnapshot, TriggerPressureSnapshot, TriggerRuntimeSnapshot,
};
#[cfg(feature = "high-priority")]
pub use runtime::{
    HighPriorityRuntimeShardSnapshot, HighPriorityShardId, HighPriorityShardPressureState,
};
pub use service::{
    PROVIDER_REGISTRY, ProviderEntry, Registry, RegistryBuilder, SERVICE_REGISTRY,
    ServiceDescription, ServiceEntry, ServiceEntryId, ServiceFn, ServiceHandle,
    ServiceInputDescriptor, ServiceInstanceHandle, ServiceInstanceId, ServiceInvocationContext,
    ServiceParam, ServiceScheduling, ServiceStatus,
};
pub(crate) use service::{
    ServiceCatalog, ServiceCatalogProjection, ServiceControl, ServiceInputPayload,
    ServiceInstanceRecord, ServiceInstanceRegistry,
};
pub use trigger::{TT, TriggerContext, TriggerHandler, TriggerHost, TriggerMessage};

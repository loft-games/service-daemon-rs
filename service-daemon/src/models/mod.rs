pub mod diagnostics;
pub mod error;
pub mod policy;
pub mod provider_error;
pub mod runtime;
pub mod service;
pub mod trigger;

pub use diagnostics::{
    DaemonDiagnosticsSnapshot, DiagnosticAggregateStats, DiagnosticConfidence,
    DiagnosticGenerationExitKind, DiagnosticHighPriorityPlacementDecision,
    DiagnosticHighPriorityPlacementDecisionKind, DiagnosticHighPriorityPlacementReason,
    DiagnosticInterpretation, DiagnosticInterpretationLabel, DiagnosticLifecycleStats,
    DiagnosticObservationStats, DiagnosticProviderFailure, DiagnosticProviderFailureBoundaryKind,
    DiagnosticProviderFailureKind, DiagnosticProviderFailureRetry,
    DiagnosticProviderFailureRuntimePhase, DiagnosticProviderFailureSourceKind,
    DiagnosticProviderFailureStats, DiagnosticRecommendationHint, DiagnosticRestartDecisionKind,
    DiagnosticRuntimeLane, DiagnosticShutdownBoundaryKind, DiagnosticShutdownBoundaryOutcome,
    DiagnosticShutdownBoundaryResultKind, DiagnosticShutdownBoundaryStats,
    DiagnosticShutdownResidualActionKind, GenerationDiagnosticsSnapshot,
    HighPriorityShardDiagnosticsSnapshot, RuntimeLaneDiagnosticsSnapshot,
    ServiceDiagnosticsSnapshot,
};
pub use error::{ProviderInitError, Result, ServiceError};
pub use policy::{
    BackoffController, RestartPolicy, ScalingPolicy, ScalingPolicyBuilder, ScalingPolicyError,
    SchedulingAdvisoryProfile, TriggerPolicyOverlay, TriggerPolicyOverlayBuilder,
    TriggerPolicyOverlayError,
};
pub use provider_error::ProviderError;
pub use runtime::{
    DaemonInstanceId, DaemonRuntimeSnapshot, HighPriorityRuntimeShardSnapshot, HighPriorityShardId,
    HighPriorityShardPressureState, ReadinessServiceError, ReadinessSnapshot,
    ServiceRuntimeSnapshot, TriggerPressureSnapshot, TriggerRuntimeSnapshot,
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

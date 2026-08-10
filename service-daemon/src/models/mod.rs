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
pub use error::{ProviderInitError, Result, ServiceError};
pub use policy::{
    BackoffController, RestartPolicy, ScalingPolicy, ScalingPolicyBuilder, ScalingPolicyError,
    SchedulingAdvisoryProfile, TriggerPolicyOverlay, TriggerPolicyOverlayBuilder,
    TriggerPolicyOverlayError,
};
pub use provider_error::ProviderError;
pub use runtime::{
    DaemonRuntimeSnapshot, ReadinessServiceError, ReadinessSnapshot, ServiceRuntimeSnapshot,
    TriggerPressureSnapshot, TriggerRuntimeSnapshot,
};
pub use service::{
    PROVIDER_REGISTRY, ProviderEntry, Registry, RegistryBuilder, SERVICE_REGISTRY,
    ServiceDescription, ServiceEntry, ServiceEntryId, ServiceFn, ServiceHandle,
    ServiceInstanceHandle, ServiceInstanceId, ServiceParam, ServiceScheduling, ServiceStatus,
};
pub(crate) use service::{ServiceCatalog, ServiceCatalogProjection};
pub use trigger::{TT, TriggerContext, TriggerHandler, TriggerHost, TriggerMessage};

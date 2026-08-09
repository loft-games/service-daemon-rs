use std::fmt;

use crate::models::service::ServiceInstanceId;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TriggerDispatchFailureKind {
    HandlerRetryExhausted,
    DispatchTaskError,
    DispatchTaskPanic,
    DispatchTaskCancelled,
    DispatchPermitAcquireFailed,
    DispatchTimedOut,
    ScaleMonitorFailed,
}

impl TriggerDispatchFailureKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::HandlerRetryExhausted => "handler_retry_exhausted",
            Self::DispatchTaskError => "dispatch_task_error",
            Self::DispatchTaskPanic => "dispatch_task_panic",
            Self::DispatchTaskCancelled => "dispatch_task_cancelled",
            Self::DispatchPermitAcquireFailed => "dispatch_permit_acquire_failed",
            Self::DispatchTimedOut => "dispatch_timed_out",
            Self::ScaleMonitorFailed => "scale_monitor_failed",
        }
    }
}

impl fmt::Display for TriggerDispatchFailureKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug)]
pub(crate) struct TriggerDispatchFailure {
    kind: TriggerDispatchFailureKind,
    trigger_name: &'static str,
    service_instance_id: ServiceInstanceId,
    instance_seq: Option<u64>,
    message_id: Option<Uuid>,
    reason: String,
}

impl TriggerDispatchFailure {
    pub(crate) fn new(
        kind: TriggerDispatchFailureKind,
        trigger_name: &'static str,
        service_instance_id: ServiceInstanceId,
        instance_seq: Option<u64>,
        message_id: Option<Uuid>,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            trigger_name,
            service_instance_id,
            instance_seq,
            message_id,
            reason: reason.into(),
        }
    }

    pub(crate) fn kind(&self) -> TriggerDispatchFailureKind {
        self.kind
    }

    pub(crate) fn trigger_name(&self) -> &'static str {
        self.trigger_name
    }

    pub(crate) fn service_instance_id(&self) -> ServiceInstanceId {
        self.service_instance_id
    }

    pub(crate) fn instance_seq(&self) -> Option<u64> {
        self.instance_seq
    }

    pub(crate) fn message_id(&self) -> Option<Uuid> {
        self.message_id
    }
}

impl fmt::Display for TriggerDispatchFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Trigger '{}' dispatch failed with {}: {}",
            self.trigger_name, self.kind, self.reason
        )?;
        if let Some(instance_seq) = self.instance_seq {
            write!(f, " (instance_seq={instance_seq}")?;
            if let Some(message_id) = self.message_id {
                write!(f, ", message_id={message_id}")?;
            }
            write!(f, ")")?;
        }
        Ok(())
    }
}

impl std::error::Error for TriggerDispatchFailure {}

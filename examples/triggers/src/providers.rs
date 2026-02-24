//! Trigger event source providers.
//!
//! This module defines the **event sources** that triggers subscribe to.
//! These are independent of business services and demonstrate
//! the decoupled nature of the trigger system.
//!
//! ## Queue Types
//! - `BroadcastQueue` (aliases: `Queue`, `BQueue`): All handlers receive every message (fanout).
//! - `LoadBalancingQueue` (alias: `LBQueue`): Messages are distributed to one handler at a time.

use service_daemon::provider;

// =============================================================================
// Signal Provider
// =============================================================================

/// A `Notify`-based signal. Calling `UserNotifier::notify()` wakes all
/// subscribed `Event`/`Notify` triggers.
#[provider(default = Notify)]
pub struct UserNotifier;

// =============================================================================
// Cron Schedule Provider
// =============================================================================

/// A cron schedule string. Triggers annotated with
/// `#[trigger(template = Cron, target = CleanupSchedule)]`
/// will fire according to this schedule.
#[derive(Clone)]
#[provider(default = "*/30 * * * * *")]
pub struct CleanupSchedule(pub String);

// =============================================================================
// Broadcast Queue Provider
// =============================================================================

/// A broadcast queue: every subscribed handler receives every message.
#[provider(default = Queue, item_type = "String")]
pub struct TaskQueue;

// =============================================================================
// Load-Balancing Queue Provider
// =============================================================================

/// A load-balancing queue: each message is delivered to exactly one handler.
#[provider(default = LBQueue, item_type = "String")]
pub struct WorkerQueue;

// =============================================================================
// Complex Payload Queue
// =============================================================================

/// A structured payload for demonstrating typed queue payloads.
#[derive(Debug, Clone)]
pub struct ComplexJob {
    pub id: u64,
    pub data: String,
}

/// A load-balancing queue that carries `ComplexJob` payloads.
#[provider(default = LBQueue, item_type = "ComplexJob")]
pub struct JobQueue;

// =============================================================================
// Watchable State Provider
// =============================================================================

/// A state value that can be watched for changes.
///
/// When any code acquires `ExternalStatus::rwlock()` and modifies the value,
/// all `Watch` triggers targeting `ExternalStatus` will fire automatically.
#[derive(Debug, Clone)]
#[provider]
pub struct ExternalStatus {
    pub message: String,
    pub updated_count: u32,
}

impl Default for ExternalStatus {
    fn default() -> Self {
        Self {
            message: "Initial state".to_owned(),
            updated_count: 0,
        }
    }
}

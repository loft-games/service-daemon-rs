//! Trigger event source providers.
//!
//! This module defines the **event sources** that triggers subscribe to.
//! These are independent of business services and demonstrate
//! the decoupled nature of the trigger system.
//!
//! ## Queue Types
//! - `BroadcastQueue` (aliases: `Queue`, `BQueue`): All handlers receive every message (fanout).

use service_daemon::provider;

// =============================================================================
// Signal Provider
// =============================================================================

/// A `Notify`-based signal. Calling `notifier.notify()` on a resolved instance
/// wakes all subscribed `Event`/`Notify`/`Signal` triggers, demonstrating one-to-many fanout.
///
/// Template providers like this are injectable.
///
/// In the current capability model, all `#[provider(...)]` forms are also watchable
/// by default. The `Watch(T)` semantics are uniformly defined as: notify when a
/// new managed-state snapshot is published.
#[provider(Notify)]
pub struct UserNotifier;

// =============================================================================
// Cron Schedule Provider
// =============================================================================

/// A cron schedule string. Triggers annotated with
/// `#[trigger(Cron(CleanupSchedule))]`
/// will fire according to this schedule.
#[derive(Clone)]
#[provider("*/30 * * * * *")]
pub struct CleanupSchedule(pub String);

// =============================================================================
// Broadcast Queue Provider
// =============================================================================

/// A broadcast queue: every subscribed handler receives every message.
#[provider(Queue(String))]
pub struct TaskQueue;

// =============================================================================
// Worker Queue Provider
// =============================================================================

/// A worker queue: messages are broadcast to all subscribed handlers.
#[provider(Queue(String))]
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

/// A broadcast queue that carries `ComplexJob` payloads.
#[provider(Queue(ComplexJob))]
pub struct JobQueue;

// =============================================================================
// Watchable State Provider
// =============================================================================

/// A state value that can be watched for changes.
///
/// When any code acquires a `RwLock` via the resolved instance and modifies the value,
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

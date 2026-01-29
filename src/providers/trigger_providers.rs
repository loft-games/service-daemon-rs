//! Trigger event source providers using Type-Based DI
//!
//! These wrapper types enable compile-time verification of trigger dependencies.
//! Using templates, the boilerplate is handled automatically by the macro.
//!
//! ## Queue Types
//! - `BroadcastQueue` (aliases: `Queue`, `BQueue`): All handlers receive every message (fanout).
//! - `LoadBalancingQueue` (alias: `LBQueue`): Messages are distributed to one handler at a time.

use service_daemon::{allow_sync, provider};

// Signal provider - generates Arc<Notify> with static `notify()` and `wait()` methods
// Aliases: Notify, Event
#[provider(default = Notify)]
pub struct UserNotifier;

// Cron schedule provider - simple string wrapper
#[derive(Clone)]
#[provider(default = "*/30 * * * * *")]
pub struct CleanupSchedule(pub String);

// Broadcast Queue - all handlers receive every message (fanout)
// Aliases: BroadcastQueue, Queue, BQueue
#[provider(default = Queue, item_type = "String")]
pub struct TaskQueue;

// Load-Balancing Queue - messages are distributed to one handler at a time
// Alias: LoadBalancingQueue, LBQueue
#[provider(default = LBQueue, item_type = "String")]
pub struct WorkerQueue;
// --- Complex Payload Example ---
#[derive(Debug, Clone)]
pub struct ComplexJob {
    pub id: u64,
    pub data: String,
}

#[provider(default = LBQueue, item_type = "ComplexJob")]
pub struct JobQueue;

// --- Async Function Provider Example ---
// This struct is initialized via an async function below.
#[derive(Clone)]
#[allow(dead_code)]
pub struct AsyncConfig {
    pub connection_string: String,
    pub initialized_at: std::time::Instant,
}

// The async fn replaces the default initialization
#[allow(dead_code)]
#[provider]
pub async fn async_config() -> AsyncConfig {
    // Simulate async initialization (e.g., fetching config from remote)
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    AsyncConfig {
        connection_string: "postgres://localhost/db".to_owned(),
        initialized_at: std::time::Instant::now(),
    }
}

// --- Sync Function Provider Example ---
#[derive(Clone)]
#[allow(dead_code)]
pub struct SyncConfig {
    pub value: String,
}

#[allow(dead_code)]
#[allow_sync]
#[provider]
pub fn sync_config() -> SyncConfig {
    SyncConfig {
        value: "sync-init-value".to_owned(),
    }
}

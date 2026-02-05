/// Template types for event-driven triggers.
///
/// This enum provides a structured way to specify trigger hosts in the `#[trigger]` macro,
/// replacing loose strings with typed variants for better IDE support.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerTemplate {
    // --- Signal-based (uses signal_trigger_host) ---
    /// Generic notification/event signal.
    Notify,
    /// Alias for Notify.
    Event,
    /// Alias for Notify.
    Signal,
    /// Custom signal-based trigger.
    Custom,

    // --- Fanout Queues (uses queue_trigger_host) ---
    /// Broadcast queue where all handlers receive every message.
    Queue,
    /// Alias for Queue (Broadcast Queue).
    BQueue,
    /// Alias for Queue.
    BroadcastQueue,

    // --- Load-Balancing Queues (uses lb_queue_trigger_host) ---
    /// Load-balanced queue where messages are distributed to a single handler.
    LBQueue,
    /// Alias for LBQueue.
    LoadBalancingQueue,

    // --- Scheduled (uses cron_trigger_host) ---
    /// Cron-based scheduled trigger.
    Cron,

    // --- State-driven (uses watch_trigger_host) ---
    /// Fires when a specific state provider is modified.
    Watch,
    /// Alias for Watch.
    State,
}

/// Short alias for TriggerTemplate.
pub use TriggerTemplate as TT;

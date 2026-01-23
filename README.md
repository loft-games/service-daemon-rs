# Service Daemon

A Rust library for automatic service management with dependency injection, inspired by Python's decorator-based service registration.

## Features

- **`#[service]`** - Mark functions (sync or async) as managed services
- **`#[trigger]`** - Event-driven functions (sync or async; templates: Cron, Queue, Event)
- **`#[provider]`** - Auto-register dependencies, supports sync/async function initialization
- **Automatic restart** - Failed services are restarted automatically
- **Type-safe DI** - Services/Triggers receive dependencies by name with type verification
- **Zero boilerplate** - Just annotate and run

> [!CAUTION]
> **Performance Warning: Synchronous Functions**
> While synchronous functions are supported for convenience, they run directly on the asynchronous executor's worker threads. **Blocking synchronous code will stall the entire daemon.**
> - For I/O or long-running tasks, always prefer `async fn`.
> - If you must use blocking code, consider wrapping it in `tokio::task::spawn_blocking` internally or converting the service to an `async fn`.

> [!TIP]
> **Suppressing Sync Warnings with `#[allow_sync]`**
> If your synchronous function is intentionally non-blocking (e.g., fast in-memory operations), you can suppress the runtime warning by adding `#[allow_sync]` before your `#[service]`, `#[trigger]`, or `#[provider]` macro:
> ```rust
> use service_daemon::{allow_sync, service};
>
> #[allow_sync]
> #[service]
> pub fn fast_sync_service() -> anyhow::Result<()> {
>     // This function is intentionally sync and safe.
>     Ok(())
> }
> ```

## Quick Start

### 1. Add dependencies

```toml
[dependencies]
service-daemon = { path = "service-daemon" }
tokio = { version = "1.40", features = ["full"] }
anyhow = "1.0"
tracing = "0.1"
tracing-subscriber = "0.3"
```

### 2. Create providers

```rust
// src/providers/typed_providers.rs
use service_daemon::provider;

#[provider(default = 8080)]
pub struct Port(pub i32);

#[provider(default = "mysql://localhost")]  // Auto-expands to .to_owned()
pub struct DbUrl(pub String);

// --- Environment Variable Provider ---
// Reads DATABASE_URL from environment, falls back to default if not set
#[provider(env_name = "DATABASE_URL", default = "postgres://localhost")]
pub struct DatabaseUrl(pub String);

// --- Async Function Provider (custom initialization) ---
pub struct AsyncConfig {
    pub connection_string: String,
}

#[provider]
pub async fn async_config() -> AsyncConfig {
    // Custom async initialization (e.g., fetching from remote)
    AsyncConfig { connection_string: "postgres://localhost".to_owned() }
}

// --- Synchronous Function Provider ---
#[provider]
pub fn sync_config() -> String {
    "some-static-value".to_owned()
}
```



### 3. Create services

```rust
// src/services/example.rs
use service_daemon::service;
use crate::providers::typed_providers::{Port, DbUrl};
use std::sync::Arc;

#[service]
pub async fn my_service(port: Arc<Port>, db_url: Arc<DbUrl>) -> anyhow::Result<()> {
    tracing::info!("Running on port {} with DB {}", **port, **db_url);
    loop {
        // do work
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
    }
}

// Synchronous services are also supported!
#[service]
pub fn my_sync_service(port: Arc<Port>) -> anyhow::Result<()> {
    tracing::info!("Sync service running on port {}", **port);
    Ok(())
}
```

### 4. Run the daemon

```rust
// src/main.rs
mod providers;
mod services;

use service_daemon::ServiceDaemon;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    
    // Registers all services (providers are resolved lazily via OnceLock)
    let daemon = ServiceDaemon::auto_init();
    daemon.run().await
}
```

## How It Works

1. **`#[provider]`** implements the `Provided` trait for a struct or function, using `OnceCell` for thread-safe asynchronous singleton resolution.
2. **`#[service]`** generates an async wrapper that calls `T::resolve().await` for each `Arc<T>` dependency.
3. **`#[trigger]`** registers a specialized service with an embedded async event loop (Cron, Queue, or Event).
4. **`ServiceDaemon::auto_init()`** discovers all services (including triggers) via `linkme`.
5. **`daemon.run()`** spawns all services/triggers and restarts them on failure with **exponential backoff**.

## Resilience Features

### Exponential Backoff & Restart Policy

Services that fail are automatically restarted with exponential backoff to prevent rapid crash loops:

```rust
use service_daemon::{ServiceDaemon, RestartPolicy};
use std::time::Duration;

// Custom restart policy with builder pattern
let policy = RestartPolicy::builder()
    .initial_delay(Duration::from_secs(2))
    .max_delay(Duration::from_secs(300))  // 5 minutes max
    .multiplier(1.5)                       // Backoff multiplier
    .reset_after(Duration::from_secs(60)) // Reset delay after stable run
    .build();

let daemon = ServiceDaemon::from_registry_with_policy(policy);
daemon.run().await?
```

Default policy: 1s initial → 2x multiplier → 5min max.

### Graceful Shutdown

The daemon handles `SIGINT` (Ctrl+C) and `SIGTERM` signals for graceful shutdown:

```rust
// Press Ctrl+C or send SIGTERM to stop gracefully
daemon.run().await?
// After receiving signal:
// INFO: Received SIGINT, shutting down...
// INFO: Stopping service: my_service
// INFO: ServiceDaemon stopped.
```

### Service Status API

Monitor service health at runtime:

```rust
use service_daemon::ServiceStatus;

let daemon = ServiceDaemon::auto_init();
// ... after spawning services ...

// Query status (Running, Restarting, or Stopped)
let status = daemon.get_service_status("my_service").await;
match status {
    ServiceStatus::Running => println!("Service is healthy"),
    ServiceStatus::Restarting => println!("Service is recovering"),
    ServiceStatus::Stopped => println!("Service has stopped"),
}
```

```mermaid
sequenceDiagram
    participant Main
    participant SERVICE_REGISTRY
    participant ServiceDaemon
    participant ServiceWrapper
    participant Provider

    Main->>ServiceDaemon: auto_init()
    ServiceDaemon->>SERVICE_REGISTRY: iterate services & triggers
    Main->>ServiceDaemon: run()
    ServiceDaemon->>ServiceWrapper: spawn()
    ServiceWrapper->>Provider: T::resolve().await
    Note over Provider: Singleton (OnceCell)
    Provider-->>ServiceWrapper: Arc<T>
    ServiceWrapper->>ServiceWrapper: run async function
```

## Compile-Time Dependency Verification

With Type-Based DI, missing dependencies are caught at **compile-time**:
```text
error[E0599]: no function or associated item named `resolve` found for struct `MissingType`
```

## Triggers

Triggers are specialized services with built-in event loops. They register normally as services but manage an internal "Call Host".

```mermaid
sequenceDiagram
    participant Main
    participant SERVICE_REGISTRY
    participant ServiceDaemon
    participant TriggerWrapper
    participant Provider
    participant EventSource

    Main->>ServiceDaemon: auto_init()
    ServiceDaemon->>SERVICE_REGISTRY: iterate services & triggers
    Main->>ServiceDaemon: run()
    ServiceDaemon->>TriggerWrapper: spawn()
    TriggerWrapper->>Provider: T::resolve().await
    Note over Provider: Singleton (OnceCell)
    Provider-->>TriggerWrapper: Arc<T>
    TriggerWrapper->>EventSource: subscribe() / wait()
    loop Event Loop
        EventSource-->>TriggerWrapper: fired / item received
        TriggerWrapper->>TriggerWrapper: run user function
    end
```

### 1. Cron Trigger

Executes a function based on a cron expression string.

```rust
#[provider(default = "*/30 * * * * *")]
pub struct CleanupSchedule(pub String);

#[trigger(template = "cron", target = CleanupSchedule)]
async fn hourly_cleanup(_request: (), id: String) -> anyhow::Result<()> {
    tracing::info!("Cleaning up... (id: {})", id);
    Ok(())
}
```

### 2. Broadcast Queue Trigger (Fanout)

All handlers receive every message pushed to a `BroadcastQueue`.

```rust
// BroadcastQueue aliases: Queue, BQueue
#[provider(default = Queue, item_type = "MyTask")]
pub struct TaskQueue;

// Multiple triggers can subscribe - all receive every message!
#[trigger(template = "queue", target = TaskQueue)]
async fn handler1(item: MyTask, id: String) -> anyhow::Result<()> { ... }

#[trigger(template = "queue", target = TaskQueue)]
async fn handler2(item: MyTask, id: String) -> anyhow::Result<()> { ... }

// Push to the queue (async)
async fn trigger_handlers() {
    let _ = TaskQueue::push(MyTask { ... }).await;
}
```

### 3. Load-Balancing Queue Trigger

Messages are distributed to one handler at a time with `LoadBalancingQueue`.

```rust
// LoadBalancingQueue alias: LBQueue
#[provider(default = LBQueue, item_type = "Task")]
pub struct WorkerQueue;

#[trigger(template = "lb_queue", target = WorkerQueue)]
async fn worker(item: Task, id: String) -> anyhow::Result<()> { ... }

// Push to the queue (async)
async fn add_work() {
    let _ = WorkerQueue::push(Task { ... }).await;
}
```


### 4. Signal Trigger (Event)

Executes a function when a `tokio::sync::Notify` is triggered.

```rust
// Provider aliases: Notify, Event
#[provider(default = Notify)]
pub struct EventNotifier;

// Trigger template aliases: custom, notify, event
#[trigger(template = "event", target = EventNotifier)]
async fn on_notification(_request: (), id: String) -> anyhow::Result<()> {
    tracing::info!("Event received! (id: {})", id);
    Ok(())
}

// Trigger the signal from anywhere (async):
async fn unlock() {
    EventNotifier::notify().await;
}
```




## Project Structure

```
service-daemon-rs/
├── service-daemon/           # Core library
│   └── src/
│       ├── lib.rs            # Re-exports macros and core types
│       ├── models/           # Service, Provider, Trigger registry
│       └── utils/            # DI Container, ServiceDaemon
├── service-daemon-macro/     # Procedural macros
│   └── src/lib.rs            # #[service], #[provider], #[trigger]
└── src/                      # Example application
    ├── main.rs
    ├── providers/            # Your providers go here
    ├── services/             # Your services go here
    └── triggers/             # Your triggers go here (optional)
```

## License

MIT

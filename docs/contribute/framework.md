# Service Daemon Architecture

This document describes the internal architecture of the `service-daemon-rs` framework, explaining how its components interact to provide automatic service management and type-based dependency injection.

## Project Overview

The `service-daemon-rs` is a high-level framework for building resilient, modular Rust applications. It automates the boilerplate associated with:
1. **Service Orchestration**: Managing the lifecycle of long-running tasks.
2. **Dependency Injection (DI)**: Automatically resolving and injecting dependencies based on types.
3. **Event Triggers**: Decoupling event sources from service logic.

## High-Level Architecture

The framework is built around a **unified service registry** and **decentralized dependency injection**.

- **Unified Registry**: Both standard services and event-driven triggers are collected into a single `SERVICE_REGISTRY` at link time.
- **Decentralized DI**: Unlike traditional DI containers, there is no central registry for providers. Instead, each type provides its own resolution logic via the `Provided` trait, typically as a lazy `OnceCell` singleton.

```mermaid
graph TD
    subgraph "User Code"
        S["#[service] Function"]
        P["#[provider] Struct/Fn"]
        T["#[trigger] Function"]
    end

    subgraph "Macros (service-daemon-macro)"
        M_S[Service Wrapper]
        M_P[Provided Trait Impl]
        M_T[Trigger Wrapper]
    end

    subgraph "Static Registry (linkme)"
        SR[(SERVICE_REGISTRY)]
        MR[(MUTABILITY_REGISTRY)]
    end

    subgraph "Core (service-daemon)"
        SD[ServiceDaemon]
        CT[CancellationToken]
        TH[TriggerHosts]
    end

    subgraph "Intelligent State"
        SM[StateManager]
        P_Impl["Provided Trait Impls"]
    end

    S --> M_S
    P --> M_P
    T --> M_T

    M_S --> SR
    M_T --> SR
    
    %% Mutability detection
    S -.->|mut mark| MR
    T -.->|mut mark| MR

    SD -->|load| SR
    SD -->|control| CT
    
    SD -->|spawn| S
    SD -->|spawn| TH
    TH -->|loop| T
    
    CT -.->|signal| S
    CT -.->|signal| TH
    
    S -.->|resolve| SM
    T -.->|resolve| SM
    SM -.->|check| MR
    SM -.->|instantiate| P_Impl
```

## Step-by-Step Technical Details

### 1. Registration Phase (Compile & Link Time)

The framework uses the `linkme` crate to perform "distributed registration". 

- **Macros**: When you annotate a function with `#[service]`, the macro generates a static entry and a wrapper function.
- **Linker**: During the linking phase of compilation, all these static entries across different modules (and even different crates in the workspace) are collected into a single contiguous slice: `SERVICE_REGISTRY`.

### 2. Initialization Phase (`auto_init`)

When `ServiceDaemon::auto_init()` is called:
1. It iterates through the `SERVICE_REGISTRY`.
2. For each entry, it registers the service into the `ServiceDaemon` instance.
3. It initializes the `CancellationToken` for graceful shutdown management.

### 3. Dependency Injection (Decentralized & Lazy)

The `service-daemon-rs` uses **Type-Based Decentralized Resolution**. There is no "Container" object that holds all instances.

- **The `Provided` Trait**: Each type that can be injected must implement the `Provided` trait. The `#[provider]` macro automates this.
- **Async Singletons**: Each `Provided::resolve()` implementation typically uses a `tokio::sync::OnceCell` to ensure that only one instance of the type is created (Singleton pattern) and shared via `Arc<T>`.
- **Recursive Resolution**: When a service starts, its macro-generated wrapper calls `Provided::resolve()`. If that provider has its own `Arc<T>` fields, it recursively calls `resolve()` for those types.
- **No Manual Mapping**: Dependency resolution happens entirely based on types at compile time.

### 4. Execution Phase (`run`)

Once started via `daemon.run().await`:
1. **Spawning**: Each service is spawned as a separate `tokio` task.
2. **Monitoring**: The `ServiceDaemon` tracks the `JoinHandle` and status (Running, Restarting, Stopped) of each service. It also automatically wraps each service execution in a `tracing::Span` named `service` with the service's name, enabling automatic log correlation.
3. **Restart Policy**: If a service fails (returns `Err`), the daemon applies an **Exponential Backoff** policy with jitter to prevent "thundering herd" issues.
4. **Graceful Shutdown**: Upon receiving a `SIGINT` (Ctrl+C) or `SIGTERM`:
    - The `CancellationToken` is cancelled.
    - All services are awaited.
    - A grace period (e.g., 30s) is enforced before forcing an abort.

### 5. Event Triggers (Specialized Services)

Triggers are not a separate primitive; they are **Specialized Services**.
- **Unified Registry**: The `#[trigger]` macro registers an entry directly into the `SERVICE_REGISTRY`.
- **Host Wrapper**: Instead of running user code directly, the trigger wrapper spawns a "Host" (e.g., `cron_trigger_host`). 
- **Inversion of Control**: The Host manages the event source (cron, queue, etc.) and executes the user's handler when the event occurs.
- **Watch Trigger**: A special trigger type that subscribes to `StateManager` notifications. It fires whenever a `TrackedRwLock` or `TrackedMutex` guard is dropped for the target type.
- **Declarative Parameter Detection**: The `#[trigger]` macro categorizes parameters into three groups:
    - **DI Resources**: Parameters of type `Arc<T>` (not marked with `#[payload]`). These are resolved via `T::resolve().await` **inside the event loop** on every firing, ensuring triggers always have the latest promoted state snapshots.
    - **Event Payloads**: Either the first non-`Arc<T>` parameter or any parameter explicitly marked with `#[payload]`.
    - **Cancellation Token**: Parameters of type `CancellationToken` are automatically injected to allow cooperative trigger shutdown.
- **Attribute Stripping**: To ensure valid Rust code after transformation, the macro strips the internal `#[payload]` attribute.
- **Trace correlation**: It automatically injects the trigger name and a unique ID into the `tracing` context via a span.

### 6. Intelligent State Management & Promotion

The framework implements a "Hybrid State" pattern that optimizes for the common case (read-only) while supporting transparent mutation.

- **MutabilityMark**: The `#[service]` and `#[trigger]` macros analyze their parameters. If they detect `Arc<RwLock<T>>` or `Arc<Mutex<T>>`, they emit a `MutabilityMark` for type `T` into the `MUTABILITY_REGISTRY` using `linkme`.
- **StateManager**: A specialized state container that holds a simple `OnceCell` for the "Fast Path" and a `OnceCell<Arc<TrackedShared<T>>>` for the "Managed Path". It also maintains a `Notify` instance to broadcast change notifications to `Watch` triggers.
- **Intelligent Switching**:
    - **The Fast Path (Immutable)**: If `T` has no mutability marks, `Provided::resolve()` returns a simple immutable singleton. Performance is equivalent to a raw pointer.
    - **The Managed Path (Mutable)**: If even one `MutabilityMark` exists for `T` anywhere in the binary, `Provided::resolve()` switches to the `StateManager`'s managed path. It reads the current value from the `RwLock` and returns a consistent `Arc<T>` snapshot.
- **Atomic Publishing & Watch**: Every time a service requests `Arc<RwLock<T>>`, it receives a `TrackedRwLock<T>`. When the write guard is dropped, it atomically updates the shared snapshot and triggers the `StateManager`'s notification.
- **The Macro Illusion**: The `#[service]` and `#[trigger]` macros inject local `use` aliases for `RwLock` and `Mutex`. This directs user code (which uses standard `tokio` names) to use our tracked versions transparently, enabling the notification logic without changing a single line of business logic.

---

## Key Components

| Component | Responsibility |
| :--- | :--- |
| `ServiceDaemon` | Main orchestrator, manages task lifecycles and restarts. |
| `SERVICE_REGISTRY` | Global list of all services found at link-time. |
| `Provided` | Trait that enables a type to be injected. |
| `RestartPolicy` | Configures backoff timing and jitter. |
| `CancellationToken` | Orchestrates graceful coordination for shutdown. |

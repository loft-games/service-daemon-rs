# Lifecycle Management & Status Plane

The `ServiceDaemon` uses a sophisticated orchestration system to manage service generations, crashes, and reloads.

## 1. Unified Status Plane

All services share a central `GLOBAL_STATUS_PLANE` (`DashMap<String, ServiceStatus>`).

| Status | Transitions to | Triggered by |
|--------|----------------|--------------|
| `Initializing` | `Healthy` | `done()` or implicit handshake |
| `Restoring` | `Healthy` | Successful warm start or implicit handshake |
| `Recovering(err)`| `Healthy` | Custom recovery logic + `done()` or implicit handshake |
| `Healthy` | `NeedReload` | Dependency mutation detected |
| `NeedReload` | `Terminated` | Service cleanup + exit |
| `ShuttingDown` | `Terminated` | Daemon shutdown signal |

> [!NOTE]
> **Immediate Reloads**: If a service is in a restart backoff delay (due to a failure) and a `NeedReload` signal is received (e.g. from an upstream dependency change), the `ServiceDaemon` will wake the service immediately, bypassing the remaining delay to ensure the system reaches a healthy state as quickly as possible.

## 2. Wave-Based Orchestration

Services are started and stopped synchronized by waves of `priority`.

- **Startup (High to Low)**: Core services start first. A wave waits until all services in it report `Healthy` (via a handshake) before starting the next wave.
- **Shutdown (Low to High)**: External APIs stop first, followed by storage and then core systems.

## 3. The Handshake Protocol

A service indicates it is "ready" via a handshake. This prevents dependent services from starting before their prerequisites are fully initialized.

### Explicit Handshake
Calling `service_daemon::done()` manually. Recommended for complex initialization.

### Implicit Handshake
For minimalist services, any call to `is_shutdown()`, `sleep()`, or `wait_shutdown()` counts as a transition to `Healthy` if the service is still in an introductory phase (`Initializing`, `Restoring`, `Recovering`).

> [!TIP]
> **Performance Optimization**: The implicit handshake is internally optimized using a task-local flag. Only the first call to these functions per service generation will interact with the central Status Plane. Subsequent calls are near-zero overhead atomic checks.

## 4. State Persistence (The Shelf)

The "Shelf" is a global store where services can deposit data before a reload or after a crash.
- **Isolation**: Buckets are isolated by service name.
- **Survival**: Unlike standard singletons, Shelf data survives the task termination and is inherited by the next "generation" of the same service.

[Back to README](../../README.md)

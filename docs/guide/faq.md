# Concept Clarification & Pitfalls (FAQ)

This page explains common behaviors that are easy to misread when first using the framework.

---

## Table of Contents
1. [Registry & Discovery](#registry-discovery)
2. [Lifecycle & Paradigms](#lifecycle-paradigms)
3. [Providers & State](#providers-state)
4. [Testing & Simulation](#testing-simulation)

---

## 1. Registry & Discovery

### The "Registered Service" Trap (Linkme)
**Problem**: You annotated a function with `#[service]`, but it doesn't start.
**Cause**: Rust's linker-based discovery (`linkme`) only finds code that is **explicitly included in the compilation tree**.
**The Fix**: Ensure the module containing your service is reachable from `main.rs` via `mod my_module;`.

### Service/Trigger Discovery vs. Manual Calls
**Misconception**: "I should call my trigger functions manually to test them."
**Reality**: Triggers and services are managed by the `ServiceDaemon`. While you *can* call them, they are designed to be driven by the framework's event loops.

---

## 2. Lifecycle & Paradigms

### Choosing Your Control Flow
**Core Rule**: Choose **exactly one** paradigm per service. Mixing them leads to race conditions.

| Paradigm | Control Flow | Use Case |
| :--- | :--- | :--- |
| **Polling** | `while !is_shutdown() { ... }` | Simple loops (e.g., heartbeats). |
| **Reactive** | `while let Some(s) = state().match(...)` | Complex state machines, reloads. |

> [!WARNING]
> Do **NOT** use `is_shutdown()` inside a `state().match()` loop. The `state()` stream handles shutdown automatically.

### Why did my `#[service]` parameter get treated like a payload?
**Problem**: A `#[service]` function with a bare parameter like `data: String` or `port: i32` fails with a macro error.
**Cause**: Services do not support payload parameters. Every parameter must be a framework-managed dependency (`Arc<T>`, `Arc<RwLock<T>>`, or `Arc<Mutex<T>>`). The macro system uses a shared validation path for services and triggers, and bare parameters are currently rejected at the same point where trigger payloads are validated.
**The Fix**: Wrap dependencies in `Arc<T>`. If you meant to handle an event payload, use `#[trigger]` instead.

---

## 3. Providers & State

### Providers vs. Shelf: Which to use?
*   **Providers (Shared Objects)**: Use for managed shared objects that multiple services need to access (e.g., DB Pools, shared configuration). Injected via `Arc<T>`.
*   **Shelf (Local Persistence)**: Use for data that belongs uniquely to one service and must survive service reloads or crashes (e.g., a current operation ID, a retry counter). This is a private, ephemeral key-value store.

> [!NOTE]
> Neither of these is a permanent database. Both are cleared when the entire process stops. For persistence across process restarts, use a real database (injected via a Provider).

### The built-in template misconception
**Problem**: Trying to modify the macro system to add a new default provider type such as MQTT.
**Solution**: Use the `#[provider]` attribute on an `async fn`. Built-in templates are for low-level primitives such as signaling, queues, and socket listeners. See the [Provider Strategy Guide](provider-best-practices.md).

---

## 4. Testing & Simulation

### Simulation is NOT a separate engine
**Reality**: `MockContext` still runs the real `ServiceDaemon` and real service/trigger code. It swaps the daemon-owned resources under that run: isolated shelf/status state plus optional daemon-local provider overrides.

### Avoid root provider pollution in tests
**Problem**: A test calls `T::resolve()` or mutates a root provider and assumes a later daemon will see that exact test value.
**The Fix**: Prefer `MockContext::builder().with_provider_override(...)` before startup or `SimulationHandle::override_provider(...)` during a run. Provider overrides are scoped to one simulation daemon and reload dependent generations through the normal provider watch path.

### Registry Isolation 
**Problem**: Integrated services in one test interfere with another test.
**The Fix**: Use **Tags** and a filtered `Registry` for your tests. See [Testing & Troubleshooting](testing-troubleshooting.md#registry-isolation-in-tests) for implementation details.

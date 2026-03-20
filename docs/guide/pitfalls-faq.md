# Concept Clarification & Pitfalls (FAQ)

This central hub explains the architectural "why" behind common behaviors and traps. Use this guide when things don't work as expected or when you're unsure which design pattern to choose.

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

### State vs. Shelf: Which to use?
*   **State (Provider)**: Global, shared, often permanent (e.g., DB Pools).
*   **Shelf**: Service-local, persistent across restarts (e.g., retry counters).

### The "Magic Provider" Misconception
**Problem**: Trying to modify the macro system to add a new "default" type (like MQTT).
**Solution**: Don't! Use the `#[provider]` attribute on an `async fn`. Magic Providers are for low-level architecture primitives only. See the [Provider Best Practices Guide](provider-best-practices.md).

---

## 4. Testing & Simulation

### Simulation is NOT a separate engine
**Reality**: `MockContext` doesn't change how your code runs; it just provides a "test-local floor" for `resolve()` and `state()` calls. Your production code remains 100% the same.

### Registry Isolation 
**Problem**: Integrated services in one test interfere with another test.
**The Fix**: Use **Tags** and a filtered `Registry` for your tests. See [Testing & Troubleshooting](testing-troubleshooting.md#registry-isolation-in-tests) for implementation details.


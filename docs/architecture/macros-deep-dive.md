# Macros Deep Dive: The Engine of Service-Daemon

This document explains how the procedural macros transform your code and how the "Macro Illusion" maintains IDE compatibility.

## 1. The `#[service]` Transformation

When you annotate a function, the macro generates:
1. **Logic Preservation**: The original function code remains mostly intact.
2. **Wrapper Generation**: An `async move` block that resolves all dependencies before calling the original function.
3. **Registry Entry**: A `static` entry collected by `linkme`.

> [!IMPORTANT]
> **Distributed Registration Requirement**: Because `linkme` works at the linker level, any module containing a `#[service]` or `#[trigger]` **must** be included in your compilation tree (e.g., via `mod my_module;`). If a module is not reachable from `main.rs`, its services will not be discovered.

## 2. The `#[trigger]` Transformation

Triggers are specialized services. The macro generates a **Host Wrapper** that:
- Spawns the appropriate "Host" logic (e.g., `cron_trigger_host`).
- **Service-Level Integration (`Watch`)**: For `Watch` templates, the macro generates a service watcher that leverages the `ServiceDaemon`'s reload mechanism, allowing triggers to be reactive with minimal internal logic.
- Manages the inversion of control: the host executes the user handler when events occur.

## 3. The "Macro Illusion"

One of the most powerful features is how the framework handles shared state without breaking your IDE experience.

### Transparent Tracking
The macros perform a "replacement" of standard types:
- `Arc<RwLock<T>>` is transparently redirected to a tracked version that reports changes to `Watch` triggers.
- **Span Preservation**: By using `quote_spanned!`, the macro attaches the original source code's "span" to the generated code.
- **Intellisense Friendly**: Because of span preservation, `rust-analyzer` still sees your original types, allowing "Jump to Definition" and documentation hints to work perfectly.

### Qualified Path Support
The macros are robust enough to handle various import styles:
- `std::sync::Arc<T>`
- `Arc<T>`
- `tokio::sync::RwLock<T>`

## 4. Promotion Logic
- **Fast Path**: If only `Arc<T>` is used, it stays an immutable singleton with zero locking overhead.
- **Managed Path**: If *any* service in the entire registry requests a lock (`RwLock`/`Mutex`), the provider is automatically promoted at link-time to support atomic CoW (Copy-on-Write) publishing.

[Back to README](../../README.md)

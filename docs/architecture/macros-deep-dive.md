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
- Spawns the appropriate "Host" logic (e.g., `Notify_trigger_host`).
- **DI Resolution**: Dependency providers are resolved **once** at trigger startup (outside the event loop), matching standard service behavior. This ensures consistent lifecycle management and prevents redundant resolutions on every event.
- **Service-Level Integration (`Watch`)**: For `Watch` templates, the macro generates a service watcher that leverages the `ServiceDaemon`'s reload mechanism.
- **Event Dispatch**: The host executes the user handler when events occur, managing the inversion of control.

## 3. The "Macro Illusion"

One of the most powerful features is how the framework handles shared state without breaking your IDE experience.

### Transparent Tracking
The macros perform a "replacement" of standard types:
- `Arc<RwLock<T>>` is transparently redirected to a tracked version that reports changes to `Watch` triggers.
- **Span Preservation**: By using `quote_spanned!`, the macro attaches the original source code's "span" to the generated code.
- **Intellisense Friendly**: Because of span preservation, `rust-analyzer` still sees your original types, allowing "Jump to Definition" and documentation hints to work reliably.

### Qualified Path Support
The macros are robust enough to handle various import styles:
- `std::sync::Arc<T>`
- `Arc<T>`
- `tokio::sync::RwLock<T>`

## 4. Promotion Logic
- **Fast Path**: If only `Arc<T>` is used, it stays an immutable singleton with zero locking overhead.
- **Managed Path**: If *any* service in the entire registry requests a lock (`RwLock`/`Mutex`), the provider is automatically promoted at link-time to support atomic CoW (Copy-on-Write) publishing.

## 5. Shared Macro Infrastructure (`common.rs`)

To ensure consistency between `#[service]` and `#[trigger]`, shared code is consolidated in `common.rs`:
- **`ParamProcessor`**: A unified state machine for parsing function inputs and identifying DI dependencies (`Arc<T>`, `Arc<RwLock<T>>`).
- **`generate_call_expr`**: A shared generator for calling user functions, handling async/sync differences and warning injection.
- **`generate_watcher`**: A unified generator for the service/trigger reload watcher.

## 6. The `#[allow(sync_handler)]` Pseudo-Lint

### Background

When a synchronous (non-`async`) function is used with `#[service]`, `#[trigger]`, or `#[provider]`, the macro generates a `tracing::warn!` call at runtime. To suppress this warning, users annotate their function with `#[allow(sync_handler)]`.

`sync_handler` is **not** a real compiler lint. It is a pseudo-lint that the framework's proc macro intercepts and strips from the attribute list before the compiler ever sees it.

### Implementation: `extract_sync_handler_flag`

Located in `common.rs`, this function:
1. Scans the item's attribute list for any `#[allow(...)]` containing `sync_handler`.
2. If found, strips `sync_handler` from the `allow` list (preserving other lints like `dead_code`).
3. If `sync_handler` was the only entry, removes the entire `#[allow(...)]` attribute.
4. Returns `(true, cleaned_attrs)` so the macro knows to skip the `tracing::warn!` generation.

### Attribute Ordering: No Constraint Required

**Finding (2026-02)**: Despite common assumptions about Rust attribute macro visibility, `#[allow(sync_handler)]` works correctly regardless of whether it is placed above or below the `#[service]` macro.

This was verified empirically with three tests:

| Configuration | `tracing::warn!` in expanded code | Runtime WARN log |
| :--- | :--- | :--- |
| No `#[allow(sync_handler)]` | Present | Yes |
| `#[allow(sync_handler)]` below `#[service]` | Absent | No |
| `#[allow(sync_handler)]` above `#[service]` | Absent | No |

Verification methods used:
- **`cargo expand`**: Confirmed the presence/absence of `tracing::warn!` in generated code.
- **Runtime execution**: Ran the binary and inspected logs for each configuration.

**Why both orders work**: This behavior is rooted in the Rust compiler's execution pipeline and the nature of **Inert Attributes**:

1.  **Built-in vs. Custom**: `#[allow(...)]` is a built-in attribute recognized by the compiler's core. Unlike custom proc-macro attributes, it doesn't require discovery.
2.  **Inertia**: In Rust, built-in attributes (like `allow`, `cfg`, `derive`) are considered "inert." When the compiler calls an attribute macro like `#[service]`, it includes *all* inert attributes attached to the item in the `item` `TokenStream`, regardless of whether they appear above or below the proc-macro attribute.
3.  **Lint Check Timing**: The compiler's **Lint Checker** (which flags `unknown_lints`) runs much later in the pipeline than **Macro Expansion**. 

**The Execution Flow**:
1.  **Expansion Phase**: The compiler sees `#[allow(sync_handler)] #[service]`.
2.  **Macro Call**: It calls the `service` macro, passing the function and the `allow` attribute in the `item` stream.
3.  **Stripping**: Our `extract_sync_handler_flag` function intercepts the `TokenStream`, identifies `sync_handler`, and physically removes it.
4.  **Re-emission**: The macro returns a "clean" `TokenStream` to the compiler.
5.  **Lint Phase**: When the Lint Checker finally runs, the `sync_handler` string has already been "deleted" from the source. Since it's gone, no "unknown lint" warning is ever triggered.

> [!NOTE]
> **For maintainers**: This relies on `#[allow]` being an inert built-in attribute and the expansion-before-linting order. If a future Rust edition changes these rules (e.g., if `allow` becomes a proc-macro itself or if linting moves earlier), the ordering might become sensitive. Maintaining the "macro first, permit second" order is still recommended for maximum robustness.

[Back to README](../../README.md)

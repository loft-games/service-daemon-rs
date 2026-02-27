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

## 5. The `#[allow(sync_handler)]` Pseudo-Lint

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

**Why both orders work**: `#[allow(...)]` is a built-in (inert) attribute. When the compiler prepares the token stream for an attribute macro like `#[service]`, built-in attributes on the same item are included in the `item` `TokenStream` — regardless of whether they appear above or below the proc macro attribute. This differs from the behavior between two proc macro attributes, where only attributes below are visible.

> [!NOTE]
> **For maintainers**: If this behavior changes in a future Rust edition, the `extract_sync_handler_flag` function and this section should be revisited. The safest place for `#[allow(sync_handler)]` is below the macro attribute, as that is guaranteed by the proc macro specification.

[Back to README](../../README.md)

---
name: sd-macro-development
description: "[dev] Work on the service-daemon-macro proc-macro crate. Use when modifying or reviewing #[service]/#[trigger]/#[provider] codegen, understanding the linkme registry slices the macros emit, inspecting expanded output, or adding compile-time macro tests."
---

# Developing the `service-daemon-macro` crate

The three attribute macros are the framework's front door: they turn a plain
function/struct into a registered, dependency-injected service, trigger, or
provider. Understanding them means understanding **two halves** — the *parse +
codegen* in `service-daemon-macro/`, and the *registry slices* in `service-daemon/`
that the generated code writes into at link time.

## Entry points (`service-daemon-macro/src/lib.rs`)

Each `#[proc_macro_attribute]` is a thin delegator. The crate is
`#![forbid(unsafe_code)]`; user-facing errors flow through the local diagnostics
facade, `syn::Result` control flow, and generated `compile_error!` tokens rather
than a third-party diagnostics shim.

| Attribute | Entry fn | Delegates to |
| :--- | :--- | :--- |
| `#[service]` | `service` (lib.rs:63) | `service::service_impl(attr, item)` |
| `#[provider]` | `provider` (lib.rs:137) | `provider::provider_impl(attr, item)` |
| `#[trigger]` | `trigger` (lib.rs:192) | `trigger::trigger_impl(attr, item)` |

## Parse / codegen split

Each macro module separates *parsing the input* from *emitting tokens*:

| Module | Files |
| :--- | :--- |
| `service/` | `mod.rs` (orchestration), `codegen.rs` (wrapper + entry emission). |
| `provider/` | `mod.rs`, `parser.rs` (attribute parsing), `templates.rs` (special provider types like `Queue`/`Notify`), `struct_gen.rs` (DI impls). |
| `trigger/` | `mod.rs`, `parser.rs` (attributes), `codegen.rs` (host selection + wrapper). |
| `common/` | shared helpers across the three. |

Change parsing in `parser.rs`; change emitted code in `codegen.rs` /
`struct_gen.rs` / `templates.rs`. Keep the two concerns apart.

## What the macros emit: the linkme registries

Generated code registers itself into two `linkme` distributed slices defined in
`service-daemon/src/models/service.rs` — no `build.rs`, no runtime scanning:

```rust
#[allow(unsafe_code)]            // linkme expands to #[link_section], which
#[distributed_slice]            // edition 2024 treats as unsafe
pub static SERVICE_REGISTRY: [ServiceEntry];   // service.rs:304

#[allow(unsafe_code)]
#[distributed_slice]
pub static PROVIDER_REGISTRY: [ProviderEntry]; // service.rs:314
```

`#[service]` / `#[trigger]` emit a `ServiceEntry`; `#[provider]` emits a
`ProviderEntry`. Their fields are the contract between codegen and the runtime.

## Companions

- `reference.md` — the exact `ServiceEntry` / `ProviderEntry` field contracts,
  the inspect-and-test workflow (`cargo expand`, trybuild), and the relevant docs.
- `pitfalls.md` — the traps (changing an entry struct without updating codegen,
  forgetting `#[allow(unsafe_code)]`, stale trybuild `.stderr`).

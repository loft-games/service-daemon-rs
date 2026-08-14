# Macro development reference

## 1. The registry entry contracts (`service-daemon/src/models/service.rs`)

Generated code constructs these; the runtime reads them. Changing a field means
updating **both** the struct and every macro that emits it.

`ServiceEntry` — emitted by `#[service]` and `#[trigger]`:

```rust
pub struct ServiceEntry {
    pub name: &'static str,
    pub module: &'static str,
    pub params: &'static [ServiceParam],
    pub input: Option<ServiceInputDescriptor>,
    pub wrapper: fn(ServiceInvocationContext) -> BoxFuture<'static, anyhow::Result<()>>,
    pub watcher: Option<fn() -> ProviderDependencyWatchSet>,
    pub priority: u8,
    pub scheduling: ServiceScheduling,
    pub tags: &'static [&'static str],
}
```

`input: None` means the selected service auto-starts one instance during daemon
startup. `input: Some(ServiceInputDescriptor { ... })` marks a service template:
the registry selects the definition, but callers must create runtime instances via
`ServiceHandle::create(input)` or `ServiceHandle::start(input)`.
The runtime validates the supplied type against the descriptor before registering
the instance; non-template services accept only unit input for manual dynamic
creation.

`ProviderEntry` — emitted by `#[provider]`:

```rust
pub struct ProviderEntry {
    pub name: &'static str,
    pub module: &'static str,
    pub type_id: TypeId,
    pub params: &'static [ServiceParam],
    pub eager: bool,
    pub init: fn(
        RestartPolicy,
        CancellationToken,
    ) -> BoxFuture<'static, Result<(), ProviderInitError>>,
}
```

Unlike `ServiceEntry`, providers do not carry a service wrapper or priority. The
entry owns provider metadata for graph analysis (`type_id`, Provider->Provider
edges), the eager-init flag, and the type-erased initializer used by startup
preflight/provider scoping.

Both slices are re-exported for consumers at `service-daemon/src/lib.rs:117` and
`service-daemon/src/models/mod.rs:22` (`SERVICE_REGISTRY`, `PROVIDER_REGISTRY`,
`ServiceEntry`, `ProviderEntry`, `ServiceFn`, `ServiceParam`).

### Linkme platform contract monitoring

The runtime depends on these `linkme` distributed slices surviving the link step.
Keep the release-validation map aligned with the CI smoke coverage:

| Platform family | Expected coverage |
| :--- | :--- |
| Linux GNU | Positive release-mode `linkme_smoke` plus registry section/symbol inspection in `rust.yml`. |
| Linux musl | Positive release-mode `linkme_smoke` plus registry section/symbol inspection in `rust.yml`. |
| Windows GNU | Best-effort watchdog with XFAIL without workaround plus positive with workaround. |
| Windows MSVC | Positive release-mode `linkme_smoke` plus registry section/symbol inspection in `rust.yml`. |
| macOS host target | Positive release-mode `linkme_smoke` plus registry section/symbol inspection in `rust.yml`. |

Do not mirror every Rust target triple; prefer OS/linker/object-format families.

## 2. Inspecting generated code

```bash
cargo expand -p example-complete   # see the wrappers + distributed_slice entries
```

`example-complete` exercises services, triggers, and providers together, so its
expansion is the reference point for reading what the macros produce. Expand after any
codegen change to confirm the output is what you intended.

## 3. Compile-time macro tests (`examples/macro-tests`, trybuild)

Macro behavior is verified with **trybuild** (`trybuild = "1.0.116"`):

- `tests/macro_compile_tests.rs` runs two cases:
  - `t.pass(...)` — code that must compile.
  - `t.compile_fail("tests/fail/*.rs")` — code that must be rejected.
- `tests/fail/NN_*.rs` — one fixture per rejected pattern (e.g.
  `02_service_rejects_payload.rs`, `11_only_provided_cannot_inject_managed.rs`),
  each paired with a `.stderr` snapshot of the expected diagnostic.

When you add or change a compile error, add/update the matching `tests/fail/`
fixture and refresh its `.stderr` (trybuild can regenerate it; review the diff).
Run with `cargo test -p example-macro-tests`.

## 4. Where macro behavior is documented

- `docs/architecture/macro-expansion.md` — what `#[service]`/`#[trigger]`/
  `#[provider]` generate (the authoritative internal explanation).
- `docs/development/extending-framework.md` — maintainer guidance for extending
  the framework, including the macro seams.

## 5. Error reporting

The crate owns diagnostics through `service-daemon-macro/src/diagnostics.rs`,
`syn::Result` control flow, and generated `compile_error!` tokens. Emit
user-facing diagnostics with spanned errors so the message points at the
offending token, not the whole item — the `tests/fail/*.stderr` snapshots assert
on these.

Rust stable does not expose proc-macro warnings. The local facade keeps
provider-template warning call sites classified, but the stable backend is a
no-op that preserves the previous behavior.

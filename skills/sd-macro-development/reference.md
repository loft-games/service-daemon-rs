# Macro development reference

## 1. The registry entry contracts (`service-daemon/src/models/service.rs`)

Generated code constructs these; the runtime reads them. Changing a field means
updating **both** the struct and every macro that emits it.

`ServiceEntry` (service.rs:210) — emitted by `#[service]` and `#[trigger]`:

```rust
pub struct ServiceEntry {
    pub name: &'static str,
    pub module: &'static str,
    pub params: &'static [ServiceParam],
    pub wrapper: fn(CancellationToken) -> BoxFuture<'static, anyhow::Result<()>>,
    pub watcher: Option<fn() -> ProviderDependencyWatchSet>,
    pub priority: u8,
  pub scheduling: ServiceScheduling,
    pub tags: &'static [&'static str],
}
```

`ProviderEntry` (service.rs:326) — emitted by `#[provider]`. Unlike `ServiceEntry`
it carries **no** wrapper or priority; its job is dependency metadata for graph
analysis (Provider→Provider edges, cycle detection) plus the `eager` opt-in. Fields
begin with `name` and `module` (the defining module path).

Both slices are re-exported for consumers at `service-daemon/src/lib.rs:117` and
`service-daemon/src/models/mod.rs:22` (`SERVICE_REGISTRY`, `PROVIDER_REGISTRY`,
`ServiceEntry`, `ProviderEntry`, `ServiceFn`, `ServiceParam`).

## 2. Inspecting generated code

```bash
cargo expand -p example-complete   # see the wrappers + distributed_slice entries
```

`example-complete` exercises services, triggers, and providers together, so its
expansion is the canonical place to read what the macros produce. Expand after any
codegen change to confirm the output is what you intended.

## 3. Compile-time macro tests (`examples/macro-tests`, trybuild)

Macro behavior is verified with **trybuild** (`trybuild = "1.0.116"`):

- `tests/macro_compile_tests.rs` runs two cases:
  - `t.pass(...)` — code that must compile.
  - `t.compile_fail("tests/fail/*.rs")` — code that must be rejected.
- `tests/fail/NN_*.rs` — one fixture per rejected pattern (e.g.
  `02_service_rejects_payload.rs`, `11_only_provided_cannot_inject_managed.rs`),
  each paired with a `.stderr` snapshot of the expected diagnostic.

When you add or change a compile error, add/Update the matching `tests/fail/`
fixture and refresh its `.stderr` (trybuild can regenerate it; review the diff).
Run with `cargo test -p example-macro-tests`.

## 4. Where macro behavior is documented

- `docs/architecture/macro-expansion.md` — what `#[service]`/`#[trigger]`/
  `#[provider]` generate (the authoritative internal explanation).
- `docs/development/extending-framework.md` — maintainer guidance for extending
  the framework, including the macro seams.

## 5. Error reporting

The crate uses `proc_macro_error2` (`#[proc_macro_error]` on each entry). Emit
user-facing diagnostics with spanned errors so the message points at the offending
token, not the whole item — the `tests/fail/*.stderr` snapshots assert on these.

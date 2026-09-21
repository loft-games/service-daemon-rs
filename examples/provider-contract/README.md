# Provider Contract Example

This example shows the cross-crate provider topology:

- `shared/` owns `SharedSettings`, marks it with `#[provider_contract]`, and
  defines the service that injects `Arc<SharedSettings>`.
- `app/src/providers.rs` owns prioritized `#[provider_impl]` functions inside
  the application library target. The final binary and integration test link
  that library with `use example_provider_contract as _;`. The primary candidate
  returns `ProviderError::Unavailable`, so the runtime selects the fallback.

Run the daemon:

```bash
cargo run -p example-provider-contract
```

Run the integration test that resolves the shared contract through candidates
compiled into the application library target:

```bash
cargo test -p example-provider-contract
```

When implementations live in a library target that the final binary or test does
not otherwise use, add an explicit linkage import in each final target:

```rust
use implementation_crate as _;
```

This links the crate that owns the registrations while keeping candidate
functions private.

Use this pattern when a library crate must define the injectable type and its
consuming services, while the final binary owns deployment-specific construction.
Candidates are ordered by descending priority; equal priorities use module path
and function name for a stable tie-break. `Unavailable` and retry timeout advance
to the next candidate, while fatal failure and cancellation stop resolution.

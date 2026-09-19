# Provider Contract Example

This example shows the cross-crate provider topology:

- `shared/` owns `SharedSettings`, marks it with `#[provider_contract]`, and
  defines the service that injects `Arc<SharedSettings>`.
- `app/` owns prioritized `#[provider_impl]` functions. Its primary candidate
  returns `ProviderError::Unavailable`, so the runtime selects the fallback.

Run the daemon:

```bash
cargo run -p example-provider-contract
```

Run the integration test that starts the shared service through the app-local
candidate registry:

```bash
cargo test -p example-provider-contract
```

Use this pattern when a library crate must define the injectable type and its
consuming services, while the final binary owns deployment-specific construction.
Candidates are ordered by descending priority; equal priorities use module path
and function name for a stable tie-break. `Unavailable` and retry timeout advance
to the next candidate, while fatal failure and cancellation stop resolution.

# Release Validation

Maintainers use this map to check release and CI coverage without expanding the
README. It ties feature flags to tests, examples, platform smoke jobs, and
dependency baselines.

## Feature Validation Matrix

| Feature | Role | CI / test coverage | Example coverage |
| :--- | :--- | :--- | :--- |
| `cron` | Default feature; production trigger host support. | `cargo check --workspace`, `cargo test --workspace`, `cargo clippy --workspace`; all-features jobs also include it. | `example-triggers`, `example-complete`, `example-macro-tests`. |
| `simulation` | Test-only sandbox utilities. | `cargo check -p example-simulation`, `cargo test --workspace --all-features`, `cargo clippy --workspace --all-features`. | `example-simulation`. |
| `diagnostics` | Diagnostic topology and runtime observation support. | `cargo check -p example-diagnostics`, `cargo test --workspace --all-features`, diagnostics unit coverage for shutdown topology tracing. | `example-diagnostics`. |
| `file-logging` | Production-capable JSON file persistence. | `cargo check -p example-logging`, `cargo test --workspace --all-features`, `cargo test -p service-daemon --features file-logging`. | `example-logging`. |

Do not expand this into a full pairwise feature matrix unless a real
combination-specific gap appears. The current release baseline is default,
all-features, and no-default-features, plus the examples that exercise the
non-default features.

## Dependency Baseline

The release baseline is recorded with:

```bash
cargo tree -p service-daemon -e features --depth 1
cargo tree -p service-daemon -e features --all-features --depth 1
cargo tree -p service-daemon -e features --no-default-features --depth 1
```

Current baseline notes:

- Default features include `cron`, which enables `tokio-cron-scheduler`.
- No-default-features removes `tokio-cron-scheduler` but still uses `tokio`
  with `full` and `tracing`.
- All-features adds `tracing-appender` and `serde_json` through
  `file-logging`; `simulation` and `diagnostics` do not currently add external
  dependencies.
- `cargo audit` and `cargo deny` are not part of the current release-validation
  gate.

Changing `default = ["cron"]` or minimizing `tokio = { features = ["full",
"tracing"] }` changes public behavior and dependencies. Handle that as a
separate compatibility review, not a release-validation cleanup.

## Linkme Platform Contract Monitoring

`linkme` registration is core runtime infrastructure: services, triggers, and
providers are discovered through distributed slices. Positive platform jobs run
the smoke test and inspect the resulting test binary:

```bash
cargo test -p service-daemon --release --test linkme_smoke
bash .github/scripts/check-linkme-registry-sections host
```

Required platform signals:

| Platform family | CI shape | Meaning |
| :--- | :--- | :--- |
| Linux GNU | Positive release smoke plus registry section/symbol inspection in `rust.yml`. | Baseline ELF/Linux path. |
| Windows GNU | XFAIL without workaround plus positive with workaround in `windows-gnu-linkme-watchdog.yml`. | Known problematic MinGW section-GC path; best-effort signal only, not a release gate. |
| Windows MSVC | Positive release smoke plus registry section/symbol inspection in `rust.yml`. | COFF/MSVC path must keep preserving distributed slices. |
| macOS host target | Positive release smoke plus registry section/symbol inspection in `rust.yml`. | Mach-O path must keep preserving distributed slices. |
| Linux musl | Positive release smoke plus registry section/symbol inspection in `rust.yml`. | Alpine/musl deployments must keep preserving distributed slices. |

Do not mirror every Rust target triple. CPU architecture is not the primary
release-validation risk; prefer OS/linker/object-format coverage families.

## Example Layers

| Layer | Examples | Responsibility |
| :--- | :--- | :--- |
| Tutorial path | `minimal`, `complete`, `triggers`, `simulation` | Teach the basic service, lifecycle, trigger, and test patterns. |
| Feature verification | `logging`, `diagnostics`, `scheduling`, `unix-domain-socket` | Keep non-default or focused framework features compiling and runnable. |
| Macro compile verification | `macro-tests` | Lock macro pass/fail behavior with compile-time tests. |
| Pressure and analysis | `stress`, `memory-analysis` | Measure scale and overhead; not production API contracts. |
| Adoption reference | `web-api`, `controller-bridge` | Show realistic integration shapes without turning every detail into a framework contract. |

When adding an example, classify it here first. Do not treat every example as a
production compatibility promise.

## Release Checklist

Before cutting a release that changes release-validation checks:

```bash
cargo check --workspace --all-features
cargo test --workspace --all-features
cargo clippy --workspace --all-features -- -D warnings
cargo test -p service-daemon --no-default-features
cargo test -p service-daemon --features file-logging
```

Also confirm that the linkme platform smoke jobs are either green or, for the
Windows GNU XFAIL job, still failing in the expected no-workaround direction.

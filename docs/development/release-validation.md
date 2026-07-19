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
- `cargo deny --locked check` is the CI dependency-policy gate.
- `cargo audit` is retained as a maintainer comparison signal. Use
  `cargo audit -D warnings` only when no reviewed temporary advisory exceptions
  are active.

Changing `default = ["cron"]` or minimizing `tokio = { features = ["full",
"tracing"] }` changes public behavior and dependencies. Handle that as a
separate compatibility review, not a release-validation cleanup.

## Dependency Policy Gate

The `Dependency Policy` job in `rust.yml` installs the current `cargo-deny`
release and runs:

```bash
cargo deny --locked check
```

The policy is defined in `deny.toml`:

- advisories are checked with stale ignored advisory hygiene enabled;
- duplicate crate versions are warning-level so the release gate exposes drift
  without blocking on upstream dependency fan-out alone;
- wildcard dependencies are warning-level, mostly to keep local workspace path
  dependencies visible;
- unknown registries and unknown git sources are denied;
- crates.io is the only allowed registry source;
- licenses are allowlisted.

Known temporary advisory exceptions:

| Advisory | Path | Release stance | Removal condition |
| :--- | :--- | :--- | :--- |
| `RUSTSEC-2024-0436` | `example-web-api -> utoipa-axum -> paste` | Example-only unmaintained dependency, allowed by `deny.toml`. | Remove the ignore when `utoipa-axum` no longer pulls `paste`, or replace the example dependency path. |

As of the current baseline, `cargo audit` reports the example-only advisory as
a warning, while `cargo audit -D warnings` fails until the temporary exception
above is removed. Treat that failure as expected and documented, not as a
separate release blocker while `cargo deny --locked check` remains green.

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
cargo deny --locked check
cargo audit
```

Also confirm that the linkme platform smoke jobs are either green or, for the
Windows GNU XFAIL job, still failing in the expected no-workaround direction.

## Manual Security and Deployment Checklist

Before cutting a release that changes deployment-facing behavior, review
[Security and Deployment Contract](security-deployment.md) and check:

- framework logs still do not promise automatic secret redaction;
- file logging still degrades to console-only when the appender cannot
  initialize, and remains documented as best-effort operational logging;
- Unix socket examples and docs do not present shared `/tmp` paths as
  production-safe;
- TCP listener examples default to loopback unless the text explicitly discusses
  firewall, authentication, rate-limit, TLS or reverse-proxy controls;
- adoption examples, especially `web-api`, are described as integration
  references rather than production-ready API templates.

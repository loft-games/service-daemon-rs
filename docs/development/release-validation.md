# Release Validation

Maintainers use this map to check release and CI coverage without expanding the
README. It ties feature flags to tests, examples, platform smoke jobs, and
dependency baselines.

## Feature Validation Matrix

| Feature | Role | CI / test coverage | Example coverage |
| :--- | :--- | :--- | :--- |
| `cron` | Default feature; production trigger host support. | `cargo check --workspace`, `cargo test --workspace`, `cargo clippy --workspace`; all-features jobs also include it. | `example-triggers`, `example-complete`, `example-macro-tests`. |
| `simulation` | Test-only sandbox utilities. | `cargo check -p example-simulation`, `cargo test --workspace --all-features`, `cargo clippy --workspace --all-features`. | `example-simulation`. |
| `diagnostics` | Diagnostic topology collection, independent of HighPriority. | `cargo check -p example-diagnostics`, `cargo test --workspace --all-features`, diagnostics unit coverage for shutdown topology tracing. | `example-diagnostics`. |
| `high-priority` | Opt-in execution mode, probes, advisory and feedback-driven intervention. | `cargo test -p service-daemon --features high-priority`, `cargo test -p service-daemon --test high_priority_feature_contract_tests`; independent downstream off/on compile matrix. | Scheduling and macro-test examples. |
| `file-logging` | Production-capable JSON file persistence. | `cargo check -p example-logging`, `cargo test --workspace --all-features`, `cargo test -p service-daemon --features file-logging`. | `example-logging`. |

Do not expand this into a full pairwise feature matrix unless a real
combination-specific gap appears. The current release baseline is default,
all-features, and no-default-features, plus the examples that exercise the
non-default features.

## General Platform CI

### Framework operation benchmarks

The `framework` Criterion target measures framework operation costs independently
of the runtime experiments below. Criterion is a dev-dependency; the library and
benchmark share `src/crate_root.rs` declarations and the same implementation files.
Cargo enables `cfg(test)` for this target. The benchmarks call framework operations
directly using pre-built fixtures.

One concrete configuration difference matters: with default features,
`cfg(test)` retains runtime-probe counters and snapshot reads that a normal
non-HighPriority library build replaces with zero statistics. These are
test-build operation baselines, not exact default-production timings. The
startup banner records test/HighPriority/debug configuration; compare like builds.

The `Rust CI` workflow has a dedicated Linux `Framework benchmarks` job. It runs
separately from `Check` and `Test`, and compiles, smoke-runs, then fully measures
the default and HighPriority configurations. The two measurements run sequentially
on the job's runner. Equivalent local build and smoke commands are:

```bash
cargo bench -p service-daemon --bench framework --no-run
cargo bench -p service-daemon --bench framework -- --test
cargo bench -p service-daemon --bench framework --features high-priority --no-run
cargo bench -p service-daemon --bench framework --features high-priority -- --test
```

Open **Actions → Rust CI → a run → Summary** for the benchmark tables. Each table
shows mean ns/op, the mean's confidence interval, median ns/op, and sample count.
These estimates describe operation costs; confidence intervals are not latency
percentiles. The report includes the checked-out commit (the merge commit for a
normal PR run), toolchain, lockfile hash, CPU, OS and feature configuration.

Download `framework-benchmarks-<run-id>-<attempt>` from the run's **Artifacts** for
`report.md`, `metadata.json`, command logs, and raw Criterion JSON in separate
`default/` and `high-priority/` directories. Artifacts are retained for 30 days.
CI uses `--locked` and `--noplot`; Markdown and JSON are the published reports.

Failed commands, missing cases, or malformed result files fail the job. The
summary marks incomplete configurations as failed and preserves available rows;
report generation and artifact upload are attempted even after earlier failures.
Runner termination or cancellation can prevent final publication. No timing
threshold fails CI, and results from different hosted runners are not automatically
classified as regressions. Each run uses a fresh directory; prior Criterion
baselines are not restored into it.

The CI report adapter presents Criterion's existing estimates. Its filesystem
and failure-path tests run without Cargo:

```bash
python3 -B -m unittest discover -s .github/scripts -p 'test_framework_benchmark_report.py'
```

There are seven default measurement points and eight with HighPriority enabled:

| Group | Cases | Measurement contract |
| :--- | :--- | :--- |
| `observation` | `standard_steady`, plus `high_priority_steady` when enabled | Real completed ServiceSleep recording, requested 1 ms / elapsed 2 ms. Pre-fill 128 observations outside timing; measure recording including its internal clock, locks and aggregate updates, never a real sleep. |
| `diagnostics_snapshot` | 1, 32, 256 instances | One Standard generation and 128 observations per instance. Include full snapshot creation, consumption and destruction; exclude fixture construction, concurrent writes and serialization. These sizes are measurement points, not recommended capacities. |
| `provider_resolve` | `immutable_warm`, `managed_warm`, `arc_clone_drop_reference` | Pre-initialized `StateManager<u64>` with real managed promotion for the managed case. Reuse a current-thread runtime; time async resolution and returned Arc consumption/destruction, not initialization or per-operation runtime/block_on construction. The Arc case uses the same async measurement form and is a reference, not a required target. |

Fixture assertions run outside timing, also in smoke mode: aggregate counts must
match at generation/service/lane levels, the HP window must stay bounded, snapshot
populations must match, and warm resolution must return the expected Arc without
calling an initializer. Criterion's default sampling parameters are retained.
The HP fixture waits once for 2 ms before pre-filling so inferred sleep starts
are inside the registered generation; this setup wait and the zero-settling
window inspection are not timed and do not run or modify the production policy.

For local full measurements, avoid concurrent builds or stress tests, and
keep feature configurations in separate directories:

```bash
CRITERION_HOME="$PWD/target/criterion/default" cargo bench -p service-daemon --bench framework
CRITERION_HOME="$PWD/target/criterion/high-priority" cargo bench -p service-daemon --bench framework --features high-priority
```

Run these commands from the workspace root; absolute output paths avoid Cargo's
package working directory changing the destination. Use fresh output directories
when preserving a run; Criterion may update prior
results at the selected location. For cross-revision comparisons, record the
commit, dirty-worktree state (and preserve changed source if needed), lockfile,
toolchain, host, feature set, parameters and fixture definition. Keep them
comparable before interpreting Criterion's baseline differences. Reports remain
under `target/` and are not committed or automatically cleaned by this workflow.
They are not the recoverable source-and-executable archives of the long runtime
experiments. Operation timings are neither service recovery times nor sleep-drift
P99/P99.9 or business throughput guarantees. CI acceptance requires successful
execution and complete results, not a percentage-regression threshold.
Console statistics and JSON results do not require plotting tools. With the
selected minimal Criterion features (no `plotters`), graphical HTML reports
require an available Gnuplot installation; missing plots do not imply missing
measurements. This workflow does not install extra plotting software.

### HighPriority feedback validation

Use the [design contract](../architecture/high-priority-feedback.md) together with
these focused checks when changing observations, placement, or convergence:

```bash
cargo test -p service-daemon --no-default-features --lib
cargo test -p service-daemon --all-features --lib
cargo test -p service-daemon --features high-priority --lib high_priority
cargo test -p service-daemon --test high_priority_feature_contract_tests
cargo test -p service-daemon --features high-priority --test high_priority_runtime_policy_tests
```

The last command includes
`timeout_reload::late_policy_generation_stays_paused_until_external_provider_reload`.
It waits for the real 120-second intervention deadline, then verifies fresh
pressure cannot rearm the late policy generation, the warning is emitted once,
and an external provider reload restores evaluation. Allow several minutes;
other tests sharing its lock may report waiting for more than 60 seconds.
Do not replace this coverage with a shortened policy timer or a capacity-bound
test that could hide an unintended rollover.

The downstream compile fixture runs outside the workspace feature union. It must
reject direct, service-macro, and trigger-macro HighPriority declarations without
the feature and accept them with it; Standard/Isolated work in both cases.

Run the manual experiment without concurrent builds or stress tests:

```bash
cargo test -p service-daemon --features high-priority --lib benchmark -- --ignored --nocapture
```

This uses a short-cycle test policy and does not calibrate production defaults.
Report whole-run and per-generation latency separately, alongside worker counts
and reloads. External async wait outside ServiceSleep is a metric-boundary check,
not evidence of low-benefit convergence. See the experiment design and interpretation in the
[design document](../architecture/high-priority-feedback.md#validation).

#### Production-default calibration

Use the separate release-build harness when evaluating the automatic defaults:

```bash
cargo test -p service-daemon --features high-priority --lib calibration::
cargo test -p examples-scheduling --test cadence_lifecycle
SD_CALIBRATION_OUTPUT_DIR="$PWD/target/calibration/rust-quick" cargo test -p service-daemon --release --features high-priority --lib calibration::runner::calibration_quick -- --ignored --nocapture
SD_CALIBRATION_OUTPUT_DIR="$PWD/target/calibration/rust-baseline" cargo test -p service-daemon --release --features high-priority --lib calibration::runner::calibration_full -- --ignored --nocapture
```

Output directories must be new; earlier evidence is never overwritten. The quick
profile runs twelve ten-second cases and only establishes `smoke_only`. The full
profile runs three repetitions of four scenarios in three modes, ninety seconds
per case (about 54 minutes plus compilation). Mode order rotates between
repetitions. Reserve at least three available CPUs and 5 GiB of free workspace
disk, and run without concurrent builds or stress tests.
The synthetic blocking interval defaults to 250 ms; `SD_CALIBRATION_BLOCK_MS` selects a
different interval (1–1000 ms) and records it in the manifest and case header.
This controls the test workload, not the runtime policy. Keep it identical across
mode comparisons, and do not replace an inconclusive baseline with a different
workload while presenting the latter as the same experiment.

For a separate, isolation-relievable dual-pressure experiment, use the existing
400 ms workload parameter with a fresh directory:

```bash
SD_CALIBRATION_OUTPUT_DIR="$PWD/target/calibration/rust-contention-400ms" SD_CALIBRATION_BLOCK_MS=400 cargo test -p service-daemon --release --features high-priority --lib calibration::runner::calibration_full -- --ignored --nocapture
```

The contention competitor stays on the source resource while the observed
service can move through generation rollover. Check that source-shard pressure
continues after placement, so workload cessation does not explain the benefit.
The interval is a workload choice, not a guarantee that every host will satisfy
the pressure gates. A successful repeated run establishes the intervention loop
for that workload and environment; it does not invalidate an earlier
inconclusive run or require a production-policy change.

Each case runs in its own process, with a five-second warmup, real service
instances and supervisor reload, and the automatic production policy including
the two-second settling period. Fixed HighPriority stops only its internal policy
loop. No public tuning interface is introduced. Standard has one body worker;
HighPriority starts with one worker inferred from the selected template. The
manifest records toolchain, source and executable hashes, and workload parameters;
the raw case records include CPU availability, policy, generation/actual placement,
completed/interrupted sleeps, resource snapshots, policy events, and cleanup.
The Rust runner's manifest schema 1 archives an explicit allowlist of workspace build inputs under
`sources/`, including untracked harness/runner files, crate sources, manifests,
the lockfile and project build configuration. It does not copy arbitrary private
files or the global Git diff. Compilation runs from that snapshot with `--locked`;
the executable is copied into the artifact directory and checked before use.
The archived executable runs both the workload and typed evidence validator, so
the manifest binds the archive ID, workload/validator executable hash, workload
parameters, raw JSON hash and summary hash. Keep the whole artifact
directory, not just `report.md`. Source files can be restored from `sources/`
without the original worktree; dependency downloads and the recorded toolchain
are still needed for rebuilding. Hash verification checks content integrity,
not reproducible-build identity across different toolchains or environments.
Shard probe snapshots record whether the supporting pressure gate was actually
met. A high whole-run service mean alone does not establish sustained accepted
windows or shard pressure; inspect both before diagnosing a missing intervention.

Interpret `report.md` with the raw JSON and per-generation summary JSON:

- Healthy and external asynchronous wait must not expand resources. External
  wait is outside the measured sleep, not a low-benefit workload.
- Contention must demonstrate an evaluated beneficial intervention. Compare
  whole-run and post-generation results separately across all repetitions.
  A pass requires a unique request linked by instance and source generation to
  the evaluation, the next actual generation and shard, and sufficient comparable
  completed before/after samples. The independent samples must support the
  reported outcome; an `Improved` string alone is insufficient.
- Self-induced blocking travels with the service during reload. Low-benefit
  convergence requires a pause before the resource cap, continuing pressure and
  samples after the pause, and no subsequent resource request.
  Both interventions must form a continuous chain for the same instance; the
  continuing pressure must belong to its paused generation and actual shard.
  Missing, contradictory or cross-instance evidence cannot pass. The case header
  must match the runner's mode, scenario, duration and blocking-work parameter.
- Missing evidence is `inconclusive` or an error, never a pass. Preserve all
  failed/inconclusive runs. Tail estimates with fewer than 100 samples are marked
  in the summary; these synthetic measurements are not a latency SLA.
  P99.9 has a separate low-sample marker below 1000 samples.

Always label percentile populations: whole-run P99.9 can retain early waiting
even when the new generation's P99 is low. Whole-run means are sample-weighted;
a faster post-intervention generation contributes more samples. Neither the
post-generation tail nor the whole-run mean alone describes the transition cost.

The runner exits nonzero on failed, incomplete, or inconclusive full profiles.
Do not tune thresholds merely to make the synthetic matrix green. A candidate
change needs a separate output directory and the same workload, toolchain and
hardware comparison. Calibration does not replace deterministic tests or the
real 120-second timeout regression above.

Older hash-only manifests cannot restore changed untracked experiment sources.
Preserve such runs as historical observations, explicitly mark their provenance
limitation, and do not re-label their verdicts as validation by a newer summarizer.
If exact source contents cannot be recovered and checked against the old hashes,
generate a new full baseline after fixing the summarizer and archive boundary.

Focused checks do not replace the release matrix, ignored tests, or platform
jobs. Local IPC tests require a writable runtime socket directory; sandbox
denial is not a test pass.

#### Linux idle-resource cost experiment

Measure the cost of retaining empty HighPriority resources before deciding whether
to implement reclamation. This test-only experiment constructs one shard and then
up to twelve single-worker shards (bounded by the current worker budget), starts
and removes real services on them, and measures HP probes on/off. Control/Standard
probes and the policy loop remain enabled. It does not test automatic scale-out
decisions or implement single-shard shutdown.

```bash
cargo test -p service-daemon --features high-priority --lib calibration::idle -- --test-threads=1
SD_IDLE_DIR="$PWD/target/calibration/linux-idle-cost" cargo test -p service-daemon --release --features high-priority --lib calibration::idle::idle_cost_experiment -- --ignored --nocapture
```

Use a new absolute directory, at least two available workers, and Linux procfs
with readable per-thread `schedstat` and process `smaps_rollup`. Allow about ten
minutes plus compilation; keep other builds and stress tasks stopped. Three
fresh processes use thirty-second windows, with HP-probe order alternating.
`SD_IDLE_SECONDS` can shorten a separate smoke run, not establish cost evidence.
The driver builds from an allowlisted source snapshot and retains executable,
raw thread/memory samples, pool/probe evidence, completed windows, exit status,
report, and content hashes. Preserve failures as well as completed runs.

Interpret HP-worker CPU separately from total process CPU, which also includes
control work and sampling. Thread identity changes or counter resets invalidate
a window. Report RSS/PSS separately from virtual address space; process memory
also includes the harness, retained diagnostics and allocator caches. The
after-daemon-shutdown comparison includes control runtime teardown and is not
evidence of an implemented single-shard reclamation path. No universal acceptable
idle-cost threshold or automatic decision to add scale-down follows from this
experiment.

#### Linux empty-shard reuse experiment

For the resource-retention decision and the separate questions answered by idle
cost, reuse, repeated cycles, placement-order comparisons and reclamation, see
[the performance evidence boundaries](../architecture/performance-benchmarks.md#highpriority-resource-retention-and-reclamation-evidence).

Validate whether a shard left empty by an earlier intervention helps an already
running service during a later pressure episode. This test uses production
policy, real generation rollover, removal of the first affected instance, and
a sixty-five-second quiet valley. It does not change policy thresholds, disable
probes, force placement, or implement scale-down.

```bash
cargo test -p service-daemon --features high-priority --lib calibration::reuse -- --test-threads=1
mkdir -p target/calibration
SD_REUSE_DIR="$PWD/target/calibration/empty-shard-reuse" cargo test -p service-daemon --release --features high-priority --lib calibration::reuse::reuse_experiment -- --ignored --nocapture
```

Use a new absolute directory, Linux `taskset`, and at least three available CPUs.
Allow about twelve minutes plus compilation, with other builds/stress tasks
stopped. The driver archives and builds the sources, then runs three fresh
processes per capacity, alternating case order. Two- and three-CPU affinity
produce inferred worker budgets of two and three without policy overrides.
These are placement comparisons, not equivalent-capacity performance comparisons.

Accept evidence only when both interventions independently pass the existing
Rust validator, the retained target is empty and Nominal immediately before the
second request, actual generation placement matches the request, and source
pressure continues after intervention. Report reuse without worker growth
separately from allocating another shard despite an existing empty one. Preserve
raw observations, resource/probe snapshots, exits, failures, executable and source
hashes. A failed case is insufficient evidence, not proof that reuse is impossible.
Results establish behavior for this workload, not universal reuse or latency
guarantees; they do not by themselves establish that reclamation is necessary.

The repeated-cycle variant holds the inferred budget at three workers and runs
six pressure episodes in each of three fresh processes. All six subjects are
already running on the original shard before pressure starts; each is activated
once, independently evaluated, and removed through the public lifecycle API.
The first two episodes exercise growth; the remaining four must demonstrate
reuse at the ceiling. Sixty-five-second inter-round valleys keep production
cooldowns intact. Allow approximately twenty-five minutes plus compilation.

```bash
mkdir -p target/calibration
SD_REUSE_DIR="$PWD/target/calibration/repeated-pressure" cargo test -p service-daemon --release --features high-priority --lib calibration::reuse::cycles_experiment -- --ignored --nocapture
# Read-only replay using the archived executable; repeat for each case directory.
SD_REUSE_CASE="$PWD/target/calibration/repeated-pressure/1-capacity-3" target/calibration/repeated-pressure/reuse-test --exact core::service_daemon::high_priority::calibration::reuse::cycles::cycles_replay --ignored --nocapture
```

Require complete per-round evidence, bounded worker counts throughout, correct
active/assigned accounting after every removal, and no pending placement or
rollover at round boundaries. Each full-budget intervention must use a target
observed empty and Nominal immediately beforehand. Preserve partial-round raw
observations on failure. Replay checks stored results and rejects missing rounds
and crossed identities. Six episodes establish finite repeated-cycle behavior,
not a long-duration soak, allocator-memory convergence, or an idle-first policy
comparison; those need separate evidence.

Distinguish successful repeated reuse from utilization of every retained shard.
Record which targets remain unused; repeatedly selecting the same empty target
does not alone indicate a correctness defect or prove another shard unnecessary.
Since subjects are removed after each episode, do not treat successive rounds
as equal-concurrency performance comparisons. An allocation-first/idle-first
comparison must hold CPU affinity, budget, workload and initial topology equal;
it is separate from testing reclamation savings and rebuild costs.

Replay compares the exact regenerated report bytes, avoiding a second floating
point parse of serialized means. If a replay-only correction is needed after a
run, preserve its original source, executable, reports and manifest. Archive and
run a separately identified verifier against the existing raw data:

```bash
SD_REUSE_DIR="$PWD/target/calibration/repeated-pressure" cargo test -p service-daemon --release --features high-priority --lib -j 2 calibration::reuse::cycles::cycles_archive_replay -- --ignored --nocapture
```

This creates a fresh `replay-verifier/` subdirectory, verifies original hashes
before and after replay, and records new verifier sources, executable, exits and
their hashes without rewriting the original manifest. It does not rerun the
workload or retroactively change the identity of its producer.

Report original-executable replay failures separately from corrected-verifier
success. Byte-identical report reconstruction establishes replay consistency,
not a new workload run. If earlier archives are unavailable, state that they
cannot be rechecked; never regenerate them from newer sources as replacements.

#### Same-budget placement preference comparison

The idle-first A/B variant is a `cfg(test)`-only experiment, not a production
configuration or a change to default placement semantics. Its default-disabled
pool-local switch is enabled only after the common first intervention and
sixty-five-second empty-shard valley. The candidate must be a different Nominal
shard with zero active generations and zero assigned instances. All pressure,
global-pressure, cooldown, generation rollover and benefit gates stay shared.

```bash
mkdir -p target/calibration
SD_REUSE_DIR="$PWD/target/calibration/placement-ab" cargo test -p service-daemon --release --features high-priority --lib -j 2 calibration::reuse::ab_experiment -- --ignored --nocapture
```

Four fresh-process pairs alternate growth-first/idle-first order, using the
same archived executable, three-CPU affinity, inferred budget three, initial
topology and workload. Allow about twenty minutes plus compilation and stop
unrelated builds during measurement. Each arm must independently pass the
two-intervention validator and exact-byte raw-data replay; failed or incomplete
cases remain archived and must not disappear from the comparison.

Compare peak workers, pressure-onset-to-generation/evaluation delay, and tails
from separate populations: the full fixed fifty-second second pressure phase,
and thirty seconds after the new generation's two-second settling interval.
Both windows require complete observations. Mark P99.9 exploratory below 10,000
samples. Report paired differences and variability; four pairs do not establish
statistical non-inferiority or a universal latency guarantee. Resource reduction
alone does not authorize changing production defaults or adding scale-down.

Keep resource benefit and performance claims separate: avoiding one HP worker
does not imply proportional process CPU/RSS savings, and earlier generation
arrival does not measure runtime construction cost. Preserve adverse Max and
percentile differences even when both arms satisfy intervention acceptance.
The controlled empty-shard valley does not validate concurrent target selection,
pending placement reservations or reuse during cooldown. Before production
adoption, separately define and test target eligibility and fresh observations,
concurrent startup/reservations, failed requests and lifecycle cleanup, existing
fallback behavior, cooldown semantics and benefit-pause protection. Do not simply
enable the experimental switch by default.

#### Long-valley retention versus reclamation

The Linux `reclaim_experiment` compares retaining/reusing an empty shard with
test-only, controlled tail reclamation followed by demand-driven rebuilding.
It is not a production scale-down controller or a public policy setting.

```bash
mkdir -p target/calibration
SD_REUSE_DIR="$PWD/target/calibration/reclamation" cargo test -p service-daemon --release --features high-priority --lib -j 2 calibration::reuse::reclaim_experiment -- --ignored --nocapture
```

Allow about fifty minutes plus compilation for four fresh-process pairs. Both
arms use the same archived executable, three-CPU affinity and inferred budget,
natural first expansion, a five-minute low valley, and a fixed fifty-second
second pressure phase. Alternate arm order and keep unrelated builds stopped.
Retaining uses the experimental idle-first preference from the preceding A/B;
reclamation removes only the last dynamically created, empty Nominal shard.

Reclamation must reject active/assigned/reserved targets and the initial shard,
detach the target from placement, stop and join only its probe, then join the
runtime shutdown before crediting worker budget. Verify the actual Linux worker
TID disappeared and the initial worker survived. Rebuilding uses a fresh shard
identity. No concurrent service creation is permitted during this controlled
experiment; successful results do not validate a production concurrent-retirement
protocol. Historical diagnostics may retain the retired shard's observations;
the live placement list and runtime facts must exclude it.

Report thread and file-descriptor release separately from RSS/PSS and virtual
address space. Compare stable idle CPU/context-switch rates and record process
CPU counters that include exited threads around transitions. Sampling overhead
and trace-buffer growth affect process-wide memory and CPU and are common to
both arms. Do not infer proportional CPU/RSS savings from worker counts, equate
virtual memory with resident memory, or assign a universal economic score.
Only estimate a CPU-time break-even interval when both saving and transition
cost are identifiable above noise; otherwise mark it inconclusive.

Separate target-worker exit from an immediate reduction in total process threads:
shutdown helpers can temporarily replace the retired worker in the count. Keep
both transition snapshots and stable-valley observations. A helper-thread path
may also change address-space retention; do not attribute the entire difference
without an allocator/mapping control or generalize it to production reclamation.
Report valley occupancy separately from full-case resource peaks. Successful
rebuilding establishes functional recovery, not that generation-delay differences
equal measured runtime construction cost or that all tails are non-inferior.

Use the same fixed post-generation thirty-second tail window after two-second
settling, separate from whole-second-phase tails. Preserve incomplete cases and
replay each successful case using the archived `reclaim_replay` entrypoint.
Fast `reclaim_` unit tests cover ownership refusal, real probe/worker shutdown,
fresh identity/budget and counter integrity. Keep all preceding archives intact.

### Workspace commands

`rust.yml` treats Linux GNU and Windows MSVC as cross-platform general CI
platforms:

```bash
cargo check --workspace
cargo check --workspace --all-features
cargo check -p service-daemon --no-default-features
cargo test --workspace
cargo test --workspace --all-features
cargo test -p service-daemon --no-default-features
cargo clippy --workspace -- -D warnings
```

The Windows MSVC job runs these gates with
`--target x86_64-pc-windows-msvc` for the cross-platform workspace surface. It
explicitly excludes Unix-only example crates such as `example-unix-domain-socket`
and keeps the local IPC examples in the IPC-specific job. Its
test steps use `.github/scripts/cargo-test-windows-msvc-general` so
`service-daemon` integration tests can run on Windows while skipping
platform-specific IPC targets (`local_ipc_*`, `named_pipe_*`, and `unix_*`).
Keep OS-specific IPC checks separate from this baseline so generic runtime
regressions, Unix-socket regressions, named-pipe regressions, and LocalIpc
mapping regressions fail in clearly named jobs.

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

## Windows Local IPC Provider Gate

`NamedPipeListen`, `NamedPipeConnect`, and the Windows side of `LocalIpcListen`
/ `LocalIpcConnect` use Tokio's Windows-only named pipe runtime APIs.
Linux/macOS can cover parser behavior and non-Windows compile errors, but they
cannot execute the Windows provider runtime contract. The release gate for these
templates is the `Windows local IPC providers` job in `rust.yml`.
For release-candidate evidence, maintainers can manually run the focused
`Windows Local IPC Providers` workflow. Both workflows call
`.github/scripts/run-windows-named-pipe-provider-tests`, which writes the target,
command set, and final pass marker to the GitHub step summary. Its command set
is:

```bash
cargo test --target x86_64-pc-windows-msvc -p service-daemon --test named_pipe_strategy_tests
cargo test --target x86_64-pc-windows-msvc -p service-daemon --test named_pipe_roundtrip_tests
cargo test --target x86_64-pc-windows-msvc -p service-daemon --test local_ipc_roundtrip_tests
cargo test --target x86_64-pc-windows-msvc -p example-named-pipe
cargo test --target x86_64-pc-windows-msvc -p example-local-ipc
```

`LocalIpc*` remains a logical-name facade only. Platform-native endpoint control
stays in `UnixListen` / `UnixConnect` / `NamedPipeListen` /
`NamedPipeConnect`.

## Example Layers

| Layer | Examples | Responsibility |
| :--- | :--- | :--- |
| Tutorial path | `minimal`, `complete`, `triggers`, `simulation` | Teach the basic service, lifecycle, trigger, and test patterns. |
| Feature verification | `logging`, `diagnostics`, `scheduling`, `local-ipc`, `unix-domain-socket`, `named-pipe` | Keep non-default or focused framework features compiling and runnable. |
| Macro compile verification | `macro-tests` | Lock macro pass/fail behavior with compile-time tests. |
| Pressure and analysis | `stress`, `memory-analysis` | Measure scale and overhead; not production API contracts. |
| Adoption reference | `web-api`, `controller-bridge` | Show realistic integration shapes without turning every detail into a framework contract. |

When adding an example, classify it here first. Do not treat every example as a
production compatibility promise.

## Platform-specific IPC Provider Checks

Unix socket, Windows named pipe, and cross-platform logical LocalIpc provider
templates have platform-specific runtime contracts. Keep their tests separate so
failures identify the OS-specific surface:

```bash
cargo test -p service-daemon --test unix_listen_strategy_tests
cargo test -p service-daemon --test unix_connect_strategy_tests
cargo test -p service-daemon --test unix_roundtrip_tests
cargo test -p service-daemon --test local_ipc_roundtrip_tests
cargo test -p example-local-ipc
cargo test --target x86_64-pc-windows-msvc -p service-daemon --test named_pipe_strategy_tests
cargo test --target x86_64-pc-windows-msvc -p service-daemon --test named_pipe_roundtrip_tests
cargo test --target x86_64-pc-windows-msvc -p service-daemon --test local_ipc_roundtrip_tests
cargo test --target x86_64-pc-windows-msvc -p example-named-pipe
cargo test --target x86_64-pc-windows-msvc -p example-local-ipc
```

The Windows commands are wired into `.github/workflows/rust.yml` as the
`named-pipe-msvc` job and must stay on the `x86_64-pc-windows-msvc` target.

The Windows commands need real named-pipe permissions. A restricted-token sandbox can turn otherwise valid local pipe opens into `PermissionDenied`, so release validation should run them in a normal Windows test context.

`docs/development/windows-named-pipe-ipc.md` records why the Windows named pipe provider contract uses explicit `NamedPipeListen` / `NamedPipeConnect` templates, and how the `LocalIpc*` facade layers logical names over Unix sockets or Windows named pipes.

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

# HighPriority feedback control

`high-priority` is an opt-in execution capability, not a hard-real-time SLA.
Core owns lifecycle, providers, triggers, reload, and shutdown. The optional
`core/service_daemon/high_priority/` module owns allocation, placement intent,
bounded performance observations, and the feedback controller. Core lifecycle
facts remain available without this feature; HighPriority variants, pool state,
performance probe tasks, and advisory tasks do not.

## Observation and intervention

The initial feedback path measures completed `service_daemon::sleep()` calls for
each HighPriority instance and generation. A bounded 128-sample buffer retains
nanosecond precision. Samples older than 30 seconds are excluded, as are sleeps
that began before the observation boundary. Interrupted sleeps are not latency
samples. Post-start sampling begins after a two-second settling period. Each
accepted window advances a cursor, so unchanged data cannot establish sustained
pressure or validate an intervention.

The controller uses mean sleep drift for both triggering and benefit evaluation.
Shard probe pressure is supporting execution-contention evidence, not CPU or IO
attribution. This metric is not business-cycle latency or a unified workload
score. Services without valid sleep observations cannot currently drive this
feedback path. No manual business instrumentation API is required.

One intervention is pending per pool. The controller records the instance,
generation, source shard, baseline, target shard, and resulting worker count;
requests reload; then waits for actual new-generation observations. A placement
intent survives old-generation release and is consumed once. Invalid targets
fall back to ordinary placement; evaluation uses the actual shard. Removal,
termination, and expired interventions clear pending placement intent.

All services are reloadable under the existing lifecycle contract. Business
continuity, in-flight work handling, and restoration remain the author's
responsibility. The framework does not migrate live futures or promise lossless
business execution across reload.

## Conservative convergence

The worker cap, allocation cooldown, and rollover cooldown remain safety limits.
A candidate also needs sustained fresh pressure windows. Internal defaults are
automatic policy choices, not public configuration contracts.

- At least 10% reduction of the triggering metric counts as improvement.
  Remaining pressure can justify another intervention after cooldown.
- Cleared pressure ends the intervention without requesting more resources.
- Two successive low-benefit interventions pause expansion for the instance;
  deterioration also counts as low benefit.
- Insufficient samples wait rather than count as failure or improvement.
- A mean requested sleep duration change exceeding 25% makes the window
  incomparable. The baseline remains pending for fresh comparable evidence.
- Unchanged placement or an intervention not evaluated within 120 seconds pauses
  further intervention. The pause retains the intervention identity independently
  of the pending evaluation. A late generation from that intervention, an unknown
  generation change, or cooldown expiry does not rearm the instance.
  A fresh healthy window or a generation following an explicitly observed external
  reload provides a new evaluation opportunity. The supervisor records provider
  changes and non-policy reload signals; the policy notification marker survives
  intervention expiry until the signal is consumed. External reload evidence is
  consumed once, only after crossing its generation boundary. Coalesced signals
  with a pending policy marker are conservatively treated as policy reloads.

Warnings are emitted on entering a paused state with reason, before/after metric,
instance/generation, target/actual placement, and resource count. Routine policy
polling does not repeatedly print that warning. Stopping expansion does not
immediately reclaim shards; scale-down is outside this controller.

## Validation

Deterministic tests cover window boundaries, stale samples, generation handoff,
low benefit, improvement, insufficient evidence, and resource accounting. An
independent downstream Cargo fixture verifies that default and no-default-feature
consumers cannot name or declare HighPriority, even when another workspace member
enables it.

Run the manual smoke experiment separately:

```bash
cargo test -p service-daemon --features high-priority --lib benchmark -- --ignored --nocapture
```

It compares Standard, fixed HighPriority, and adaptive HighPriority using real
service bodies, supervisor reload, and sleep. It reports drift percentiles,
generation changes and workers for execution contention and external async wait.
These are machine-specific measurements, not CI latency guarantees or production
threshold calibration. External wait checks the observation boundary; the
deterministic feedback tests, not that workload, prove low-benefit stopping.

### Local smoke observation (2026-09-07)

One debug-profile run with the short-cycle internal test policy produced these
results; they are not a repeated benchmark study or production-policy calibration:

| Scenario / mode | HP workers | Probe reloads | Sleep drift P99 (ms) |
| --- | ---: | ---: | ---: |
| Shared worker CPU contention / Standard | 0 | 0 | 25.147 |
| Shared worker CPU contention / fixed HighPriority | 1 | 0 | 25.241 |
| Shared worker CPU contention / adaptive HighPriority, all generations | 2 | 1 | 20.061 |
| Shared worker CPU contention / adaptive HighPriority, new generation only | 2 | 1 | 1.432 |
| External async wait / adaptive HighPriority | 1 | 0 | 1.511 |

The whole-run adaptive tail includes the pre-intervention interval; the
new-generation result must not be substituted for the whole-run result. The
external-wait case retained roughly 47 ms median business-round duration without
mistaking it for sleep drift or requesting expansion.

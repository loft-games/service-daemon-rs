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

### Experiment design and interpretation

The smoke harness starts with one body-lane worker. It compares Standard,
fixed HighPriority with its controller stopped, and adaptive HighPriority with
a short-cycle internal policy permitting two HighPriority workers. Each scenario
runs for four seconds and records real framework sleep calls and generation
changes; it is not production-policy calibration.

- Execution contention combines a five-millisecond sleep loop with a competing
  service performing 25 milliseconds of CPU work. Compare whole-run latency,
  post-reload latency, worker cost, and reload count. A lower post-reload tail
  demonstrates improvement for that workload, not an absolute latency guarantee.
- The external-wait scenario adds 40 milliseconds of asynchronous wait outside
  the measured sleep. Business-round duration and sleep drift are different
  measurements: external wait alone is not evidence for shard expansion.
- Whole-run statistics include pre-intervention pressure and reload transitions.
  Never substitute post-generation results for the whole-run tail or compare
  different modes using different observation scopes.
- The smoke test requires observations but does not assert machine-dependent
  latency thresholds. Deterministic controller tests establish low-benefit
  stopping; passing this experiment does not establish that property by itself.

The [maintainer validation map](../development/release-validation.md#highpriority-feedback-validation)
defines regression scope and real timeout coverage.

### Production-default calibration boundary

The separate calibration harness uses the automatic policy rather than the
short-cycle smoke policy. Test builds retain the production two-second settling
period; only deterministic controller tests explicitly opt into zero settling.
It compares healthy execution, movable contention, self-induced blocking that
persists after placement, and external asynchronous wait. The last two workloads
are intentionally different: persistent measured drift can establish low-benefit
stopping, while unmeasured external wait cannot.

The calibration records actual generation placement and policy evaluation events
alongside raw workload samples. Whole-run latency includes the cost of waiting
for intervention and reload; per-generation latency describes the new resource
environment. Neither substitutes for the other. A low-benefit pause is only
meaningful if pressure continues and the worker cap has not already prevented
expansion. Machine-specific results validate these synthetic scenarios, not a
general diagnosis of CPU/IO causes or a business latency guarantee. Reproduction
commands and evidence acceptance belong in the maintainer validation map.

An isolation-relievable contention experiment keeps a separate competitor on
the source shard while the observed service rolls over to another shard.
Sustained service and source-shard pressure must both support intervention;
continued source pressure after relocation helps distinguish isolation benefit
from the workload simply ending. Repeated success establishes this bounded
feedback behavior, not universal latency improvement. Report whole-run and
post-generation percentiles with their sample populations: early waiting remains
part of the whole-run tail, and faster generations contribute more samples to
sample-weighted means. A different workload is a separate experiment, not a
replacement verdict for an earlier inconclusive baseline.

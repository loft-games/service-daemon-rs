# Memory Analysis Tool

Measures per-service memory overhead of `service-daemon-rs` through three
complementary layers:

| Layer | What It Shows | Technique |
|:------|:-------------|:----------|
| **Section 1 - Static** | Stack footprint of core types | `std::mem::size_of` |
| **Section 2 - Dynamic** | True heap cost of each component | RSS delta (N=1000 isolation) |
| **Section 3 - End-to-End** | Full framework per-service delta | RSS delta (100 real services) |

An automatic **Component Attribution** reconciles isolation costs against
the end-to-end delta to show where each byte goes.

## Quick Start

```bash
cargo run --release -p example-memory-analysis
```

> **Platform**: Dynamic RSS measurement requires **Linux** (`/proc/self/statm`).
> On other platforms, only the static analysis (Section 1) is available.

## Understanding the Output

### Section 1: Static Sizes

Reports `std::mem::size_of` -- the **on-stack** footprint. These are lower
bounds because they do not include heap-backing stores behind pointers
(`Arc`, `DashMap` buckets, etc.).

### Section 2: Dynamic Isolation

Each component is allocated **1,000 times in isolation** and the resulting
RSS delta measured. This captures allocator metadata, hash bucket overhead,
and `Arc` control blocks that `size_of` cannot see.

A **warmup round** runs before each test to prime the allocator and reduce
first-allocation noise.

### Section 3: End-to-End

Starts 100 real services via `ServiceDaemon`, waits for stabilization,
then measures the total RSS delta. This validates isolation data against
the actual framework overhead.

### Component Attribution

Compares isolation costs against the E2E delta and reports percentages.
If the **Unaccounted** value is near zero (or slightly negative), all
overhead is fully explained by the measured components.

## Keeping the Mock in Sync

The tool uses a `MockSupervisor` struct that mirrors the private
`ServiceSupervisor` in `runner.rs`. A **compile-time assertion** checks that
`size_of::<MockSupervisor>()` matches the expected value. If the real struct
changes layout, this assertion will fail, prompting you to update the mock.

## Measured Components

| Component | What It Represents |
|:----------|:------------------|
| `DashMap<ServiceId, ServiceStatus>` | StatusPlane entry (lifecycle tracking) |
| `DashMap<ServiceId, Arc<Notify>>` | ReloadSignals entry (hot-reload support) |
| `CancellationToken::new()` | Shutdown/reload token (x2 per service) |
| `Box<ServiceSupervisor>` | Supervisor struct heap allocation |
| `tracing::info_span!` | Per-service tracing span metadata |
| `tokio::spawn` | Tokio task runtime cost (future boxing + header) |
| `HashMap<ServiceId, JoinHandle>` | Running tasks map entry |

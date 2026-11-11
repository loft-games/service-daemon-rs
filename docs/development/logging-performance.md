# File Logging Benchmarks

Measured on November 11, 2026, using the logging working tree based on
`917cd8b3067747d164069b0a11bff4b420de4fe3`.

## Setup

| Item | Value |
|---|---|
| CPU | Intel Core i5-11400, 2.60GHz, 12 logical CPUs |
| OS | Linux 6.18.57-2-lts, x86_64 |
| Rust | 1.99.1 (`58cf8f9c3`), release/bench profiles |
| Storage | Workspace disk `/dev/sda1`; `stat -f` reports ext2/ext3 |
| Criterion | 20 samples, 1s warmup, 2s requested measurement window |
| Data | Fixed-seed JSONL with random lowercase message text |

Fixtures use the workspace disk through `TMPDIR` and normal OS caching. No other
builds or stress tests ran during measurement. Archive scheduling jitter is
disabled, so timings measure processing rather than the 0–30 minute waits.

Run from the workspace root:

```bash
mkdir -p target/logging-validation/tmp
export TMPDIR="$PWD/target/logging-validation/tmp"
cargo bench -p service-daemon --features file-logging --bench framework --offline -- file_logging --sample-size 20 --warm-up-time 1 --measurement-time 2 --noplot
cargo test -p service-daemon --release --features file-logging --lib logging_performance --offline -- --ignored --nocapture
cargo test -p service-daemon --release --features file-logging --lib core::logging::benchmarks::logging_scale_100mib --offline -- --ignored --exact --nocapture
cargo test -p service-daemon --release --features file-logging --lib logging_scale_100mib_and_1gib --offline -- --ignored --nocapture
```

Fixture code: `service-daemon/src/core/logging/benchmarks.rs`. Local logs are in
`target/logging-validation/`; Criterion estimates are in `target/criterion/`.

## Write and compression costs

| Case | Time estimate | Confidence interval | Throughput |
|---|---:|---:|---:|
| About 1MiB, time rotation only | 1.7221ms | 1.6874–1.7601ms | 580.79MiB/s |
| Same writes, plus 100MiB size check | 1.7327ms | 1.6899–1.7808ms | 577.22MiB/s |
| One close/sync/rename/reopen rotation | 222.32ms | 185.51–274.00ms | — |
| About 1MiB, gzip level 1 | 4.9486ms | 4.9334–4.9705ms | 202.11MiB/s |
| Same data, gzip level 6 | 24.914ms | 24.707–25.165ms | 40.145MiB/s |
| Same data, gzip level 9 | 26.075ms | 25.877–26.286ms | 38.357MiB/s |

The size check increased the central write-time estimate by 0.62%; the confidence
intervals overlap. This case writes complete records through the real writer to
the OS page cache. Setup, teardown, tracing serialization, queueing, rotation and
final sync are excluded.

The rotation case includes state-file publication and filesystem syncs on a small
file. Rotation latency for a dirty 100MiB file was not measured. Gzip cases encode
into memory and exclude decompression and disk I/O.

## Archive processing

The pipeline case repeats a seeded ~1MiB block eight times, then processes the
result through fragment, daily and monthly compression. Timing includes output
publication and source cleanup. A streaming SHA-256 comparison checks that the
decoded output matches the input.

| Measurement | Result |
|---|---:|
| Input | 8,389,920 bytes |
| Monthly output | 4,782,288 bytes (57.000% of input) |
| Processing time | 1,827ms (~4.38MiB/s of input) |
| Scheduler steps | 425 |
| Monthly volumes | 1 |
| Process peak RSS / current RSS | 8,364KiB / 8,124KiB |

RSS was read after output verification and before the in-memory compression
comparisons. It includes the test process, corpus and decoder. Peak RSS for the
multi-GiB case was not measured.

Separate encodes of the same 8,389,920-byte input:

| Level | Time | Compressed bytes | Percentage of input |
|---|---:|---:|---:|
| 1 | 42ms | 4,513,325 | 53.795% |
| 6 | 207ms | 4,783,083 | 57.010% |
| 9 | 211ms | 4,782,288 | 57.000% |

Level 1 produced smaller output on this synthetic data; levels 6 and 9 were
nearly equal. Higher levels spend more encoder effort but do not always reduce
size. Use deployment logs to estimate storage savings.

## Full-size fixtures

| Fixture | Time | Checks |
|---|---:|---|
| 100MiB rotation | 1.60s | Closed fragment is exactly 104,857,600 bytes. |
| 1GiB monthly target | 346.43s | About 2GiB of input produces at least two independently readable volumes; decoded byte count matches input. |

Small fixtures check ordered content, line boundaries and recovery. See
[release validation](release-validation.md) for commands and platform coverage.
These measurements are from Linux; Windows handle and rename behavior requires
Windows MSVC CI.

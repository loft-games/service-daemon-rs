use std::hint::black_box;

use criterion::{BenchmarkId, Criterion};

use crate::core::service_daemon::high_priority::policy_bench::PolicyFixture;

pub(super) fn run(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("high_priority_policy");
    for (name, active, standard_history) in [
        ("idle", 0, 0),
        ("active_1", 1, 0),
        ("active_32", 32, 0),
        ("active_256", 256, 0),
        ("active_1_standard_history_256x8", 1, 256),
    ] {
        let mut fixture = PolicyFixture::new(active, standard_history);
        for (filter, level) in [
            ("info", tracing::Level::INFO),
            ("debug", tracing::Level::DEBUG),
        ] {
            // Formatting to a discard sink isolates synchronous tracing work. It
            // excludes the daemon's async log queues, timestamps and filesystem I/O.
            let subscriber = tracing_subscriber::fmt()
                .without_time()
                .with_ansi(false)
                .with_max_level(level)
                .with_writer(std::io::sink)
                .finish();
            tracing::subscriber::with_default(subscriber, || {
                group.bench_function(BenchmarkId::new(name, filter), |b| {
                    // Includes the whole synchronous evaluator, but no mutex acquisition.
                    b.iter(|| black_box(&mut fixture).tick());
                });
            });
            fixture.verify();
        }
    }
    group.finish();
}

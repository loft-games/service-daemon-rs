//! Benchmark-only fixtures and groups; never compiled into the library.
mod observation;
#[cfg(feature = "high-priority")]
mod policy;
mod provider_resolve;
mod snapshot;

pub(super) fn run(criterion: &mut criterion::Criterion) {
    observation::run(criterion);
    snapshot::run(criterion);
    #[cfg(feature = "file-logging")]
    crate::core::logging::benchmarks::run(criterion);
    provider_resolve::run(criterion);
    #[cfg(feature = "high-priority")]
    policy::run(criterion);
}

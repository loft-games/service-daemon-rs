//! Benchmark-only fixtures and groups; never compiled into the library.
mod observation;
mod provider_resolve;
mod snapshot;

pub(super) fn run(criterion: &mut criterion::Criterion) {
    observation::run(criterion);
    snapshot::run(criterion);
    provider_resolve::run(criterion);
}

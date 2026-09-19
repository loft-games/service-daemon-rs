//! Shared crate for the provider-contract example.
//!
//! This crate owns the injectable contract type and a service that consumes it.
//! The runnable app crate supplies the concrete provider implementations.

use service_daemon::{done, provider_contract, service};
use std::sync::atomic::{AtomicUsize, Ordering};
use tracing::info;

static SERVICE_RUNS: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Debug, PartialEq, Eq)]
#[provider_contract]
pub struct SharedSettings {
    source: &'static str,
}

impl SharedSettings {
    pub const fn new(source: &'static str) -> Self {
        Self { source }
    }

    pub const fn source(&self) -> &'static str {
        self.source
    }
}

/// Returns how many times the example service has observed a resolved contract.
pub fn service_run_count() -> usize {
    SERVICE_RUNS.load(Ordering::SeqCst)
}

/// Resets the example observation counter for integration tests.
pub fn reset_service_run_count() {
    SERVICE_RUNS.store(0, Ordering::SeqCst);
}

#[service(tags = ["provider-contract-example"])]
pub async fn settings_consumer(settings: std::sync::Arc<SharedSettings>) -> anyhow::Result<()> {
    SERVICE_RUNS.fetch_add(1, Ordering::SeqCst);
    info!(
        source = settings.source(),
        "Resolved shared provider contract from app-local implementation"
    );
    done();
    Ok(())
}

use crate::models::{PROVIDER_REGISTRY, ProviderInitError};

use super::DaemonInstanceInner;
use super::provider_graph::validate_dependency_graph;
use super::runtime::{PreparedRuntimes, RuntimePreparationError};

pub(super) enum StartupPreflightError {
    ProviderGraph(ProviderInitError),
    EagerProviderInit(ProviderInitError),
    Runtime(RuntimePreparationError),
}

impl DaemonInstanceInner {
    pub(super) async fn run_startup_preflight(
        &mut self,
    ) -> Result<PreparedRuntimes, StartupPreflightError> {
        if let Err(err) = validate_dependency_graph(&self.services, PROVIDER_REGISTRY.iter()) {
            return Err(StartupPreflightError::ProviderGraph(err));
        }

        if let Err(err) = self.eager_init_reachable_providers().await {
            return Err(StartupPreflightError::EagerProviderInit(err));
        }

        self.prepare_startup_runtimes()
            .map_err(StartupPreflightError::Runtime)
    }
}

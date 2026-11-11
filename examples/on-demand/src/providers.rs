//! Providers for the on-demand service instance example.

use service_daemon::{ProviderError, ServiceHandle, provider, service_handle};

#[derive(Clone)]
pub struct WorkerService(ServiceHandle);

impl From<ServiceHandle> for WorkerService {
    fn from(service: ServiceHandle) -> Self {
        Self(service)
    }
}

impl std::ops::Deref for WorkerService {
    type Target = ServiceHandle;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[provider]
pub fn worker_service() -> Result<WorkerService, ProviderError> {
    match service_handle!(crate::services::template::worker) {
        Ok(handle) => Ok(handle.into()),
        Err(error) => Err(error),
    }
}

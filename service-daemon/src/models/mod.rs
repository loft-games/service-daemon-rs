pub mod error;
pub mod service;
pub mod trigger;

pub use error::{Result, ServiceError};
pub use service::{
    Registry, RegistryBuilder, SERVICE_REGISTRY, ServiceDescription, ServiceEntry, ServiceFn,
    ServiceId, ServiceParam, ServicePriority, ServiceStatus,
};
pub use trigger::{TT, TriggerTemplate};

pub mod service;
pub mod trigger;

pub use service::{
    SERVICE_REGISTRY, ServiceDescription, ServiceEntry, ServiceFn, ServiceParam, ServicePriority,
};
pub use trigger::{TT, TriggerTemplate};

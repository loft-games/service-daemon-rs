//! Pass case: A provider struct with field-based DI compiles successfully.

use service_daemon::provider;
use std::sync::Arc;

#[derive(Clone)]
#[provider(8080)]
pub struct Port(pub i32);

#[derive(Clone)]
#[provider("localhost")]
pub struct Host(pub String);

/// A composite provider that depends on other providers via Arc fields.
#[derive(Clone)]
#[provider]
pub struct AppConfig {
    pub port: Arc<Port>,
    pub host: Arc<Host>,
}

fn main() {}

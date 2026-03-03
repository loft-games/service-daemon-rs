//! Pass case: An async fn provider with Arc parameter dependencies compiles
//! successfully and generates correct DI resolution code.
//!
//! This test guards against regression of P1 (async fn provider parameter injection).

use service_daemon::provider;
use std::sync::Arc;

/// Base provider: a simple port value.
#[derive(Clone)]
#[provider(8080)]
pub struct Port(pub i32);

/// Base provider: a simple host string.
#[derive(Clone)]
#[provider("localhost")]
pub struct Host(pub String);

/// A composite config type assembled from other providers via async fn injection.
#[derive(Clone)]
pub struct CompositeConfig {
    pub address: String,
}

/// Async fn provider with Arc parameter injection.
/// The framework should automatically resolve `Port` and `Host` before calling this.
#[provider]
pub async fn composite_config_provider(port: Arc<Port>, host: Arc<Host>) -> CompositeConfig {
    CompositeConfig {
        address: format!("{}:{}", host, port),
    }
}

/// A downstream consumer that depends on the async fn provider's output.
#[derive(Clone)]
#[provider]
pub struct FinalConfig {
    pub composite: Arc<CompositeConfig>,
}

fn main() {}

//! Pass case: A basic async service with Arc dependencies compiles successfully.

use service_daemon::{provider, service};

#[derive(Clone)]
#[provider(8080)]
pub struct Port(pub i32);

#[service]
pub async fn basic_service(port: Arc<Port>) -> anyhow::Result<()> {
    tracing::info!("Running on port {}", port);
    Ok(())
}

fn main() {}

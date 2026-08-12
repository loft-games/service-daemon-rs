//! # On-Demand Service Instance Example
//!
//! This example demonstrates:
//! - Marking a service definition with `#[service(auto_start = false)]`
//! - Resolving a daemon-bound `ServiceHandle` from a provider
//! - Creating runtime instances with `ServiceHandle::create().await`
//! - Starting created instances with `ServiceInstanceHandle::start().await`
//! - Stopping and cleaning instances with `ServiceInstanceHandle`
//!
//! **Run**: `RUST_LOG=info cargo run -p example-on-demand`

use example_on_demand as _;
use service_daemon::{Registry, ServiceDaemon};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    service_daemon::init_logging();

    let daemon = ServiceDaemon::builder()
        .with_registry(Registry::builder().with_tag("on-demand").build())
        .build();
    daemon.run().await;
    daemon.wait().await?;

    Ok(())
}

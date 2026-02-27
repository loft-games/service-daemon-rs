//! Pass case: A service with priority and tags compiles successfully.

use service_daemon::{provider, service};
use std::sync::Arc;

#[derive(Clone)]
#[provider(default = "localhost:5432")]
pub struct DbHost(pub String);

#[service(priority = 80, tags = ["infra", "database"])]
pub async fn tagged_service(host: Arc<DbHost>) -> anyhow::Result<()> {
    tracing::info!("DB host: {}", host.0);
    Ok(())
}

fn main() {}

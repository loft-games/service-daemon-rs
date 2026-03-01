//! Pass case: A service with priority and tags compiles successfully.

use service_daemon::{provider, service};

#[derive(Clone)]
#[provider("localhost:5432")]
pub struct DbHost(pub String);

#[service(priority = 80, tags = ["infra", "database"])]
pub async fn tagged_service(host: Arc<DbHost>) -> anyhow::Result<()> {
    tracing::info!("DB host: {}", host);
    Ok(())
}

fn main() {}

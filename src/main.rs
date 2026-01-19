mod providers;
mod services;

use service_daemon::ServiceDaemon;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 1. Setup Logging
    tracing_subscriber::fmt::init();

    // 2. Verify dependencies (only when 'macros' feature is enabled)
    service_daemon::verify_setup!();

    // 3. Initialize Daemon (auto-runs providers, then registers services)
    let daemon = ServiceDaemon::auto_init();

    // 4. Run Daemon
    daemon.run().await?;

    Ok(())
}

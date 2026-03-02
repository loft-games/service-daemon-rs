//! # Stress Test -- Real Framework Pipeline
//!
//! This example registers scaling services via the standard `#[service]` macro,
//! exercising the full framework pipeline:
//! - linkme static registration
//! - Registry discovery and ID allocation
//! - Wave-based startup
//! - StatusPlane tracking
//! - Reload signal pre-allocation
//!
//! **Run**: `cargo run -p example-stress --release --features s100`
//!
//! Use `--features s0` to measure the framework baseline (zero business services).

use example_stress as _;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    #[cfg(not(feature = "s0"))]
    {
        println!("No stress features enabled (s0..s1000). Skipping real framework pipeline.");
        Ok(())
    }

    #[cfg(feature = "s0")]
    {
        run_stress_test().await
    }
}

#[cfg(feature = "s0")]
async fn run_stress_test() -> anyhow::Result<()> {
    use service_daemon::ServiceDaemon;
    // Minimal tracing setup (suppress noisy output for benchmarking)
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .init();

    let mut daemon = ServiceDaemon::builder().build();
    daemon.run().await;
    daemon.wait().await?;

    Ok(())
}

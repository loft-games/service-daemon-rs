// Bootstrap variants for a service-daemon-rs binary. Pick one shape for main().
use service_daemon::{Registry, RestartPolicy, ServiceDaemon, ServicePriority};
use std::num::NonZeroUsize;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

// 1. Minimal: run every discovered service, block until a signal.
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let daemon = ServiceDaemon::builder().build();
    daemon.run().await; // non-blocking: brings services up wave by wave
    daemon.wait().await?; // blocks until SIGINT / SIGTERM / Ctrl+C
    Ok(())
}

// 2. Tag-filtered subset: one binary, several deployment shapes.
#[allow(dead_code)]
async fn run_web_tier() -> anyhow::Result<()> {
    let registry = Registry::builder()
        .with_tag("web") // include web ...
        .with_tag("worker") // ... OR worker (union)
        .exclude_tag("debug") // minus debug-only services
        .build();

    let daemon = ServiceDaemon::builder().with_registry(registry).build();
    daemon.run().await;
    daemon.wait().await?;
    Ok(())
}

// 3. Custom supervision: bound provider-init retries, cap isolated startup.
#[allow(dead_code)]
async fn run_with_policy() -> anyhow::Result<()> {
    let policy = RestartPolicy::builder()
        .provider_init_timeout(Duration::from_secs(30))
        .build();

    let daemon = ServiceDaemon::builder()
        .with_restart_policy(policy)
        .with_isolated_startup_concurrency_limit(NonZeroUsize::new(2).unwrap_or(NonZeroUsize::MIN))
        .build();
    daemon.run().await;
    daemon.wait().await?;
    Ok(())
}

// 4. External shutdown: drive teardown from your own task, not just a signal.
#[allow(dead_code)]
async fn run_with_token() -> anyhow::Result<()> {
    let token = CancellationToken::new();
    let daemon = ServiceDaemon::builder()
        .with_cancel_token(token.clone())
        .build();
    daemon.run().await;

    // e.g. an admin endpoint elsewhere calls token.cancel() to stop the daemon.
    let trigger = token.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(60)).await;
        trigger.cancel();
    });

    daemon.wait().await?; // returns once the token (or a signal) fires
    Ok(())
}

// Priority on a service controls wave order: high starts first, stops last.
//   #[service(priority = ServicePriority::STORAGE)]  // 80: up before edge listeners
//   #[service(priority = ServicePriority::EXTERNAL)] // 0: up last, down first
#[allow(dead_code)]
const _: u8 = ServicePriority::DEFAULT; // 50

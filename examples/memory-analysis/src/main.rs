//! # Memory Analysis -- Per-Service RSS Delta Measurement
//!
//! This binary measures the actual memory overhead per service by:
//! 1. Reading baseline RSS before starting any services
//! 2. Starting 100 services via the standard `ServiceDaemon` pipeline
//! 3. Waiting for stabilization (3 seconds)
//! 4. Reading RSS again and computing the per-service delta
//!
//! Additionally prints `std::mem::size_of` for core framework types.
//!
//! **Linux only**: Uses `/proc/self/statm` for RSS measurement.
//!
//! **Run**: `cargo run -p example-memory-analysis --release`

use example_memory_analysis as _;
use service_daemon::ServiceDaemon;

use std::io::Read;

const SERVICE_COUNT: usize = 100;

/// Reads the current process RSS from `/proc/self/statm`.
///
/// Returns RSS in **kilobytes**. The second field in `statm` is the
/// resident set size measured in pages; we multiply by the page size
/// (typically 4096 bytes on x86-64 Linux) and convert to KB.
fn read_rss_kb() -> u64 {
    let mut buf = String::new();
    std::fs::File::open("/proc/self/statm")
        .expect("Failed to open /proc/self/statm -- Linux only")
        .read_to_string(&mut buf)
        .expect("Failed to read /proc/self/statm");

    let rss_pages: u64 = buf
        .split_whitespace()
        .nth(1) // second field = RSS in pages
        .expect("Malformed /proc/self/statm")
        .parse()
        .expect("RSS field is not a number");

    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u64;
    rss_pages * page_size / 1024
}

/// Prints `std::mem::size_of` for core framework types.
///
/// These are **stack-only** sizes and do not include heap allocations,
/// but they serve as a useful lower bound for understanding the overhead.
fn print_struct_sizes() {
    use service_daemon::core::context::{DaemonResources, ServiceIdentity};
    use service_daemon::models::{
        BackoffController, RestartPolicy, ServiceDescription, ServiceId, ServiceStatus,
    };

    println!("\n=== Stack Sizes (std::mem::size_of) ===");
    println!(
        "  ServiceId:          {:>4} bytes",
        std::mem::size_of::<ServiceId>()
    );
    println!(
        "  ServiceStatus:      {:>4} bytes",
        std::mem::size_of::<ServiceStatus>()
    );
    println!(
        "  ServiceDescription: {:>4} bytes",
        std::mem::size_of::<ServiceDescription>()
    );
    println!(
        "  RestartPolicy:      {:>4} bytes",
        std::mem::size_of::<RestartPolicy>()
    );
    println!(
        "  BackoffController:  {:>4} bytes",
        std::mem::size_of::<BackoffController>()
    );
    println!(
        "  ServiceIdentity:    {:>4} bytes",
        std::mem::size_of::<ServiceIdentity>()
    );
    println!(
        "  DaemonResources:    {:>4} bytes",
        std::mem::size_of::<DaemonResources>()
    );
    println!(
        "  CancellationToken:  {:>4} bytes",
        std::mem::size_of::<tokio_util::sync::CancellationToken>()
    );
    println!(
        "  Arc<DaemonRes>:     {:>4} bytes",
        std::mem::size_of::<std::sync::Arc<DaemonResources>>()
    );
    println!(
        "  JoinHandle<()>:     {:>4} bytes",
        std::mem::size_of::<tokio::task::JoinHandle<()>>()
    );
    println!();
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Minimal tracing setup (suppress noisy output)
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_target(false)
                .with_level(true),
        )
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    // Print struct sizes first (before any services are spawned)
    print_struct_sizes();

    // --- Phase 1: Baseline RSS ---
    let baseline_rss = read_rss_kb();
    println!("=== RSS Measurement ===");
    println!("  Baseline RSS (0 services): {} KB", baseline_rss);

    // --- Phase 2: Start services ---
    let mut daemon = ServiceDaemon::builder().build();
    daemon.run().await;

    // --- Phase 3: Wait for stabilization ---
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;

    // --- Phase 4: Measure post-startup RSS ---
    let loaded_rss = read_rss_kb();
    let delta = loaded_rss.saturating_sub(baseline_rss);
    let per_service = delta as f64 / SERVICE_COUNT as f64;

    println!(
        "  Loaded RSS ({} services): {} KB",
        SERVICE_COUNT, loaded_rss
    );
    println!("  Total delta: {} KB", delta);
    println!("  Per-service delta: {:.2} KB", per_service);
    println!();

    // --- Phase 5: Summary ---
    println!("=== Summary ===");
    println!(
        "  Each service adds approximately {:.1} KB of RSS overhead.",
        per_service
    );
    println!("  This includes: Supervisor state, DashMap slots, ServiceIdentity,",);
    println!("  Tokio task runtime, and indirect heap allocations (String, Arc, etc.).",);

    // Graceful shutdown
    daemon.shutdown();

    Ok(())
}

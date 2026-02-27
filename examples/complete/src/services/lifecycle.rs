//! Full lifecycle service examples using the `state()` match pattern.
//!
//! These services demonstrate advanced lifecycle management patterns:
//! - Explicit `state()` matching for every `ServiceStatus` variant
//! - Crash recovery with `shelve()` / `unshelve()` (Shelf persistence)
//! - Dependency-change-triggered restarts
//! - Priority-based startup/shutdown ordering
//! - Sync service support via `#[allow_sync]`
//!
//! > [!WARNING]
//! > Never mix `is_shutdown()` polling (from the minimal pattern) with
//! > `state()` lifecycle matching in the same service. These are two
//! > independent control-flow paradigms. The framework does NOT test
//! > or guarantee behavior when both are used together.

use crate::providers::typed_providers::{DbUrl, GlobalStats, Port};
use service_daemon::prelude::*;
use service_daemon::{allow_sync, done, service, shelve, sleep, state, unshelve};
use tracing::{error, info, warn};

// =============================================================================
// 1. Advanced Lifecycle Pattern (Pure state() loop)
// =============================================================================

/// Demonstrates explicit handling of every `ServiceStatus` variant.
///
/// This is the recommended pattern for services that need to:
/// - Perform cleanup before reload
/// - Initialize differently on first start vs. recovery
/// - Handle shutdown with custom finalization logic
#[service]
pub async fn advanced_lifecycle_service() -> anyhow::Result<()> {
    loop {
        match state() {
            ServiceStatus::Initializing
            | ServiceStatus::Recovering(_)
            | ServiceStatus::Restoring => {
                info!("[Lifecycle] Initializing/Recovering context...");
                done(); // Transition to Healthy
            }
            ServiceStatus::Healthy => {
                info!("[Lifecycle] Working in Healthy state.");
                if !sleep(std::time::Duration::from_secs(5)).await {
                    continue; // Interrupted -- re-check state
                }
            }
            ServiceStatus::NeedReload => {
                warn!("[Lifecycle] Performing cleanup before reload...");
                done(); // Acknowledge reload
                break;
            }
            ServiceStatus::ShuttingDown => {
                info!("[Lifecycle] Final cleanup before shutdown...");
                break;
            }
            _ => break,
        }
    }
    Ok(())
}

// =============================================================================
// 2. Crash Recovery Pattern (Pure state() with shelve/unshelve)
// =============================================================================

/// Demonstrates persistent state across crashes via `shelve()`/`unshelve()`.
///
/// Uses the `loop + match state()` pattern throughout. The `Recovering` arm
/// restores the iteration counter from the shelf.
///
/// To test crash recovery, uncomment the panic line and observe the logs.
#[service]
pub async fn recovery_service(port: Arc<Port>) -> anyhow::Result<()> {
    let mut iteration = 0u32;

    loop {
        match state() {
            ServiceStatus::Initializing => {
                info!("[Recovery] Starting fresh on port {}", port);
                iteration = 0;
                done();
            }
            ServiceStatus::Recovering(err) => {
                warn!("[Recovery] RECOVERING from crash: {}", err);
                iteration = unshelve::<u32>("iteration").await.unwrap_or(0);
                done();
            }
            ServiceStatus::Restoring => {
                info!("[Recovery] Restoring context...");
                iteration = unshelve::<u32>("iteration").await.unwrap_or(0);
                done();
            }
            ServiceStatus::Healthy => {
                iteration += 1;
                info!("[Recovery] Iteration {}", iteration);

                // Persist state before potential failure
                shelve("iteration", iteration).await;

                // Uncomment to simulate a crash on the 3rd iteration:
                // if iteration == 3 {
                //     panic!("Simulated crash on iteration 3!");
                // }

                if !sleep(std::time::Duration::from_secs(2)).await {
                    continue;
                }
            }
            ServiceStatus::NeedReload => {
                warn!("[Recovery] Shelving state before reload: {}", iteration);
                shelve("iteration", iteration).await;
                done();
                break;
            }
            ServiceStatus::ShuttingDown => {
                info!("[Recovery] Exiting at iteration {}", iteration);
                break;
            }
            _ => break,
        }
    }
    Ok(())
}

// =============================================================================
// 3. Shared Mutable State Pattern (Pure state() loop)
// =============================================================================

/// Demonstrates writing to shared state via `Arc<RwLock<GlobalStats>>`.
///
/// The daemon automatically promotes `GlobalStats` into managed state
/// when any service requests it wrapped in `RwLock`.
#[service]
pub async fn stats_writer(stats: Arc<RwLock<GlobalStats>>) -> anyhow::Result<()> {
    info!("[Stats Writer] Service started");

    loop {
        match state() {
            ServiceStatus::Initializing | ServiceStatus::Restoring => {
                done();
            }
            ServiceStatus::Healthy => {
                {
                    let mut guard = stats.write().await;
                    guard.total_processed += 1;
                    guard.last_status = format!("Processed {}", guard.total_processed);
                    info!("[Stats Writer] Updated: {}", guard.total_processed);
                } // Guard dropped -- triggers any Watch triggers on GlobalStats

                if !sleep(std::time::Duration::from_secs(5)).await {
                    continue;
                }
            }
            ServiceStatus::ShuttingDown => {
                info!("[Stats Writer] Shutting down");
                break;
            }
            _ => break,
        }
    }
    Ok(())
}

// =============================================================================
// 4. Dependency-Injection Restart Demo
// =============================================================================

/// Demonstrates automatic service restart when an injected dependency changes.
///
/// This service depends on `GlobalStats`. When `stats_writer` modifies it,
/// the daemon automatically restarts this service. The `Restoring` state
/// indicates a dependency-change-triggered restart (not a crash).
#[service]
pub async fn di_restart_demo(stats: Arc<GlobalStats>) -> anyhow::Result<()> {
    loop {
        match state() {
            ServiceStatus::Initializing => {
                info!(
                    "[DI Restart] Initializing with stats: {}",
                    stats.total_processed
                );
                done();
            }
            ServiceStatus::Restoring => {
                info!("[DI Restart] Dependency (GlobalStats) changed! Warm restart...");
                done();
            }
            ServiceStatus::Healthy => {
                info!("[DI Restart] Active and watching for changes...");
                if !sleep(std::time::Duration::from_secs(1)).await {
                    continue;
                }
            }
            ServiceStatus::ShuttingDown => {
                info!("[DI Restart] Shutting down");
                break;
            }
            _ => break,
        }
    }
    Ok(())
}

// =============================================================================
// 5. Reloading Counter (state preservation across reloads)
// =============================================================================

/// Demonstrates how a service preserves internal state (counters) across
/// configuration reloads using `shelve()`/`unshelve()` and `NeedReload`.
#[service]
pub async fn reloading_counter_service(stats: Arc<GlobalStats>) -> anyhow::Result<()> {
    let mut count = unshelve::<u32>("counter").await.unwrap_or(0);
    info!("[Counter] Started. Current count: {}", count);

    loop {
        match state() {
            ServiceStatus::Initializing | ServiceStatus::Restoring => {
                done();
            }
            ServiceStatus::Healthy => {
                count += 1;
                info!(
                    "[Counter] Working... count = {} (Global Stats: {})",
                    count, stats.total_processed
                );

                if !sleep(std::time::Duration::from_secs(3)).await {
                    continue;
                }
            }
            ServiceStatus::NeedReload => {
                warn!("[Counter] Shelving state before reload: {}", count);
                shelve("counter", count).await;
                done();
                break;
            }
            ServiceStatus::ShuttingDown => {
                info!("[Counter] Final count: {}", count);
                break;
            }
            _ => break,
        }
    }
    Ok(())
}

// =============================================================================
// 6. Priority-Based Services (Pure state() loop)
// =============================================================================

/// SYSTEM priority (100): Starts first, shuts down last.
/// Ideal for infrastructure services (log flusher, metrics collector).
#[service(priority = ServicePriority::SYSTEM)]
pub async fn system_service() -> anyhow::Result<()> {
    info!("[SYSTEM] Infrastructure service started (Priority 100)");
    loop {
        match state() {
            ServiceStatus::Initializing => {
                done();
            }
            ServiceStatus::Healthy => {
                if !sleep(std::time::Duration::from_secs(1)).await {
                    continue;
                }
            }
            ServiceStatus::ShuttingDown => {
                info!("[SYSTEM] Infrastructure service exiting LAST");
                break;
            }
            _ => break,
        }
    }
    Ok(())
}

/// STORAGE priority (80): Starts after SYSTEM, shuts down before SYSTEM.
#[service(priority = ServicePriority::STORAGE)]
pub async fn storage_service(db_url: Arc<DbUrl>) -> anyhow::Result<()> {
    info!(
        "[STORAGE] DB connector started (Priority 80) for {}",
        db_url
    );
    loop {
        match state() {
            ServiceStatus::Initializing => {
                done();
            }
            ServiceStatus::Healthy => {
                if !sleep(std::time::Duration::from_secs(1)).await {
                    continue;
                }
            }
            ServiceStatus::ShuttingDown => {
                info!("[STORAGE] DB connector exiting");
                break;
            }
            _ => break,
        }
    }
    Ok(())
}

/// EXTERNAL priority (0): Starts last, shuts down first.
/// Ideal for user-facing API endpoints.
#[service(priority = ServicePriority::EXTERNAL)]
pub async fn external_service(port: Arc<Port>) -> anyhow::Result<()> {
    info!(
        "[EXTERNAL] Public API started (Priority 0) on port {}",
        port
    );
    loop {
        match state() {
            ServiceStatus::Initializing => {
                done();
            }
            ServiceStatus::Healthy => {
                if !sleep(std::time::Duration::from_secs(1)).await {
                    continue;
                }
            }
            ServiceStatus::ShuttingDown => {
                info!("[EXTERNAL] Public API exiting FIRST");
                break;
            }
            _ => break,
        }
    }
    Ok(())
}

// =============================================================================
// 7. Fatal Error Pattern (Pure state() loop)
// =============================================================================

/// Demonstrates permanent service termination on unrecoverable errors.
///
/// When `ServiceError::Fatal` is returned, the daemon does NOT restart
/// this service -- it is marked as permanently failed.
#[service]
pub async fn fatal_error_demo() -> anyhow::Result<()> {
    info!("[Fatal] Service started, checking config...");

    // Simulate a config validation that might fail
    let is_config_valid = std::env::var("DEMO_INVALID_CONFIG").is_err();

    if !is_config_valid {
        error!("[Fatal] UNRECOVERABLE error. Stopping permanently.");
        return Err(service_daemon::models::ServiceError::Fatal(
            "Configuration is invalid and cannot be recovered".into(),
        )
        .into());
    }

    info!("[Fatal] Config is valid, proceeding normally.");
    loop {
        match state() {
            ServiceStatus::Initializing => {
                done();
            }
            ServiceStatus::Healthy => {
                if !sleep(std::time::Duration::from_secs(10)).await {
                    continue;
                }
                info!("[Fatal] Heartbeat");
            }
            ServiceStatus::ShuttingDown => break,
            _ => break,
        }
    }

    Ok(())
}

// =============================================================================
// 8. Sync Service (via #[allow_sync])
// =============================================================================

/// Demonstrates that synchronous (blocking) functions can also be services.
///
/// `#[allow_sync]` wraps this function in `spawn_blocking`, so it runs
/// on a dedicated OS thread rather than the async executor.
///
/// **Key point**: A sync service must still call `done()` to signal
/// readiness, and must park/block to avoid immediate exit (which the
/// daemon would treat as a crash and restart endlessly).
#[allow_sync]
#[service]
pub fn sync_service(port: Arc<Port>) -> anyhow::Result<()> {
    info!("[Sync] Sync service started on port {}", port);
    done(); // Signal ready -- without this, the daemon won't proceed to the next wave.

    // Block the thread until the daemon signals shutdown.
    // `is_shutdown()` is checked periodically with a small sleep to avoid busy-wait.
    while !service_daemon::is_shutdown() {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    info!("[Sync] Sync service shutting down");
    Ok(())
}

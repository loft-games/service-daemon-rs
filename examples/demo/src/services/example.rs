use crate::providers::trigger_providers::{TaskQueue, UserNotifier};
use crate::providers::typed_providers::{DbUrl, GlobalStats, Port};
use service_daemon::prelude::*;
use service_daemon::{ServiceStatus, allow_sync, done, service, shelve, sleep, state, unshelve};
use tracing::{info, warn};

#[service]
pub async fn example_service(
    port: std::sync::Arc<Port>, // Now supports qualified paths!
    db_url: Arc<DbUrl>,
) -> anyhow::Result<()> {
    // TIP: Hover over 'Port' or 'DbUrl' above - doc hints are preserved!
    info!(
        "Example service running on port {} with DB {}",
        port, db_url
    );
    while !service_daemon::is_shutdown() {
        // Use interruptible sleep - returns false if interrupted
        if !sleep(std::time::Duration::from_secs(10)).await {
            break;
        }
        info!("Heartbeat from example service");

        // --- Template-Based Interaction ---

        // 1. Trigger a Signal (Event) from here
        UserNotifier::notify().await;

        // 2. Push to a Broadcast Queue
        let _ = TaskQueue::push("Message from service".to_owned()).await;
    }
    info!("Example service shutting down");
    Ok(())
}

#[service]
pub async fn stats_writer(stats: Arc<RwLock<GlobalStats>>) -> anyhow::Result<()> {
    info!("Stats writer service started");
    done(); // Signal initialization complete
    while !service_daemon::is_shutdown() {
        // Gain exclusive write access
        let mut guard = stats.write().await;
        guard.total_processed += 1;
        guard.last_status = format!("Processed {}", guard.total_processed);

        info!("Updated global stats: {}", guard.total_processed);

        // Drop guard before sleep
        drop(guard);

        sleep(std::time::Duration::from_secs(5)).await;
    }
    info!("Stats writer shutting down");
    Ok(())
}

#[allow_sync]
#[service]
pub fn sync_service(port: Arc<Port>) -> anyhow::Result<()> {
    // This is a synchronous service. It still works because the macro wraps it!
    // Note: Calling blocking code here would block the executor.
    info!("Sync service started on port {}", port);
    Ok(())
}

// --- External Modification Mock Service ---
// This service simulates an "external" callback or context that doesn't
// have the state injected as a parameter, but uses the new static methods.
#[service]
pub async fn external_status_updater() -> anyhow::Result<()> {
    info!("External status updater mock service started");
    done(); // Signal initialization complete
    let mut count = 0;

    while !service_daemon::is_shutdown() {
        // Wait 15 seconds between updates
        if !sleep(std::time::Duration::from_secs(15)).await {
            break;
        }

        count += 1;
        let msg = format!("External update #{}", count);

        // DIRECT MODIFICATION SYNTAX (Zero Overhead)
        let state_lock = crate::providers::trigger_providers::ExternalStatus::rwlock().await;
        {
            let mut guard = state_lock.write().await;
            guard.message = msg;
            guard.updated_count = count;
            info!("Pushed external update #{}", count);
        } // <--- Trigger fires here when guard is dropped!
    }
    Ok(())
}

// --- Priority Verification Examples ---

/// A service with SYSTEM priority (100).
/// This will be the FIRST to start and the LAST to shut down.
#[service(priority = ServicePriority::SYSTEM)]
pub async fn log_flusher() -> anyhow::Result<()> {
    info!("[SYSTEM] Log flusher started (Priority 100)");
    done(); // Signal initialization complete
    while !service_daemon::is_shutdown() {
        sleep(std::time::Duration::from_secs(1)).await;
    }
    info!("[SYSTEM] Log flusher exiting LAST");
    Ok(())
}

/// A service with STORAGE priority (80).
/// Starts after SYSTEM, shuts down before SYSTEM.
#[service(priority = ServicePriority::STORAGE)]
pub async fn db_connector(db_url: Arc<DbUrl>) -> anyhow::Result<()> {
    info!(
        "[STORAGE] DB connector started (Priority 80) for {}",
        db_url
    );
    done(); // Signal initialization complete
    while !service_daemon::is_shutdown() {
        sleep(std::time::Duration::from_secs(1)).await;
    }
    info!("[STORAGE] DB connector exiting (Reload or Shutdown)");
    Ok(())
}

/// A service with EXTERNAL priority (0).
/// This will be the LAST to start and the FIRST to shut down.
#[service(priority = ServicePriority::EXTERNAL)]
pub async fn public_api(port: Arc<Port>) -> anyhow::Result<()> {
    info!(
        "[EXTERNAL] Public API started (Priority 0) on port {}",
        port
    );
    done(); // Signal initialization complete
    while !service_daemon::is_shutdown() {
        sleep(std::time::Duration::from_secs(1)).await;
    }
    info!("[EXTERNAL] Public API exiting FIRST");
    Ok(())
}

// --- Service Control Plane Demonstrations ---

/// Demonstrates how a service can use the state() API to handle restarts and recoveries.
#[service]
pub async fn resilient_service_demo() -> anyhow::Result<()> {
    match state() {
        ServiceStatus::Initializing => {
            info!("Resilient service starting for the FIRST time.");
        }
        ServiceStatus::Restoring => {
            info!("Resilient service RESTORING (warm restart).");
        }
        ServiceStatus::Recovering(last_error) => {
            warn!("Resilient service RECOVERING from a crash!");
            warn!("Context from previous failure: {}", last_error);
        }
        _ => {
            // Uniformly handle other states (like Healthy) if no special logic is needed
            done();
        }
    }

    // Simulate work using interruptible sleep
    sleep(std::time::Duration::from_secs(5)).await;
    Ok(())
}

/// Demonstrates cross-generational state handoff using shelving.
#[service]
pub async fn state_handoff_demo() -> anyhow::Result<()> {
    // 1. Try to unshelve data from the previous generation
    if let Some(history) = unshelve::<Vec<String>>("history").await {
        info!("Retrieved {} historical events from shelf", history.len());
        for event in history {
            info!("  - {}", event);
        }
    } else {
        info!("No historical data found in shelf (first run or empty).");
    }

    // 2. Perform some work
    let event = format!("Event at {:?}", std::time::SystemTime::now());

    // 3. Shelve state for the next generation (e.g., if we were to restart)
    shelve("history", vec![event]).await;

    tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
    Ok(())
}

/// Demonstrates automatic service restart when an injected dependency changes.
/// This service depends on GlobalStats. When stats_writer updates GlobalStats,
/// this service will be automatically restarted by the daemon.
#[service]
pub async fn di_restart_demo(stats: Arc<GlobalStats>) -> anyhow::Result<()> {
    match state() {
        ServiceStatus::Restoring => {
            info!("DI Restart Demo: Dependency (GlobalStats) changed! Performing warm restart...");
        }
        _ => {
            info!(
                "DI Restart Demo: Initializing with stats: {}",
                stats.total_processed
            );
            done();
        }
    }

    info!("DI Restart Demo is now active and watching for changes...");
    while !service_daemon::is_shutdown() {
        sleep(std::time::Duration::from_secs(1)).await;
    }
    Ok(())
}

/// A comprehensive example of a state-aware service.
/// It maintains an internal counter that survives dependency-triggered reloads.
/// This service uses the unified state() API to handle its lifecycle in a single loop.
#[service]
pub async fn reloading_counter_service(stats: Arc<GlobalStats>) -> anyhow::Result<()> {
    // 1. Restore state from previous generation if it exists
    let mut count = unshelve::<u32>("counter").await.unwrap_or(0);
    info!("Counter Service: Started. Current count: {}", count);

    // 2. Signal that initialization is complete
    done();

    // 3. Main Loop: Unified state handling
    while !service_daemon::is_shutdown() {
        match state() {
            ServiceStatus::NeedReload => {
                warn!(
                    "Counter Service: Reloading due to dependency change! Submitting count: {}",
                    count
                );
                // Save state for the next generation
                shelve("counter", count).await;
                done(); // Signal ready to exit
                break; // Graceful exit for warm restart
            }
            ServiceStatus::ShuttingDown => break,
            ServiceStatus::Recovering(err) => {
                warn!("Counter Service: Recovering from error: {}", err);
                // We can choose to reset or continue
                done();
            }
            _ => {
                // Normal Healthy state
                count += 1;
                info!(
                    "Counter Service: Working... count = {} (Global Stats: {})",
                    count, stats.total_processed
                );

                // Simulate work using interruptible sleep
                sleep(std::time::Duration::from_secs(3)).await;
            }
        }
    }
    info!("Counter Service: Shutting down. Final count was: {}", count);

    Ok(())
}

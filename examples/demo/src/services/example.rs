use crate::providers::trigger_providers::{TaskQueue, UserNotifier};
use crate::providers::typed_providers::{DbUrl, GlobalStats, Port};
use service_daemon::prelude::*;
use service_daemon::{ServiceStatus, allow_sync, done, service, shelve, sleep, state, unshelve};
use tracing::{error, info, warn};

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

// =============================================================================
// LIFECYCLE PATTERN EXAMPLES
// =============================================================================

// 1. The Simple Pattern:
// Ideal for 90% of services. Just use `while !is_shutdown()`.
// The framework handles the "Initializing -> Healthy" transition automatically.
// (See also: `example_service` at the top of this file.)

/// 2. The Advanced Pattern:
/// For services that need fine-grained control over every state transition.
///
/// This demonstrates explicit handling of each `ServiceStatus` state in a `loop + match` pattern.
#[service]
pub async fn advanced_lifecycle_demo() -> anyhow::Result<()> {
    loop {
        match state() {
            ServiceStatus::Initializing
            | ServiceStatus::Recovering(_)
            | ServiceStatus::Restoring => {
                info!("Advanced Service: Initializing/Restoring context...");
                done(); // Signal we are now Healthy
            }
            ServiceStatus::Healthy => {
                info!("Advanced Service: Working in Healthy state.");
                if !sleep(std::time::Duration::from_secs(5)).await {
                    continue;
                }
            }
            ServiceStatus::NeedReload => {
                warn!("Advanced Service: Custom cleanup before reload...");
                done();
                break;
            }
            ServiceStatus::ShuttingDown => {
                info!("Advanced Service: Final cleanup before total shutdown...");
                break;
            }
            _ => break,
        }
    }
    Ok(())
}

/// 3. Crash Recovery Pattern:
/// Demonstrates how a service can persist its state via `shelve()` and recover
/// it from the Shelf after a crash, using the `Recovering` state.
/// To test: Uncomment the panic line, run the demo, and observe the recovery.
#[service]
pub async fn recovery_service_demo() -> anyhow::Result<()> {
    // Restore state from shelf if recovering from a crash
    let mut iteration = match state() {
        ServiceStatus::Recovering(err) => {
            warn!("Recovery Demo: RECOVERING from crash: {}", err);
            unshelve::<u32>("iteration").await.unwrap_or(0)
        }
        _ => {
            info!("Recovery Demo: Starting fresh.");
            0
        }
    };
    done(); // Signal we are ready

    while !service_daemon::is_shutdown() {
        iteration += 1;
        info!("Recovery Demo: Iteration {}", iteration);

        // Persist state before potential failure
        shelve("iteration", iteration).await;

        // Simulate a crash on the 3rd iteration (uncomment to test)
        // if iteration == 3 {
        //     panic!("Simulated crash on iteration 3!");
        // }

        if !sleep(std::time::Duration::from_secs(2)).await {
            break;
        }
    }
    info!("Recovery Demo: Exiting at iteration {}", iteration);
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

/// Demonstrates how an advanced service uses the loop+match pattern to manage
/// its internal state (like counters) across reloads.
#[service]
pub async fn reloading_counter_service(stats: Arc<GlobalStats>) -> anyhow::Result<()> {
    // 1. Restore state from previous generation
    let mut count = unshelve::<u32>("counter").await.unwrap_or(0);
    info!("Counter Service: Started. Current count: {}", count);

    // 2. Main Loop: Explicit state handling
    loop {
        match state() {
            ServiceStatus::Initializing | ServiceStatus::Restoring => {
                done(); // Move to Healthy
            }
            ServiceStatus::Healthy => {
                count += 1;
                info!(
                    "Counter Service: Working... count = {} (Global Stats: {})",
                    count, stats.total_processed
                );

                if !sleep(std::time::Duration::from_secs(3)).await {
                    continue;
                }
            }
            ServiceStatus::NeedReload => {
                warn!("Counter Service: Shelving state before reload: {}", count);
                shelve("counter", count).await;
                done();
                break;
            }
            _ => break, // ShuttingDown or Terminated
        }
    }
    info!("Counter Service: Final count was: {}", count);
    Ok(())
}

/// 5. Fatal Error Pattern:
/// Demonstrates how a service can permanently stop itself when it encounters
/// an unrecoverable condition (e.g. invalid config, missing dependencies).
#[service]
pub async fn fatal_error_service_demo() -> anyhow::Result<()> {
    info!("Fatal Error Demo: Started.");

    // Simulate a check that might fail fatally
    let is_config_valid = std::env::var("DEMO_INVALID_CONFIG").is_err();

    if !is_config_valid {
        error!("Fatal Error Demo: UNRECOVERABLE error detected. Stopping permanently.");
        // Returning ServiceError::Fatal tells the daemon NOT to restart this service.
        return Err(service_daemon::models::ServiceError::Fatal(
            "Configuration is invalid and cannot be recovered".into(),
        )
        .into());
    }

    info!("Fatal Error Demo: Config is valid, proceeding normally.");
    done();

    while !service_daemon::is_shutdown() {
        sleep(std::time::Duration::from_secs(10)).await;
        info!("Fatal Error Demo: Heartbeat");
    }

    Ok(())
}

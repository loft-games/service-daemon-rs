//! A simple service that generates log output for the file logging demo.

use std::time::Duration;

use service_daemon::service;
use tracing::{info, warn};

/// Generates periodic log messages at different levels.
/// These appear both on stderr (console) and in the log file (JSON).
#[service]
pub async fn log_generator() -> anyhow::Result<()> {
    info!("[LogGenerator] Service started -- file logging is active");

    let mut tick = 0u32;
    while !service_daemon::is_shutdown() {
        tick += 1;

        if tick.is_multiple_of(3) {
            warn!("[LogGenerator] Warning at tick {}", tick);
        } else {
            info!("[LogGenerator] Heartbeat tick {}", tick);
        }

        if !service_daemon::sleep(Duration::from_secs(3)).await {
            break;
        }
    }

    info!("[LogGenerator] Shutting down");
    Ok(())
}

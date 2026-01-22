//! Trigger event source providers using Type-Based DI
//!
//! These wrapper types enable compile-time verification of trigger dependencies.

use service_daemon::provider;
use std::sync::Arc;
use tokio::sync::{Mutex, Notify, mpsc};

/// Wrapper for custom trigger notifier
/// (Manual impl since Notify doesn't implement Display)
pub struct UserNotifier(pub Arc<Notify>);

impl Default for UserNotifier {
    fn default() -> Self {
        Self(Arc::new(Notify::new()))
    }
}

impl service_daemon::Provided for UserNotifier {
    fn resolve() -> Arc<Self> {
        Arc::new(Self::default())
    }
}

impl std::ops::Deref for UserNotifier {
    type Target = Arc<Notify>;
    fn deref(&self) -> &Arc<Notify> {
        &self.0
    }
}

/// Wrapper for cron schedule string
#[provider(default = "*/30 * * * * *".to_string())]
pub struct CleanupSchedule(pub String);

/// Wrapper for queue receiver
#[provider(default = mpsc::channel(100))]
pub struct TaskQueue(pub Arc<Mutex<mpsc::Receiver<String>>>);

// TaskQueue needs special initialization, so we implement Provided manually
impl service_daemon::Provided for TaskQueue {
    fn resolve() -> Arc<Self> {
        let (tx, rx) = mpsc::channel(100);

        // Simulate work being added to the queue
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                let _ = tx.send("Auto-generated task".to_string()).await;
            }
        });

        Arc::new(Self(Arc::new(Mutex::new(rx))))
    }
}

impl std::ops::Deref for TaskQueue {
    type Target = Arc<Mutex<mpsc::Receiver<String>>>;
    fn deref(&self) -> &Arc<Mutex<mpsc::Receiver<String>>> {
        &self.0
    }
}

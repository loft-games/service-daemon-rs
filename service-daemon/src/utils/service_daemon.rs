use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::{error, info, warn};

use crate::models::{PROVIDER_REGISTRY, SERVICE_REGISTRY, ServiceDescription, ServiceFn};

pub struct ServiceDaemon {
    services: Vec<ServiceDescription>,
    running_tasks: Arc<Mutex<HashMap<String, JoinHandle<()>>>>,
}

impl ServiceDaemon {
    pub fn new() -> Self {
        Self {
            services: Vec::new(),
            running_tasks: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Automatically initialize the daemon by:
    /// 1. Running all auto-registered Providers (populates GLOBAL_CONTAINER)
    /// 2. Registering all auto-registered Services
    pub fn auto_init() -> Self {
        // 1. Initialize all providers
        info!(
            "Initializing {} providers from registry",
            PROVIDER_REGISTRY.len()
        );
        for provider in PROVIDER_REGISTRY {
            info!(
                "Initializing provider '{}' (type: {})",
                provider.name, provider.type_name
            );
            (provider.init)();
        }

        // 2. Load services
        Self::from_registry()
    }

    /// Create a new daemon with all services from the auto-generated registry.
    /// Services register themselves via the #[service] macro using linkme.
    /// NOTE: This does NOT initialize providers. Use auto_init() for full setup.
    pub fn from_registry() -> Self {
        let mut daemon = Self::new();

        for entry in SERVICE_REGISTRY {
            info!(
                "Registering service '{}' from module '{}' with {} params: {:?}",
                entry.name,
                entry.module,
                entry.params.len(),
                entry
                    .params
                    .iter()
                    .map(|p| p.container_key())
                    .collect::<Vec<_>>()
            );

            let wrapper = entry.wrapper;
            daemon.register(entry.name, Arc::new(move || wrapper()));
        }

        daemon
    }

    pub fn register(&mut self, name: &str, run: ServiceFn) {
        self.services.push(ServiceDescription {
            name: name.to_string(),
            run,
        });
    }

    pub async fn run(self) -> anyhow::Result<()> {
        let running_tasks = self.running_tasks.clone();

        for service in &self.services {
            let name = service.name.clone();
            let run = service.run.clone();
            let handle = tokio::spawn(async move {
                loop {
                    info!("Starting service: {}", name);
                    match run().await {
                        Ok(_) => warn!("Service {} exited normally", name),
                        Err(e) => error!("Service {} failed: {:?}", name, e),
                    }
                    warn!("Restarting service {} in 5 seconds...", name);
                    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                }
            });
            running_tasks
                .lock()
                .await
                .insert(service.name.clone(), handle);
        }

        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
        }
    }

    /// Run for a limited duration (for testing)
    #[allow(dead_code)]
    pub async fn run_for_duration(self, duration: std::time::Duration) -> anyhow::Result<()> {
        let running_tasks = self.running_tasks.clone();

        for service in &self.services {
            let name = service.name.clone();
            let run = service.run.clone();
            let handle = tokio::spawn(async move {
                loop {
                    info!("Starting service: {}", name);
                    match run().await {
                        Ok(_) => warn!("Service {} exited normally", name),
                        Err(e) => error!("Service {} failed: {:?}", name, e),
                    }
                    warn!("Restarting service {} in 1 second...", name);
                    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                }
            });
            running_tasks
                .lock()
                .await
                .insert(service.name.clone(), handle);
        }

        tokio::time::sleep(duration).await;

        let mut tasks = running_tasks.lock().await;
        for (name, handle) in tasks.drain() {
            info!("Stopping service: {}", name);
            handle.abort();
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;
    use tracing::debug;

    fn setup_tracing() {
        let _ = tracing_subscriber::fmt::try_init();
    }

    #[tokio::test]
    async fn test_service_daemon_new() {
        setup_tracing();
        let daemon = ServiceDaemon::new();
        assert!(daemon.services.is_empty());
        debug!("test_service_daemon_new passed");
    }

    #[tokio::test]
    async fn test_service_daemon_register() {
        setup_tracing();
        let mut daemon = ServiceDaemon::new();

        let service_fn: ServiceFn = Arc::new(|| Box::pin(async { Ok(()) }));

        daemon.register("test_service", service_fn);

        assert_eq!(daemon.services.len(), 1);
        assert_eq!(daemon.services[0].name, "test_service");
        debug!("test_service_daemon_register passed");
    }

    #[tokio::test]
    async fn test_service_runs_and_restarts_on_error() {
        setup_tracing();
        let mut daemon = ServiceDaemon::new();

        let call_count = Arc::new(AtomicU32::new(0));
        let call_count_clone = call_count.clone();

        let service_fn: ServiceFn = Arc::new(move || {
            let count = call_count_clone.clone();
            Box::pin(async move {
                count.fetch_add(1, Ordering::SeqCst);
                Err(anyhow::anyhow!("Intentional failure"))
            })
        });

        daemon.register("failing_service", service_fn);

        daemon
            .run_for_duration(Duration::from_secs(3))
            .await
            .unwrap();

        let final_count = call_count.load(Ordering::SeqCst);
        debug!("Service was called {} times", final_count);

        assert!(
            final_count >= 2,
            "Service should restart on failure, but only ran {} times",
            final_count
        );
        debug!("test_service_runs_and_restarts_on_error passed");
    }

    #[tokio::test]
    async fn test_service_runs_successfully() {
        setup_tracing();
        let mut daemon = ServiceDaemon::new();

        let executed = Arc::new(AtomicU32::new(0));
        let executed_clone = executed.clone();

        let service_fn: ServiceFn = Arc::new(move || {
            let exec = executed_clone.clone();
            Box::pin(async move {
                exec.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(100)).await;
                Ok(())
            })
        });

        daemon.register("successful_service", service_fn);

        daemon
            .run_for_duration(Duration::from_secs(2))
            .await
            .unwrap();

        let final_count = executed.load(Ordering::SeqCst);
        debug!("Service executed {} times", final_count);

        assert!(
            final_count >= 1,
            "Service should have executed at least once"
        );
        debug!("test_service_runs_successfully passed");
    }

    #[tokio::test]
    async fn test_multiple_services() {
        setup_tracing();
        let mut daemon = ServiceDaemon::new();

        let count_a = Arc::new(AtomicU32::new(0));
        let count_b = Arc::new(AtomicU32::new(0));

        let count_a_clone = count_a.clone();
        let service_a: ServiceFn = Arc::new(move || {
            let count = count_a_clone.clone();
            Box::pin(async move {
                count.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(50)).await;
                Ok(())
            })
        });

        let count_b_clone = count_b.clone();
        let service_b: ServiceFn = Arc::new(move || {
            let count = count_b_clone.clone();
            Box::pin(async move {
                count.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(50)).await;
                Ok(())
            })
        });

        daemon.register("service_a", service_a);
        daemon.register("service_b", service_b);

        daemon
            .run_for_duration(Duration::from_secs(2))
            .await
            .unwrap();

        let final_a = count_a.load(Ordering::SeqCst);
        let final_b = count_b.load(Ordering::SeqCst);

        debug!(
            "Service A executed {} times, Service B executed {} times",
            final_a, final_b
        );

        assert!(final_a >= 1, "Service A should have executed");
        assert!(final_b >= 1, "Service B should have executed");
        debug!("test_multiple_services passed");
    }
}

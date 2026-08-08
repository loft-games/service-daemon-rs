//! Integration smoke for the cross-platform local IPC example topology.

use example_local_ipc::providers::{EXAMPLE_LOCAL_IPC_ENV, ExampleLocalIpcListener};
use service_daemon::{ManagedProvided, ServiceDaemon};
use std::ffi::OsString;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

static IPC_COUNTER: AtomicU64 = AtomicU64::new(0);

struct EnvVarGuard {
    previous: Option<OsString>,
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        // Rust 2024 marks environment mutation unsafe because it is process-global.
        unsafe {
            if let Some(previous) = &self.previous {
                std::env::set_var(EXAMPLE_LOCAL_IPC_ENV, previous);
            } else {
                std::env::remove_var(EXAMPLE_LOCAL_IPC_ENV);
            }
        }
    }
}

fn set_example_ipc_name() -> EnvVarGuard {
    let counter = IPC_COUNTER.fetch_add(1, Ordering::Relaxed);
    let name = format!(
        "service-daemon-rs-local-ipc-example-{}-{counter}",
        std::process::id(),
    );
    let previous = std::env::var_os(EXAMPLE_LOCAL_IPC_ENV);
    // Rust 2024 marks environment mutation unsafe because it is process-global.
    unsafe {
        std::env::set_var(EXAMPLE_LOCAL_IPC_ENV, name);
    }
    EnvVarGuard { previous }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_ipc_example_starts_and_roundtrips() -> anyhow::Result<()> {
    let env_var = set_example_ipc_name();
    let name = std::env::var(EXAMPLE_LOCAL_IPC_ENV)?;
    let listener = <ExampleLocalIpcListener as ManagedProvided>::resolve_managed()
        .await
        .map_err(|error| anyhow::anyhow!("ExampleLocalIpcListener resolve failed: {error:?}"))?;
    assert_eq!(listener.name(), name);

    let mut daemon = ServiceDaemon::builder().build();
    daemon.run().await;

    tokio::time::sleep(Duration::from_secs(1)).await;
    daemon.shutdown();
    tokio::time::timeout(Duration::from_secs(5), daemon.wait()).await??;
    drop(env_var);
    Ok(())
}

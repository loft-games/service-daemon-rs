//! Integration smoke for the cross-platform local IPC example topology.

#[path = "../../../service-daemon/tests/support/provider_context.rs"]
mod provider_context;

use example_local_ipc::providers::{EXAMPLE_LOCAL_IPC_ENV, ExampleLocalIpcListener};
use service_daemon::ServiceDaemon;
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
    // Leave room for macOS's long per-user temporary directory in sockaddr_un.
    let name = format!("sde-{:x}-{counter:x}", std::process::id());
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
    let daemon = ServiceDaemon::builder().build();
    daemon.run().await;
    let listener = provider_context::published::<ExampleLocalIpcListener>(&daemon).await;
    assert_eq!(listener.name(), name);

    tokio::time::sleep(Duration::from_secs(1)).await;
    daemon.shutdown();
    tokio::time::timeout(Duration::from_secs(5), daemon.wait()).await??;
    drop(env_var);
    Ok(())
}

#[cfg(unix)]
#[test]
fn local_ipc_example_supports_long_temp_directory() -> anyhow::Result<()> {
    // A 50-byte base models macOS TMPDIR; the former verbose logical name
    // exceeded even Linux's larger Unix socket path limit under this base.
    let root = std::path::PathBuf::from(format!("/tmp/sd-ipc-{:0>38}", std::process::id()));
    std::fs::create_dir(&root)?;
    // Set the environment only in the child, before its Tokio workers start.
    let output = std::process::Command::new(std::env::current_exe()?)
        .args([
            "--exact",
            "local_ipc_example_starts_and_roundtrips",
            "--nocapture",
        ])
        .env("XDG_RUNTIME_DIR", "")
        .env("TMPDIR", &root)
        .output();
    std::fs::remove_dir_all(&root)?;
    let output = output?;
    assert!(
        output.status.success(),
        "Local IPC failed with a long TMPDIR:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    Ok(())
}

#[service_daemon::service]
async fn publish_listener(listener: std::sync::Arc<ExampleLocalIpcListener>) -> anyhow::Result<()> {
    provider_context::publish(&listener);
    service_daemon::done();
    service_daemon::wait_shutdown().await;
    Ok(())
}

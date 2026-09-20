//! Integration smoke for the Windows named pipe example topology.

#![cfg(windows)]

#[path = "../../../service-daemon/tests/support/provider_context.rs"]
mod provider_context;

use example_named_pipe::providers::{EXAMPLE_NAMED_PIPE_ENV, ExampleNamedPipeListener};
use service_daemon::ServiceDaemon;
use std::ffi::OsString;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

static PIPE_COUNTER: AtomicU64 = AtomicU64::new(0);

struct EnvVarGuard {
    previous: Option<OsString>,
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        // Rust 2024 marks environment mutation unsafe because it is process-global.
        unsafe {
            if let Some(previous) = &self.previous {
                std::env::set_var(EXAMPLE_NAMED_PIPE_ENV, previous);
            } else {
                std::env::remove_var(EXAMPLE_NAMED_PIPE_ENV);
            }
        }
    }
}

fn set_example_pipe_name() -> EnvVarGuard {
    let counter = PIPE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let pipe_name = format!(
        r"\\.\pipe\service-daemon-rs-named-pipe-example-{}-{counter}",
        std::process::id(),
    );
    let previous = std::env::var_os(EXAMPLE_NAMED_PIPE_ENV);
    // Rust 2024 marks environment mutation unsafe because it is process-global.
    unsafe {
        std::env::set_var(EXAMPLE_NAMED_PIPE_ENV, pipe_name);
    }
    EnvVarGuard { previous }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn named_pipe_example_starts_and_roundtrips() -> anyhow::Result<()> {
    let env_var = set_example_pipe_name();
    let pipe_name = std::env::var(EXAMPLE_NAMED_PIPE_ENV)?;
    let daemon = ServiceDaemon::builder().build();
    daemon.run().await;
    let listener = provider_context::published::<ExampleNamedPipeListener>(&daemon).await;
    assert_eq!(listener.name(), pipe_name);

    tokio::time::sleep(Duration::from_secs(1)).await;
    daemon.shutdown();
    tokio::time::timeout(Duration::from_secs(5), daemon.wait()).await??;
    drop(env_var);
    Ok(())
}

#[service_daemon::service]
async fn publish_listener(
    listener: std::sync::Arc<ExampleNamedPipeListener>,
) -> anyhow::Result<()> {
    provider_context::publish(&listener);
    service_daemon::done();
    service_daemon::wait_shutdown().await;
    Ok(())
}

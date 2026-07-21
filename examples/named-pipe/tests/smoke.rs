#![cfg(windows)]

use example_named_pipe::providers::EXAMPLE_NAMED_PIPE_ENV;
use service_daemon::{RestartPolicy, ServiceDaemon};
use std::ffi::OsString;
use std::time::Duration;

struct EnvVarGuard {
    key: &'static str,
    previous: Option<OsString>,
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        unsafe {
            if let Some(previous) = &self.previous {
                std::env::set_var(self.key, previous);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }
}

fn set_test_env(key: &'static str, value: &str) -> EnvVarGuard {
    let previous = std::env::var_os(key);
    unsafe {
        std::env::set_var(key, value);
    }
    EnvVarGuard { key, previous }
}

fn unique_pipe_name(name: &str) -> String {
    format!(
        r"\\.\pipe\service-daemon-rs-example-{name}-{}",
        std::process::id()
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn named_pipe_example_smoke() -> anyhow::Result<()> {
    use example_named_pipe as _;

    service_daemon::init_logging();
    let pipe_name = unique_pipe_name("smoke");
    let _env = set_test_env(EXAMPLE_NAMED_PIPE_ENV, &pipe_name);

    let mut daemon = ServiceDaemon::builder()
        .with_restart_policy(
            RestartPolicy::builder()
                .initial_delay(Duration::from_millis(1))
                .max_delay(Duration::from_millis(10))
                .jitter_factor(0.0)
                .provider_init_timeout(Duration::from_secs(5))
                .wave_spawn_timeout(Duration::from_secs(5))
                .wave_stop_timeout(Duration::from_secs(5))
                .build(),
        )
        .build();
    let cancel = daemon.cancel_token();

    daemon.run().await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(5), daemon.wait()).await??;

    Ok(())
}

#![cfg(feature = "file-logging")]

use std::{fs, process::Command, time::Duration};

use service_daemon::{FileLogConfig, Registry, ServiceDaemon, enable_file_logging};

const CHILD: &str = "SD_LOG_FILTER_TEST_DIR";
const TRACE_MARKER: &str = "wire_trace_marker";
const INFO_MARKER: &str = "wire_info_marker";
const DEBUG_MARKER: &str = "wire_debug_marker";

#[test]
fn default_and_explicit_filters_apply_to_both_outputs() -> anyhow::Result<()> {
    if let Some(directory) = std::env::var_os(CHILD) {
        enable_file_logging(FileLogConfig::new(directory.to_string_lossy(), "wire.log"));
        service_daemon::try_init_logging()?;
        return tokio::runtime::Runtime::new()?.block_on(async {
            let daemon = ServiceDaemon::builder()
                .with_registry(Registry::builder().with_tag("__log__").with_tag("__file_log__").build())
                .build();
            daemon.run().await;
            {
                let span = tracing::info_span!("wire_test", message_id = "019fe746-6158-7403-82c9-ac8b7e502cb3");
                let _entered = span.enter();
                tracing::trace!(target: "adapter_connection::wire", payload = %"AB".repeat(1024), "{TRACE_MARKER}");
                tracing::debug!(target: "adapter_connection::wire", "{DEBUG_MARKER}");
                tracing::info!(target: "adapter_connection::wire", "{INFO_MARKER}");
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
            daemon.shutdown();
            daemon.wait().await?;
            Ok(())
        });
    }
    for (filter, console_debug, file_debug, trace) in [
        (None, false, true, false),
        (Some("info"), false, false, false),
        (Some("debug"), true, true, false),
        (
            Some("debug,adapter_connection::wire=trace"),
            true,
            true,
            true,
        ),
        (Some("info,[wire_test]=trace"), true, true, true),
    ] {
        let directory =
            std::env::temp_dir().join(format!("sd-log-filter-{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(&directory)?;
        let mut command = Command::new(std::env::current_exe()?);
        command
            .args([
                "--exact",
                "default_and_explicit_filters_apply_to_both_outputs",
                "--nocapture",
            ])
            .env(CHILD, &directory);
        match filter {
            Some(filter) => {
                command.env("RUST_LOG", filter);
            }
            None => {
                command.env_remove("RUST_LOG");
            }
        }
        let output = command.output()?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let console = String::from_utf8_lossy(&output.stderr);
        assert_eq!(console.matches(TRACE_MARKER).count(), usize::from(trace));
        assert_eq!(
            console.matches(DEBUG_MARKER).count(),
            usize::from(console_debug)
        );
        assert_eq!(console.matches(INFO_MARKER).count(), 1);
        let mut contents = String::new();
        for entry in fs::read_dir(&directory)? {
            let path = entry?.path();
            if path.is_file() && !path.to_string_lossy().ends_with(".gz") {
                contents.push_str(&fs::read_to_string(path)?);
            }
        }
        assert_eq!(
            contents.matches(TRACE_MARKER).count(),
            usize::from(trace),
            "{contents}"
        );
        assert_eq!(
            contents.matches(DEBUG_MARKER).count(),
            usize::from(file_debug)
        );
        assert_eq!(contents.matches(INFO_MARKER).count(), 1);
        if trace {
            let event: serde_json::Value = serde_json::from_str(
                contents
                    .lines()
                    .find(|line| line.contains(TRACE_MARKER))
                    .expect("trace event"),
            )?;
            assert_eq!(event["level"], "Trace");
            assert_eq!(event["message_id"], "019fe746-6158-7403-82c9-ac8b7e502cb3");
            assert!(
                event["message"]
                    .as_str()
                    .unwrap()
                    .contains(&"AB".repeat(1024))
            );
        }
        fs::remove_dir_all(directory)?;
    }
    Ok(())
}

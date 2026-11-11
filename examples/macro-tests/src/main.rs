//! # Macro Tests -- Compile-Time Verification Suite
//!
//! This crate uses `trybuild` to verify that the procedural macros
//! (`#[service]`, `#[trigger]`, `#[provider]`) produce correct compile-time
//! errors for invalid inputs and expand correctly for valid inputs.
//!
//! **Run**: `cargo test -p example-macro-tests`

fn main() {
    service_daemon::init_logging();
    tracing::info!("This crate is designed to be run as tests:");
    tracing::info!("  cargo test -p example-macro-tests");
}

//! # Simulation Example -- Interactive Debugging Sandbox
//!
//! This example demonstrates the `simulation` feature with **real `#[service]`** functions:
//! - `MockContext::builder()` for creating isolated simulation environments
//! - `SimulationHandle` for dynamic "God Hand" intervention
//! - Real `#[service]`-annotated services running inside a sandbox `ServiceDaemon`
//!
//! The `simulation` feature is compile-time gated: all simulation types are
//! physically absent from production builds.
//!
//! **Run tests**: `cargo test -p example-simulation`

// Import library so that `#[service]` registrations are linked into this binary.
use example_simulation as _;

/// This file is intentionally minimal -- the real demonstration is in the tests.
fn main() {
    tracing_subscriber::fmt::init();
    tracing::info!("This example is designed to be run as tests:");
    tracing::info!("  cargo test -p example-simulation");
}

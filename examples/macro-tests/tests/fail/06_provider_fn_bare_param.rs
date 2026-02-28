//! Fail case: An async fn provider with a non-Arc parameter should produce
//! a clear compile error.
//!
//! Provider function parameters MUST be Arc-wrapped so the framework can
//! resolve them via the DI system. Bare types are rejected at macro expansion.

use service_daemon::provider;

/// A bare (non-Arc) parameter is not allowed in fn providers.
#[provider]
pub async fn bad_provider(port: i32) -> String {
    format!("port: {}", port)
}

fn main() {}

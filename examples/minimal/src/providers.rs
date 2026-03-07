//! Minimal dependency providers.
//!
//! Demonstrates the simplest form of type-based DI:
//! - A newtype wrapper with a default value.

use service_daemon::provider;

/// Server port configuration.
/// The `#[provider(8080)]` macro auto-generates `Deref`, `Display`,
/// and `Default` implementations, making `Port` injectable into any service.
#[derive(Clone)]
#[provider(8080)]
pub struct Port(pub i32);

#[provider(Listen("0.0.0.0:8081", env = "MINIMAL_LISTEN_ADDR"))]
pub struct MinimalListener;

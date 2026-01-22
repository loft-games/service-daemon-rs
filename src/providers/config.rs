//! Configuration providers
//!
//! DEPRECATED: These string-based providers are replaced by Type-Based DI.
//! See `typed_providers.rs` for the new type-safe versions.
//! This file is kept for reference only.

#![allow(dead_code)]

use service_daemon::provider;

#[provider(name = "port")]
const PORT: i32 = 8080;

#[provider(name = "db_url")]
fn get_db_url() -> String {
    std::env::var("DATABASE_URL").unwrap_or_else(|_| "mysql://localhost".to_string())
}

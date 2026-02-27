//! Fail case: #[provider] on an enum should fail.
//!
//! The #[provider] macro only supports struct and async fn items.
//! Applying it to an enum should produce a clear compile error.

use service_daemon::provider;

#[provider(default = "A")]
pub enum BadProvider {
    A,
    B,
}

fn main() {}

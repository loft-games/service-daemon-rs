//! Fail case: #[provider] async fn without return type should fail.
//!
//! An async fn provider MUST have a return type so the framework
//! knows what type to register in the DI container.

use service_daemon::provider;

#[provider]
pub async fn no_return_provider() {
    // Missing return type -- should fail.
}

fn main() {}

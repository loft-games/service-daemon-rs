//! Pass case: A synchronous (non-async) fn provider should compile
//! successfully, though it emits a runtime warning at first resolution.
//!
//! The `#[allow(sync_handler)]` variant suppresses the warning.

use service_daemon::provider;

/// Config type for the sync provider test.
#[derive(Clone)]
pub struct SyncConfig(pub i32);

/// Sync fn provider — generates a runtime warning on first call.
#[provider]
pub fn sync_config() -> SyncConfig {
    SyncConfig(42)
}

/// A separate config type for the silent sync provider test.
#[derive(Clone)]
pub struct SilentSyncConfig(pub i32);

/// Sync fn provider with explicit opt-in — no warning generated.
#[provider]
#[allow(sync_handler)]
pub fn silent_sync_config() -> SilentSyncConfig {
    SilentSyncConfig(99)
}

fn main() {}

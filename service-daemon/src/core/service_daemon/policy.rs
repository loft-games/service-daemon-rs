//! Restart policy configuration for service recovery.
//!
//! This module now re-exports from `crate::models::policy`, which is the
//! canonical location for the shared retry / backoff types.

pub use crate::models::policy::{RestartPolicy, RestartPolicyBuilder};

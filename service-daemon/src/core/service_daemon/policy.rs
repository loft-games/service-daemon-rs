//! Restart policy configuration for service recovery.
//!
//! This module now re-exports from `crate::models::policy`, which is the
//! shared location for the retry / backoff types.

pub use crate::models::policy::{RestartPolicy, RestartPolicyBuilder};

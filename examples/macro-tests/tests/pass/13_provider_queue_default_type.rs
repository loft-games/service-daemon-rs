//! Pass case: `#[provider(Queue)]` without specifying an inner type should
//! default to `String` and compile successfully.
//!
//! This test guards against regression of the Queue template default behavior.

use service_daemon::provider;

#[provider(Queue)]
pub struct DefaultQueue;

fn main() {}

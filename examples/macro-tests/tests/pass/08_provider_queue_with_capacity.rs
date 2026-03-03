//! Pass case: Queue template with custom capacity parameter compiles successfully.

use service_daemon::provider;

#[provider(Queue(String), capacity = 500)]
pub struct HighCapacityQueue;

#[provider(Queue(i32), capacity = 10)]
pub struct SmallQueue;

fn main() {}

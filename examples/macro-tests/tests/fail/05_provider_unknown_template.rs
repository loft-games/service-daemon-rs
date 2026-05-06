//! Fail case: Unknown named attribute should produce a compile error.

use service_daemon::provider;

#[provider(8080, bogus = "bad")]
pub struct BadAttr(pub i32);

fn main() {}

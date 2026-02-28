//! Fail case: #[provider] on a method with `self` parameter should fail.
//!
//! Provider functions must be free functions. Applying `#[provider]` to a
//! method (with `self`, `&self`, or `&mut self`) is not supported.

use service_daemon::provider;

struct Foo;

impl Foo {
    #[provider]
    pub async fn bad_method(&self) -> String {
        "oops".to_string()
    }
}

fn main() {}

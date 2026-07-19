//! Fail case: arbitrary path-call provider heads are not open provider templates.

use service_daemon::provider;

mod templates {
    pub struct Custom;
}

#[provider(templates::Custom(String), capacity = 1)]
pub struct CustomProvider;

fn main() {}

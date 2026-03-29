//! Pass case: `pub(in super::super)` remains correct after scope nesting.

use service_daemon::service;

mod outer {
    use super::service;

    mod parent {
        use super::service;

        pub mod child {
            use super::service;

            #[service]
            pub(in super::super) async fn svc_in_outer() -> anyhow::Result<()> {
                Ok(())
            }
        }
    }

    pub fn call_from_outer() {
        let _ = parent::child::svc_in_outer;
    }
}

fn main() {}

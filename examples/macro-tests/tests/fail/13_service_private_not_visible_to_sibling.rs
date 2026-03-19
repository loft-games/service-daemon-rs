//! Fail case: a private #[service] should not become visible to sibling modules.

use service_daemon::service;

mod parent {
    use super::service;

    mod a {
        use super::service;

        #[service]
        async fn private_service() -> anyhow::Result<()> {
            Ok(())
        }
    }

    mod b {
        // Sibling module should not be able to access `a::private_service`.
        pub fn try_access() {
            let _ = super::a::private_service;
        }
    }
}

fn main() {}

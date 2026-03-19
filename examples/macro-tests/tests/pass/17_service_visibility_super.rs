//! Pass case: Visibility is preserved through the scope module for #[service].

use service_daemon::service;

mod outer {
    use super::service;

    mod parent {
        use super::service;

        // This is `pub(super)` from `parent` -> visible to `outer`.
        #[service]
        pub(super) async fn svc() -> anyhow::Result<()> {
            Ok(())
        }

        // Ensure the symbol is reachable from the parent module.
        pub fn call_from_parent() {
            let _ = svc;
        }
    }

    pub fn call_from_outer() {
        // Should be callable from the super module (outer).
        let _ = parent::svc;
    }
}

fn main() {}

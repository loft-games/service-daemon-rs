//! Pass case: generated service wrappers are reachable by path.
//!
//! Future service-reference macros can rewrite a service function path to the
//! generated wrapper path only if the wrapper symbol is consistently reachable
//! under ordinary module visibility patterns.

use service_daemon::service;

mod root_private {
    use super::service;

    #[service]
    async fn private_worker() -> anyhow::Result<()> {
        Ok(())
    }

    pub fn wrapper_from_defining_module() {
        let _wrapper = private_worker_wrapper;
    }
}

mod nested_visibility {
    use super::service;

    mod parent {
        use super::service;

        #[service]
        pub(super) async fn super_visible_worker() -> anyhow::Result<()> {
            Ok(())
        }

        #[service]
        pub(in super) async fn in_super_visible_worker() -> anyhow::Result<()> {
            Ok(())
        }
    }

    pub fn wrappers_from_parent_scope() {
        let _super_visible = parent::super_visible_worker_wrapper;
        let _in_super_visible = parent::in_super_visible_worker_wrapper;
    }
}

mod reexported {
    use super::service;

    mod implementation {
        use super::service;

        #[service]
        pub async fn exported_worker() -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[allow(unused_imports)]
    pub use implementation::exported_worker;

    pub fn wrapper_from_original_module_path() {
        let _wrapper = implementation::exported_worker_wrapper;
    }
}

fn main() {
    root_private::wrapper_from_defining_module();
    nested_visibility::wrappers_from_parent_scope();
    reexported::wrapper_from_original_module_path();
}

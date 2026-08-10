//! Pass case: `service_handle!` can target generated service wrappers by path.

use service_daemon::service;

mod root_private {
    use super::service;

    #[service]
    async fn private_worker() -> anyhow::Result<()> {
        Ok(())
    }

    pub fn handle_from_defining_module() {
        let _handle: Result<service_daemon::ServiceHandle, service_daemon::ProviderError> =
            service_daemon::service_handle!(private_worker);
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

    pub fn handles_from_parent_scope() {
        let _super_visible: Result<service_daemon::ServiceHandle, service_daemon::ProviderError> =
            service_daemon::service_handle!(parent::super_visible_worker);
        let _in_super_visible: Result<service_daemon::ServiceHandle, service_daemon::ProviderError> =
            service_daemon::service_handle!(parent::in_super_visible_worker);
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

    pub fn handle_from_original_module_path() {
        let _handle: Result<service_daemon::ServiceHandle, service_daemon::ProviderError> =
            service_daemon::service_handle!(implementation::exported_worker);
    }
}

fn main() {
    root_private::handle_from_defining_module();
    nested_visibility::handles_from_parent_scope();
    reexported::handle_from_original_module_path();
}

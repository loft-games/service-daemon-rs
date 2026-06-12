use service_daemon::provider;

mod visibility_cases {
    use super::*;

    #[provider(Notify)]
    pub struct VisibilitySignal;

    pub mod nested {
        pub mod leaf {
            use super::super::VisibilitySignal;
            use service_daemon::TT::*;
            use service_daemon::{service, trigger};

            #[service(tags = ["__macro_visibility_crate_path__"])]
            pub(in crate::visibility_cases) async fn crate_path_service() -> anyhow::Result<()> {
                Ok(())
            }

            #[service(tags = ["__macro_visibility_nested_super__"])]
            pub(in super::super) async fn nested_super_service() -> anyhow::Result<()> {
                Ok(())
            }

            #[service(tags = ["__macro_visibility_self__"])]
            pub(self) async fn self_restricted_service() -> anyhow::Result<()> {
                Ok(())
            }

            #[trigger(Notify(VisibilitySignal))]
            pub(in super::super) async fn nested_super_trigger() -> anyhow::Result<()> {
                Ok(())
            }
        }
    }
}

fn main() {}

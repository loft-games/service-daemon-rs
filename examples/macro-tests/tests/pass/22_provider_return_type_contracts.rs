use service_daemon::{ProviderInitError, provider};
use std::sync::Arc;

#[derive(Clone, Default)]
pub struct QualifiedFallible;

#[provider]
async fn qualified_fallible_provider(
) -> std::result::Result<QualifiedFallible, service_daemon::ProviderError> {
    Ok(QualifiedFallible)
}

mod imported_provider_error {
    use service_daemon::{ProviderError, provider};

    #[derive(Clone, Default)]
    pub struct ImportedFallible;

    #[provider]
    async fn imported_fallible_provider() -> Result<ImportedFallible, ProviderError> {
        Ok(ImportedFallible)
    }
}

mod aliased_provider_error {
    use service_daemon::provider;

    type ProviderError = service_daemon::ProviderError;

    #[derive(Clone, Default)]
    pub struct AliasedFallible;

    #[provider]
    async fn aliased_fallible_provider() -> Result<AliasedFallible, ProviderError> {
        Ok(AliasedFallible)
    }
}

async fn assert_return_type_contracts() -> Result<(), ProviderInitError> {
    let _: Arc<QualifiedFallible> = QualifiedFallible::resolve().await?;
    let _: Arc<imported_provider_error::ImportedFallible> =
        imported_provider_error::ImportedFallible::resolve().await?;
    let _: Arc<aliased_provider_error::AliasedFallible> =
        aliased_provider_error::AliasedFallible::resolve().await?;

    Ok(())
}

fn main() {}

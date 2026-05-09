//! Pass case: provider helper return shape depends on whether init is known-fallible.

use service_daemon::{ProviderInitError, provider};
use std::sync::Arc;

#[derive(Clone, Default)]
#[provider]
pub struct PlainProvider {
    pub value: i32,
}

#[derive(Clone, Default)]
pub struct PlainFunctionProvider;

#[provider]
async fn plain_function_provider() -> PlainFunctionProvider {
    PlainFunctionProvider
}

#[provider(Notify)]
pub struct HelperSignal;

#[provider(Queue(String))]
pub struct HelperQueue;

#[derive(Clone)]
#[provider(env = "SERVICE_DAEMON_RS_MACRO_FALLIBLE_HELPER_ENV")]
pub struct RequiredEnv(pub String);

#[derive(Clone, Default)]
pub struct FnWithDeps;

#[provider]
async fn fn_with_deps(_plain: Arc<PlainProvider>) -> FnWithDeps {
    FnWithDeps
}

#[derive(Clone)]
#[provider]
pub struct StructWithDeps {
    pub plain: Arc<PlainProvider>,
}

async fn assert_helper_return_shapes() -> Result<(), ProviderInitError> {
    let _: Arc<PlainProvider> = PlainProvider::resolve().await;
    let _: Arc<service_daemon::RwLock<PlainProvider>> = PlainProvider::resolve_rwlock().await;
    let _: Arc<service_daemon::Mutex<PlainProvider>> = PlainProvider::resolve_mutex().await;

    let _: Arc<PlainFunctionProvider> = PlainFunctionProvider::resolve().await;
    let _: Arc<HelperSignal> = HelperSignal::resolve().await;
    let _: Arc<HelperQueue> = HelperQueue::resolve().await;

    let _: Arc<RequiredEnv> = RequiredEnv::resolve().await?;
    let _: Arc<FnWithDeps> = FnWithDeps::resolve().await?;
    let _: Arc<StructWithDeps> = StructWithDeps::resolve().await?;
    let _: Arc<service_daemon::RwLock<StructWithDeps>> = StructWithDeps::resolve_rwlock().await?;
    let _: Arc<service_daemon::Mutex<StructWithDeps>> = StructWithDeps::resolve_mutex().await?;

    Ok(())
}

fn main() {}

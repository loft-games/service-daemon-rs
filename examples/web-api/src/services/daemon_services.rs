use crate::providers::{ExampleConfig, HttpListener, SharedExampleState};
use crate::routers::build_router;
use crate::services::api::HttpApiState;
use service_daemon::{ServiceError, ServicePriority, service};
use tracing::{error, info};

#[service(priority = ServicePriority::EXTERNAL)]
pub async fn http_server_service(
    config: Arc<ExampleConfig>,
    state: Arc<SharedExampleState>,
    listener: Arc<HttpListener>,
) -> anyhow::Result<()> {
    let listener = listener
        .get()
        .map_err(|error| ServiceError::runtime_io("clone HTTP listener", error))?;
    let addr = listener
        .local_addr()
        .map_err(|error| ServiceError::runtime_io("read HTTP listener local address", error))?;

    let app = build_router(HttpApiState::new(config, state));
    info!(%addr, "Web API example listening");

    let server = axum::serve(listener, app.into_make_service())
        .with_graceful_shutdown(service_daemon::wait_shutdown());
    if let Err(error) = server.await {
        error!(%error, "Web API example server stopped with an error");
        return Err(error.into());
    }

    info!(%addr, "Web API example stopped gracefully");
    Ok(())
}

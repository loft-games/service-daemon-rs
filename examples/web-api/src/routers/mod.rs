use axum::Router;
use axum::http::{Method, header};
use tower_http::cors::CorsLayer;
use utoipa::openapi::OpenApi;

use crate::providers::ExampleConfig;
use crate::services::api::HttpApiState;

mod v1;

#[cfg(any(debug_assertions, feature = "devtools"))]
use utoipa::openapi::{Info, OpenApiBuilder, Paths};
#[cfg(any(debug_assertions, feature = "devtools"))]
use utoipa_swagger_ui::SwaggerUi;

pub fn build_router(api_state: HttpApiState) -> Router {
    let cors = cors_layer(&api_state.config);
    let (api_router, api_doc) = api_router();
    let mut app = Router::new().nest("/api", api_router);

    #[cfg(any(debug_assertions, feature = "devtools"))]
    {
        app = app.merge(build_swagger_ui(api_doc));
    }

    app.layer(cors).with_state(api_state)
}

fn api_router() -> (Router<HttpApiState>, OpenApi) {
    let (router, doc) = v1::router();
    (
        Router::new().nest("/v1", router),
        OpenApi::default().nest("/v1", doc),
    )
}

fn cors_layer(config: &ExampleConfig) -> CorsLayer {
    CorsLayer::new()
        .allow_origin(config.cors_allowed_origin.clone())
        .allow_methods([Method::GET, Method::POST, Method::PATCH])
        .allow_headers([header::CONTENT_TYPE])
}

#[cfg(any(debug_assertions, feature = "devtools"))]
fn build_swagger_ui(api_doc: OpenApi) -> SwaggerUi {
    let openapi = OpenApiBuilder::new()
        .info(Info::new("Service Daemon Web API Example", "1.0.0"))
        .paths(Paths::new())
        .build();

    SwaggerUi::new("/docs").url("/swagger_doc", openapi.nest("/api", api_doc))
}

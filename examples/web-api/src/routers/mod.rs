use axum::Router;
use axum::http::{Method, header};
use tower_http::cors::CorsLayer;
use utoipa_axum::router::OpenApiRouter;

use crate::providers::ExampleConfig;
use crate::services::api::HttpApiState;

mod v1;

#[cfg(any(debug_assertions, feature = "devtools"))]
use utoipa::openapi::{Info, OpenApi, OpenApiBuilder, Paths};
#[cfg(any(debug_assertions, feature = "devtools"))]
use utoipa_swagger_ui::SwaggerUi;

pub fn build_router(api_state: HttpApiState) -> Router {
    let cors = cors_layer(&api_state.config);
    let api_router = OpenApiRouter::new().nest("/api", api_router());
    #[cfg(any(debug_assertions, feature = "devtools"))]
    let app = {
        let (api_router, api_doc) = api_router.split_for_parts();
        api_router.merge(build_swagger_ui(api_doc))
    };
    #[cfg(not(any(debug_assertions, feature = "devtools")))]
    let app: Router<HttpApiState> = api_router.into();

    app.layer(cors).with_state(api_state)
}

fn api_router() -> OpenApiRouter<HttpApiState> {
    OpenApiRouter::new().nest("/v1", v1::router())
}

fn cors_layer(config: &ExampleConfig) -> CorsLayer {
    CorsLayer::new()
        .allow_origin(config.cors_allowed_origin.clone())
        .allow_methods([Method::GET, Method::POST, Method::PATCH])
        .allow_headers([header::CONTENT_TYPE])
}

#[cfg(any(debug_assertions, feature = "devtools"))]
fn build_swagger_ui(api_doc: OpenApi) -> SwaggerUi {
    let mut openapi = OpenApiBuilder::new()
        .info(Info::new("Service Daemon Web API Example", "1.0.0"))
        .paths(Paths::new())
        .build();
    openapi.merge(api_doc);

    SwaggerUi::new("/docs").url("/swagger_doc", openapi)
}

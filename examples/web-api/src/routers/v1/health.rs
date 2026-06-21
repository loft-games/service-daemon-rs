use axum::Router;
use axum::extract::State;
use utoipa::openapi::OpenApi;
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::models::api::response::{HealthResponse, WebResponse};
use crate::services::api::{HttpApiState, health_response};

pub fn router() -> (Router<HttpApiState>, OpenApi) {
    OpenApiRouter::new()
        .routes(routes!(health))
        .split_for_parts()
}

#[utoipa::path(
    get,
    path = "",
    responses((status = OK, description = "Return service health metadata", body = WebResponse<HealthResponse>)),
    tag = "health"
)]
async fn health(State(api): State<HttpApiState>) -> WebResponse<HealthResponse> {
    WebResponse::success(health_response(&api).await)
}

use axum::extract::State;
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::models::api::response::{HealthResponse, WebResponse};
use crate::services::api::{HttpApiState, health_response};

pub fn router() -> OpenApiRouter<HttpApiState> {
    OpenApiRouter::new().routes(routes!(health))
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

use axum::Router;
use utoipa::openapi::OpenApi;

use crate::services::api::HttpApiState;

mod health;
mod items;

pub fn router() -> (Router<HttpApiState>, OpenApi) {
    let (health_router, health_doc) = health::router();
    let (items_router, items_doc) = items::router();

    let router = Router::new()
        .nest("/health", health_router)
        .nest("/items", items_router);
    let doc = OpenApi::default()
        .nest("/health", health_doc)
        .nest("/items", items_doc);

    (router, doc)
}

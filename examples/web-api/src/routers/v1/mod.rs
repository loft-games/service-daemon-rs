use utoipa_axum::router::OpenApiRouter;

use crate::services::api::HttpApiState;

mod health;
mod items;

pub fn router() -> OpenApiRouter<HttpApiState> {
    OpenApiRouter::new()
        .nest("/health", health::router())
        .nest("/items", items::router())
}

pub mod health;
pub mod item;
pub mod maintenance;

use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

pub use health::HealthResponse;
pub use item::ItemResponse;
pub use maintenance::MaintenanceOutcome;

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct ListResponse<T> {
    pub items: Vec<T>,
    pub total: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct WebResponse<T> {
    pub code: u16,
    pub message: String,
    pub data: Option<T>,
}

impl<T> WebResponse<T> {
    pub fn success(data: T) -> Self {
        Self {
            code: StatusCode::OK.as_u16(),
            message: "success".to_owned(),
            data: Some(data),
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self {
            code: StatusCode::BAD_REQUEST.as_u16(),
            message: message.into(),
            data: None,
        }
    }

    pub fn not_found() -> Self {
        Self {
            code: StatusCode::NOT_FOUND.as_u16(),
            message: "not found".to_owned(),
            data: None,
        }
    }

    pub fn too_many_requests() -> Self {
        Self {
            code: StatusCode::TOO_MANY_REQUESTS.as_u16(),
            message: "item limit reached".to_owned(),
            data: None,
        }
    }
}

impl<T> IntoResponse for WebResponse<T>
where
    T: Serialize,
{
    fn into_response(self) -> Response {
        (StatusCode::OK, Json(self)).into_response()
    }
}

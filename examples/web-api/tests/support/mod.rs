use axum::body::to_bytes;
use example_web_api::providers::{ExampleConfig, SharedExampleState};
use example_web_api::services::api::HttpApiState;
use std::sync::Arc;

pub fn api_state() -> HttpApiState {
    HttpApiState::new(
        Arc::new(ExampleConfig::default()),
        Arc::new(SharedExampleState::default()),
    )
}

#[allow(dead_code)]
pub async fn json_body<T: serde::de::DeserializeOwned>(response: axum::response::Response) -> T {
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body should be readable");
    serde_json::from_slice(&bytes).expect("response body should be valid JSON")
}

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::models::api::response::ItemResponse;
use axum::http::HeaderValue;
use service_daemon::provider;
use tokio::sync::RwLock;

#[provider(Listen("127.0.0.1:0"), env = "WEB_API_LISTEN_ADDR", eager = true)]
pub struct HttpListener;

#[derive(Clone)]
pub struct ExampleConfig {
    pub service_name: String,
    pub max_items: usize,
    pub maintenance_batch_size: usize,
    pub cors_allowed_origin: HeaderValue,
}

impl Default for ExampleConfig {
    fn default() -> Self {
        Self {
            service_name: "service-daemon web example".to_owned(),
            max_items: 100,
            maintenance_batch_size: 16,
            cors_allowed_origin: HeaderValue::from_static("http://localhost:3000"),
        }
    }
}

#[derive(Clone, Default)]
pub(crate) struct ExampleState {
    pub next_id: u64,
    pub items: BTreeMap<u64, ItemResponse>,
    pub maintenance_runs: u64,
    pub archived_total: usize,
    pub refreshed_total: usize,
}

#[derive(Clone, Default)]
pub struct SharedExampleState {
    pub(crate) inner: Arc<RwLock<ExampleState>>,
}

#[provider]
pub async fn example_config_provider() -> ExampleConfig {
    ExampleConfig::default()
}

#[provider]
pub async fn shared_example_state_provider() -> SharedExampleState {
    SharedExampleState::default()
}

#[derive(Clone)]
#[provider("0 */30 * * * *")]
pub struct MaintenanceSchedule(pub String);

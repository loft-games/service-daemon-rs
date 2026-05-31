use std::sync::Arc;

use axum::http::StatusCode;
use tracing::info;

use crate::models::api::request::{CreateItemRequest, PatchItemRequest};
use crate::models::api::response::{HealthResponse, ItemResponse, ListResponse};
use crate::providers::{ExampleConfig, SharedExampleState};

#[derive(Clone)]
pub struct HttpApiState {
    pub config: Arc<ExampleConfig>,
    pub state: Arc<SharedExampleState>,
}

impl HttpApiState {
    pub fn new(config: Arc<ExampleConfig>, state: Arc<SharedExampleState>) -> Self {
        Self { config, state }
    }
}

pub async fn health_response(api: &HttpApiState) -> HealthResponse {
    let state = api.state.inner.read().await;
    HealthResponse {
        service_name: api.config.service_name.clone(),
        item_count: state.items.len(),
        maintenance_runs: state.maintenance_runs,
    }
}

pub async fn list_item_records(api: &HttpApiState) -> ListResponse<ItemResponse> {
    let state = api.state.inner.read().await;
    let items: Vec<_> = state.items.values().cloned().collect();
    ListResponse {
        total: items.len() as u64,
        items,
    }
}

pub async fn get_item_record(api: &HttpApiState, id: u64) -> Result<ItemResponse, StatusCode> {
    let state = api.state.inner.read().await;
    state.items.get(&id).cloned().ok_or(StatusCode::NOT_FOUND)
}

pub async fn create_item_record(
    api: &HttpApiState,
    payload: CreateItemRequest,
) -> Result<ItemResponse, StatusCode> {
    let label = normalized_label(payload.label)?;
    let mut state = api.state.inner.write().await;
    if state.items.len() >= api.config.max_items {
        return Err(StatusCode::TOO_MANY_REQUESTS);
    }

    state.next_id += 1;
    let item = ItemResponse {
        id: state.next_id,
        label,
        stale: false,
        archived: false,
        revision: 1,
    };
    state.items.insert(item.id, item.clone());
    drop(state);

    info!(item_id = item.id, "created Web API example item");
    Ok(item)
}

pub async fn patch_item_record(
    api: &HttpApiState,
    id: u64,
    payload: PatchItemRequest,
) -> Result<ItemResponse, StatusCode> {
    let mut state = api.state.inner.write().await;
    let item = state.items.get_mut(&id).ok_or(StatusCode::NOT_FOUND)?;

    if let Some(label) = payload.label {
        item.label = normalized_label(label)?;
    }
    if let Some(stale) = payload.stale {
        item.stale = stale;
    }
    item.revision += 1;
    let updated = item.clone();
    drop(state);

    info!(item_id = updated.id, "patched Web API example item");
    Ok(updated)
}

fn normalized_label(label: String) -> Result<String, StatusCode> {
    let label = label.trim();
    if label.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(label.to_owned())
}

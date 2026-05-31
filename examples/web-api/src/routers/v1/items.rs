use axum::Router;
use axum::extract::{Json, Path, State};
use axum::http::StatusCode;
use utoipa::openapi::OpenApi;
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::models::api::request::{CreateItemRequest, PatchItemRequest};
use crate::models::api::response::{ItemResponse, ListResponse, WebResponse};
use crate::services::api::{
    HttpApiState, create_item_record, get_item_record, list_item_records, patch_item_record,
};

pub fn router() -> (Router<HttpApiState>, OpenApi) {
    OpenApiRouter::new()
        .routes(routes!(list_items))
        .routes(routes!(create_item))
        .routes(routes!(get_item))
        .routes(routes!(patch_item))
        .split_for_parts()
}

#[utoipa::path(
    get,
    path = "",
    responses((status = OK, description = "List items", body = WebResponse<ListResponse<ItemResponse>>)),
    tag = "items"
)]
async fn list_items(State(api): State<HttpApiState>) -> WebResponse<ListResponse<ItemResponse>> {
    WebResponse::success(list_item_records(&api).await)
}

#[utoipa::path(
    post,
    path = "",
    request_body = CreateItemRequest,
    responses(
        (status = OK, description = "HTTP 200 envelope with code 200 when the item is created", body = WebResponse<ItemResponse>),
        (status = OK, description = "HTTP 200 envelope with business code 400 for validation errors or 429 for quota errors", body = WebResponse<ItemResponse>)
    ),
    tag = "items"
)]
async fn create_item(
    State(api): State<HttpApiState>,
    Json(payload): Json<CreateItemRequest>,
) -> WebResponse<ItemResponse> {
    match create_item_record(&api, payload).await {
        Ok(item) => WebResponse::success(item),
        Err(status) => response_from_status(status),
    }
}

#[utoipa::path(
    get,
    path = "/{id}",
    params(("id" = u64, Path, description = "Item identifier")),
    responses((status = OK, description = "Return an item", body = WebResponse<ItemResponse>)),
    tag = "items"
)]
async fn get_item(
    State(api): State<HttpApiState>,
    Path(id): Path<u64>,
) -> WebResponse<ItemResponse> {
    match get_item_record(&api, id).await {
        Ok(item) => WebResponse::success(item),
        Err(status) => response_from_status(status),
    }
}

#[utoipa::path(
    patch,
    path = "/{id}",
    params(("id" = u64, Path, description = "Item identifier")),
    request_body = PatchItemRequest,
    responses((status = OK, description = "Patch an item", body = WebResponse<ItemResponse>)),
    tag = "items"
)]
async fn patch_item(
    State(api): State<HttpApiState>,
    Path(id): Path<u64>,
    Json(payload): Json<PatchItemRequest>,
) -> WebResponse<ItemResponse> {
    match patch_item_record(&api, id, payload).await {
        Ok(item) => WebResponse::success(item),
        Err(status) => response_from_status(status),
    }
}

fn response_from_status<T>(status: StatusCode) -> WebResponse<T> {
    match status {
        StatusCode::BAD_REQUEST => WebResponse::bad_request("label must not be empty"),
        StatusCode::NOT_FOUND => WebResponse::not_found(),
        StatusCode::TOO_MANY_REQUESTS => WebResponse::too_many_requests(),
        _ => WebResponse::bad_request("request could not be processed"),
    }
}

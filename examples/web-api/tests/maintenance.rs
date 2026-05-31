mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use example_web_api::models::api::request::{CreateItemRequest, PatchItemRequest};
use example_web_api::models::api::response::{ItemResponse, ListResponse, WebResponse};
use example_web_api::routers::build_router;
use example_web_api::services::api::{create_item_record, patch_item_record};
use example_web_api::trigger_handlers::run_maintenance;
use tower::ServiceExt;

#[tokio::test]
async fn maintenance_handler_records_state_side_effects() -> anyhow::Result<()> {
    let api = support::api_state();
    let stale_item = create_item_record(
        &api,
        CreateItemRequest {
            label: "stale item".to_owned(),
        },
    )
    .await
    .expect("stale item should be created");
    patch_item_record(
        &api,
        stale_item.id,
        PatchItemRequest {
            label: None,
            stale: Some(true),
        },
    )
    .await
    .expect("stale flag should be set");
    create_item_record(
        &api,
        CreateItemRequest {
            label: "active item".to_owned(),
        },
    )
    .await
    .expect("active item should be created");

    let outcome = run_maintenance(api.config.clone(), api.state.clone()).await?;
    assert_eq!(outcome.archived, 1);
    assert_eq!(outcome.refreshed, 1);
    assert_eq!(outcome.runs, 1);

    let list_response = build_router(api.clone())
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/items")
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(list_response.status(), StatusCode::OK);
    let list_response: WebResponse<ListResponse<ItemResponse>> =
        support::json_body(list_response).await;
    assert_eq!(list_response.code, StatusCode::OK.as_u16());
    let list = list_response
        .data
        .expect("list response should include item list data");
    assert_eq!(list.total, 2);
    assert!(list.items.iter().any(|item| item.archived));
    assert!(
        list.items
            .iter()
            .any(|item| !item.archived && item.revision == 2)
    );

    let health_response = build_router(api)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/health")
                .body(Body::empty())?,
        )
        .await?;
    let health: serde_json::Value = support::json_body(health_response).await;
    assert_eq!(health["code"], StatusCode::OK.as_u16());
    assert_eq!(health["data"]["maintenance_runs"], 1);
    Ok(())
}

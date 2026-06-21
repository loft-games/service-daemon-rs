mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use example_web_api::models::api::request::{CreateItemRequest, PatchItemRequest};
use example_web_api::models::api::response::{ItemResponse, ListResponse, WebResponse};
use example_web_api::routers::build_router;
use tower::ServiceExt;

#[tokio::test]
async fn request_path_mutates_state() -> anyhow::Result<()> {
    let app = build_router(support::api_state());

    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/items")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&CreateItemRequest {
                    label: "alpha".to_owned(),
                })?))?,
        )
        .await?;
    assert_eq!(create_response.status(), StatusCode::OK);
    let created_response: WebResponse<ItemResponse> = support::json_body(create_response).await;
    assert_eq!(created_response.code, StatusCode::OK.as_u16());
    let created = created_response
        .data
        .expect("create response should include item data");
    assert_eq!(created.label, "alpha");
    assert_eq!(created.revision, 1);

    let patch_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/v1/items/{}", created.id))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&PatchItemRequest {
                    label: Some("beta".to_owned()),
                    stale: Some(true),
                })?))?,
        )
        .await?;
    assert_eq!(patch_response.status(), StatusCode::OK);
    let patched_response: WebResponse<ItemResponse> = support::json_body(patch_response).await;
    assert_eq!(patched_response.code, StatusCode::OK.as_u16());
    let patched = patched_response
        .data
        .expect("patch response should include item data");
    assert_eq!(patched.label, "beta");
    assert!(patched.stale);
    assert_eq!(patched.revision, 2);

    let list_response = app
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
    assert_eq!(list.total, 1);
    assert_eq!(list.items[0], patched);

    Ok(())
}

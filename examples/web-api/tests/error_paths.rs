mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode, header::CONTENT_TYPE};
use example_web_api::models::api::request::{CreateItemRequest, PatchItemRequest};
use example_web_api::models::api::response::{ItemResponse, ListResponse, WebResponse};
use example_web_api::providers::ExampleConfig;
use example_web_api::routers::build_router;
use example_web_api::services::api::HttpApiState;
use std::sync::Arc;
use tower::ServiceExt;

fn state_with_limit(max_items: usize) -> HttpApiState {
    let mut config = ExampleConfig::default();
    config.max_items = max_items;
    HttpApiState::new(
        Arc::new(config),
        Arc::new(example_web_api::providers::SharedExampleState::default()),
    )
}

fn json_request(method: &str, uri: &str, body: Vec<u8>) -> anyhow::Result<Request<Body>> {
    Ok(Request::builder()
        .method(method)
        .uri(uri)
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(body))?)
}

async fn create_item(
    app: &axum::Router,
    label: &str,
) -> anyhow::Result<(StatusCode, WebResponse<ItemResponse>)> {
    let request = json_request(
        "POST",
        "/api/v1/items",
        serde_json::to_vec(&CreateItemRequest {
            label: label.to_owned(),
        })?,
    )?;
    let response = app.clone().oneshot(request).await?;
    let status = response.status();
    Ok((status, support::json_body(response).await))
}

async fn list_items(app: &axum::Router) -> anyhow::Result<WebResponse<ListResponse<ItemResponse>>> {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/items")
                .body(Body::empty())?,
        )
        .await?;
    Ok(support::json_body(response).await)
}

#[tokio::test]
async fn empty_labels_are_rejected_without_mutating_state() -> anyhow::Result<()> {
    let app = build_router(support::api_state());

    for label in ["", "   "] {
        let (status, response) = create_item(&app, label).await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(response.code, StatusCode::BAD_REQUEST.as_u16());
        assert_eq!(response.message, "label must not be empty");
        assert!(response.data.is_none());
    }

    let list = list_items(&app).await?;
    assert_eq!(list.data.expect("list data").total, 0);
    Ok(())
}

#[tokio::test]
async fn item_limit_is_reported_without_adding_an_extra_item() -> anyhow::Result<()> {
    let app = build_router(state_with_limit(1));

    let (first_status, first) = create_item(&app, "first").await?;
    assert_eq!(first_status, StatusCode::OK);
    assert_eq!(first.code, StatusCode::OK.as_u16());

    let (second_status, second) = create_item(&app, "second").await?;
    assert_eq!(second_status, StatusCode::OK);
    assert_eq!(second.code, StatusCode::TOO_MANY_REQUESTS.as_u16());
    assert_eq!(second.message, "item limit reached");
    assert!(second.data.is_none());

    let list = list_items(&app).await?;
    let list = list.data.expect("list data");
    assert_eq!(list.total, 1);
    assert_eq!(list.items[0].label, "first");
    Ok(())
}

#[tokio::test]
async fn missing_items_return_not_found_for_get_and_patch() -> anyhow::Result<()> {
    let app = build_router(support::api_state());

    let get_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/items/404")
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(get_response.status(), StatusCode::OK);
    let get_body: WebResponse<ItemResponse> = support::json_body(get_response).await;
    assert_eq!(get_body.code, StatusCode::NOT_FOUND.as_u16());
    assert!(get_body.data.is_none());

    let patch_response = app
        .clone()
        .oneshot(json_request(
            "PATCH",
            "/api/v1/items/404",
            serde_json::to_vec(&PatchItemRequest {
                label: Some("updated".to_owned()),
                stale: Some(true),
            })?,
        )?)
        .await?;
    assert_eq!(patch_response.status(), StatusCode::OK);
    let patch_body: WebResponse<ItemResponse> = support::json_body(patch_response).await;
    assert_eq!(patch_body.code, StatusCode::NOT_FOUND.as_u16());
    assert!(patch_body.data.is_none());
    Ok(())
}

#[tokio::test]
async fn invalid_patch_preserves_the_existing_record() -> anyhow::Result<()> {
    let app = build_router(support::api_state());
    let (created_status, created_response) = create_item(&app, "before").await?;
    assert_eq!(created_status, StatusCode::OK);
    let created = created_response.data.expect("created item");

    let response = app
        .clone()
        .oneshot(json_request(
            "PATCH",
            &format!("/api/v1/items/{}", created.id),
            serde_json::to_vec(&PatchItemRequest {
                label: Some("  ".to_owned()),
                stale: Some(true),
            })?,
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let body: WebResponse<ItemResponse> = support::json_body(response).await;
    assert_eq!(body.code, StatusCode::BAD_REQUEST.as_u16());

    let current = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/items/{}", created.id))
                .body(Body::empty())?,
        )
        .await?;
    let current: WebResponse<ItemResponse> = support::json_body(current).await;
    let current = current.data.expect("current item");
    assert_eq!(current.label, "before");
    assert!(!current.stale);
    assert_eq!(current.revision, created.revision);
    Ok(())
}

#[tokio::test]
async fn extractor_and_route_errors_have_stable_http_statuses() -> anyhow::Result<()> {
    let app = build_router(support::api_state());

    let malformed = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/items")
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from("{"))?,
        )
        .await?;
    assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);

    let missing_content_type = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/items")
                .body(Body::from(r#"{"label":"item"}"#))?,
        )
        .await?;
    assert_eq!(
        missing_content_type.status(),
        StatusCode::UNSUPPORTED_MEDIA_TYPE
    );

    let wrong_content_type = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/items")
                .header(CONTENT_TYPE, "text/plain")
                .body(Body::from(r#"{"label":"item"}"#))?,
        )
        .await?;
    assert_eq!(
        wrong_content_type.status(),
        StatusCode::UNSUPPORTED_MEDIA_TYPE
    );

    let unknown_route = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/unknown")
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(unknown_route.status(), StatusCode::NOT_FOUND);

    let unsupported_method = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/v1/items")
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(unsupported_method.status(), StatusCode::METHOD_NOT_ALLOWED);
    Ok(())
}

#[tokio::test]
async fn error_envelopes_match_golden_fixture() -> anyhow::Result<()> {
    let app = build_router(support::api_state());
    let (empty_status, empty) = create_item(&app, "").await?;
    assert_eq!(empty_status, StatusCode::OK);
    let missing = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/items/404")
                .body(Body::empty())?,
        )
        .await?;
    let missing: WebResponse<ItemResponse> = support::json_body(missing).await;
    let quota_app = build_router(state_with_limit(0));
    let (quota_status, quota) = create_item(&quota_app, "quota").await?;
    assert_eq!(quota_status, StatusCode::OK);

    let snapshots = vec![
        serde_json::json!({
            "case": "empty-label",
            "code": empty.code,
            "message": empty.message,
            "data": empty.data,
        }),
        serde_json::json!({
            "case": "missing-item",
            "code": missing.code,
            "message": missing.message,
            "data": missing.data,
        }),
        serde_json::json!({
            "case": "quota",
            "code": quota.code,
            "message": quota.message,
            "data": quota.data,
        }),
    ];
    let fixture = include_str!("fixtures/error-envelopes.json").replace("\r\n", "\n");
    for fixture in [fixture.clone(), fixture.replace('\n', "\r\n")] {
        let expected: serde_json::Value = serde_json::from_str(&fixture)?;
        assert_eq!(serde_json::to_value(&snapshots)?, expected);
    }
    Ok(())
}

#![cfg(any(debug_assertions, feature = "devtools"))]

mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use example_web_api::routers::build_router;
use tower::ServiceExt;

#[tokio::test]
async fn swagger_document_describes_public_routes() -> anyhow::Result<()> {
    let response = build_router(support::api_state())
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/swagger_doc")
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::OK);

    let document: serde_json::Value = support::json_body(response).await;
    let paths = document["paths"]
        .as_object()
        .expect("OpenAPI document should include route paths");
    let route_keys: Vec<_> = paths.keys().map(String::as_str).collect();
    assert!(
        route_keys
            .iter()
            .any(|path| *path == "/api/v1/health" || *path == "/api/v1/health/")
    );
    assert!(
        route_keys
            .iter()
            .any(|path| *path == "/api/v1/items" || *path == "/api/v1/items/")
    );
    assert!(route_keys.contains(&"/api/v1/items/{id}"));

    let schemas = document["components"]["schemas"]
        .as_object()
        .expect("OpenAPI document should include schemas");
    assert!(schemas.contains_key("CreateItemRequest"));
    assert!(schemas.contains_key("PatchItemRequest"));
    assert!(schemas.contains_key("ItemResponse"));

    Ok(())
}

#[tokio::test]
async fn swagger_ui_is_available_in_devtools() -> anyhow::Result<()> {
    let response = build_router(support::api_state())
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/docs")
                .body(Body::empty())?,
        )
        .await?;
    assert!(response.status().is_success() || response.status().is_redirection());
    Ok(())
}

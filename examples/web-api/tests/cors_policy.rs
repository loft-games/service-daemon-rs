mod support;

use axum::body::Body;
use axum::http::Request;
use axum::http::header::{
    ACCESS_CONTROL_ALLOW_HEADERS, ACCESS_CONTROL_ALLOW_METHODS, ACCESS_CONTROL_ALLOW_ORIGIN,
};
use example_web_api::routers::build_router;
use tower::ServiceExt;

#[tokio::test]
async fn cors_preflight_exposes_explicit_policy() -> anyhow::Result<()> {
    let response = build_router(support::api_state())
        .oneshot(
            Request::builder()
                .method("OPTIONS")
                .uri("/api/v1/items")
                .header("origin", "http://localhost:3000")
                .header("access-control-request-method", "POST")
                .header("access-control-request-headers", "content-type")
                .body(Body::empty())?,
        )
        .await?;
    assert!(response.status().is_success());
    assert_eq!(
        response
            .headers()
            .get(ACCESS_CONTROL_ALLOW_ORIGIN)
            .and_then(|value| value.to_str().ok()),
        Some("http://localhost:3000")
    );

    let allowed_methods = response
        .headers()
        .get(ACCESS_CONTROL_ALLOW_METHODS)
        .expect("CORS preflight should expose allowed methods")
        .to_str()?;
    assert!(allowed_methods.contains("GET"));
    assert!(allowed_methods.contains("POST"));
    assert!(allowed_methods.contains("PATCH"));

    let allowed_headers = response
        .headers()
        .get(ACCESS_CONTROL_ALLOW_HEADERS)
        .expect("CORS preflight should expose allowed headers")
        .to_str()?;
    assert!(allowed_headers.contains("content-type"));

    Ok(())
}

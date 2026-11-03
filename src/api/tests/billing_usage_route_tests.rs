use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;

use super::ApiClient;

async fn spawn_billing_usage_mock() -> String {
    let app = Router::new()
        .route(
            "/api/v1/billing/usage",
            get(|| async {
                Json(json!({
                    "used_bytes": 42,
                    "quota_bytes": 1_000_000_000_000i64,
                    "percentage": 0.000042,
                }))
            }),
        )
        .route(
            "/api/v1/files/usage",
            // A DIFFERENT shape (the real live /files/usage handler,
            // beebeeb-api/src/routes/files.rs::storage_usage) — no
            // `quota_bytes` key. If `get_billing_usage()` ever aliases
            // back to this route, the assertion below fails loudly
            // instead of silently reading a stale field name.
            get(|| async {
                Json(json!({
                    "used_bytes": 42,
                    "plan_limit_bytes": 1_000_000_000_000i64,
                    "plan_name": "free",
                }))
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn get_billing_usage_hits_the_billing_route_not_files_usage() {
    let api = ApiClient::new_for_test(spawn_billing_usage_mock().await);
    let resp = api.get_billing_usage().await.expect("mock request should succeed");

    assert_eq!(
        resp.get("quota_bytes").and_then(|v| v.as_i64()),
        Some(1_000_000_000_000),
        "get_billing_usage() must read /api/v1/billing/usage's quota_bytes field \
         (bb billing show reads this directly — see commands::billing::show), \
         not /api/v1/files/usage's plan_limit_bytes: {resp}"
    );
    assert!(
        resp.get("plan_limit_bytes").is_none(),
        "response carries plan_limit_bytes — get_billing_usage() hit /api/v1/files/usage \
         instead of /api/v1/billing/usage: {resp}"
    );
}

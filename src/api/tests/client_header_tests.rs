//! Confirms every request through `ApiClient` carries the writer-provenance
//! headers (`X-Beebeeb-Client` / `X-Beebeeb-Client-Version`) that the
//! server records on `object_versions` rows (task 1392 / server PR #23).
//! Uses the same in-process axum mock pattern as `list_pagination_tests`
//! above, echoing the *incoming* request headers back so the assertion is
//! against what `ApiClient` actually sent over the wire, not a re-derived
//! value.

use axum::extract::Request as AxumRequest;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{Map, Value, json};

use super::ApiClient;

async fn spawn_header_echo_mock() -> String {
    let app = Router::new().route(
        "/api/v1/auth/me",
        get(|req: AxumRequest| async move {
            let echoed: Map<String, Value> = req
                .headers()
                .iter()
                .filter_map(|(name, value)| value.to_str().ok().map(|v| (name.as_str().to_string(), json!(v))))
                .collect();
            Json(json!({ "headers": echoed }))
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
async fn every_request_carries_client_and_version_headers() {
    let api = ApiClient::new_for_test(spawn_header_echo_mock().await);
    let resp = api.get_me().await.expect("mock request should succeed");
    let headers = resp.get("headers").expect("mock echoed no headers back");

    let client = headers
        .get("x-beebeeb-client")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let version = headers
        .get("x-beebeeb-client-version")
        .and_then(|v| v.as_str())
        .unwrap_or_default();

    assert_eq!(
        client, "cli",
        "X-Beebeeb-Client header missing or wrong on an ApiClient request"
    );
    assert_eq!(
        version,
        env!("CARGO_PKG_VERSION"),
        "X-Beebeeb-Client-Version header missing or stale on an ApiClient request \
         (must match `bb --version`'s CARGO_PKG_VERSION)"
    );
}

use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use serde_json::json;

use super::ApiClient;

async fn spawn_typed_error_mock() -> String {
    let app = Router::new().route(
        "/api/v1/billing/addons",
        post(|| async {
            (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": "billing_no_payment_method",
                    "message": "No valid payment method on file for this charge. Please add a payment method and try again.",
                })),
            )
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
async fn typed_api_error_surfaces_the_servers_human_message_not_the_bare_code() {
    let api = ApiClient::new_for_test(spawn_typed_error_mock().await);
    let err = api
        .update_billing_addons(json!({ "extra_storage_tb": 1 }))
        .await
        .expect_err("mock returns 400");

    assert_eq!(
        err.message, "No valid payment method on file for this charge. Please add a payment method and try again.",
        "parse_response must prefer the server's `message` field over the bare `error` code \
         (task 1547 finding 3) — every one of ~20 typed server errors carries a friendly \
         message the CLI was dropping on the floor: {err}"
    );
}

#[tokio::test]
async fn error_without_a_message_field_still_falls_back_to_the_bare_code() {
    // Not every non-2xx body is a typed error with a `message` — e.g. an
    // upstream 502 or a handler that only ever set `error`. Must not panic
    // or lose the code entirely.
    let app = Router::new().route(
        "/api/v1/billing/addons",
        post(|| async {
            (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "some_code_with_no_message" })),
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let api = ApiClient::new_for_test(format!("http://{addr}"));
    let err = api
        .update_billing_addons(json!({ "extra_storage_tb": 1 }))
        .await
        .expect_err("mock returns 400");
    assert_eq!(err.message, "some_code_with_no_message");
}

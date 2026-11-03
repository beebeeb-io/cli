//! task 0484: proves `delete_passkey` hits the exact live route
//! (`repos/server/beebeeb-api/src/routes/passkeys.rs::router()`:
//! `DELETE /{id}` nested under `/api/v1/auth/passkeys`) and sends NO
//! `X-Confirm-Token` header — the live handler takes only `AuthUser` +
//! `NotImpersonated`, no `ConfirmedAction` extractor (deviation eng-0484,
//! same reasoning as `revoke_session`/eng-0481 above). Same in-process
//! axum mock pattern: the mock echoes back what it actually received.

use axum::extract::{Path as AxumPath, Request as AxumRequest};
use axum::routing::delete;
use axum::{Json, Router};
use serde_json::{Map, Value, json};

use super::ApiClient;

fn echo_method_id_and_headers(method: &str, id: &str, req: &AxumRequest) -> Value {
    let headers: Map<String, Value> = req
        .headers()
        .iter()
        .filter_map(|(name, value)| value.to_str().ok().map(|v| (name.as_str().to_string(), json!(v))))
        .collect();
    json!({ "method": method, "id": id, "headers": headers })
}

async fn spawn_delete_passkey_mock() -> String {
    let app = Router::new().route(
        "/api/v1/auth/passkeys/:id",
        delete(|AxumPath(id): AxumPath<String>, req: AxumRequest| async move {
            Json(echo_method_id_and_headers("DELETE", &id, &req))
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
async fn delete_passkey_sends_delete_to_the_id_path_with_bearer_auth_and_no_confirm_token() {
    let api = ApiClient::new_for_test(spawn_delete_passkey_mock().await);
    let resp = api
        .delete_passkey("11111111-1111-1111-1111-111111111111")
        .await
        .expect("mock request should succeed");

    assert_eq!(resp["method"], "DELETE", "must be a DELETE, matching the live router");
    assert_eq!(
        resp["id"], "11111111-1111-1111-1111-111111111111",
        "the passkey id must be interpolated into the path, not sent as a body/query param"
    );
    let headers = &resp["headers"];
    assert!(
        headers["authorization"]
            .as_str()
            .unwrap_or_default()
            .starts_with("Bearer "),
        "must send Bearer auth: {headers}"
    );
    assert!(
        headers.get("x-confirm-token").is_none(),
        "must NOT send X-Confirm-Token — the live delete_passkey handler takes only \
         AuthUser + NotImpersonated, no ConfirmedAction extractor (deviation eng-0484): {headers}"
    );
}

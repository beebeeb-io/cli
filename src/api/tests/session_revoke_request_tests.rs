//! task 0481: proves `revoke_session` / `revoke_all_other_sessions` hit
//! the exact live routes with the exact live methods
//! (`account_activity.rs::router()`: `DELETE /sessions/{id}`,
//! `POST /sessions/revoke-all-others`) and, crucially, send NO
//! `X-Confirm-Token` header — the live handlers take only `AuthUser`
//! (read: no `ConfirmedAction` extractor), so a step-up token would be
//! dead weight the server ignores. Same in-process axum mock pattern as
//! `client_header_tests` above: the mock echoes back what it actually
//! received, not a re-derived value.

use axum::extract::{Path as AxumPath, Request as AxumRequest};
use axum::routing::{delete, post};
use axum::{Json, Router};
use serde_json::{Map, Value, json};

use super::ApiClient;

fn echo_method_id_and_headers(method: &str, id: Option<&str>, req: &AxumRequest) -> Value {
    let headers: Map<String, Value> = req
        .headers()
        .iter()
        .filter_map(|(name, value)| value.to_str().ok().map(|v| (name.as_str().to_string(), json!(v))))
        .collect();
    json!({ "method": method, "id": id, "headers": headers })
}

async fn spawn_revoke_mock() -> String {
    let app = Router::new()
        .route(
            "/api/v1/account/sessions/revoke-all-others",
            post(|req: AxumRequest| async move { Json(echo_method_id_and_headers("POST", None, &req)) }),
        )
        .route(
            "/api/v1/account/sessions/:id",
            delete(|AxumPath(id): AxumPath<String>, req: AxumRequest| async move {
                Json(echo_method_id_and_headers("DELETE", Some(&id), &req))
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
async fn revoke_session_sends_delete_to_the_id_path_with_bearer_auth_and_no_confirm_token() {
    let api = ApiClient::new_for_test(spawn_revoke_mock().await);
    let resp = api
        .revoke_session("11111111-1111-1111-1111-111111111111")
        .await
        .expect("mock request should succeed");

    assert_eq!(resp["method"], "DELETE", "must be a DELETE, matching the live router");
    assert_eq!(
        resp["id"], "11111111-1111-1111-1111-111111111111",
        "the session id must be interpolated into the path, not sent as a body/query param"
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
        "must NOT send X-Confirm-Token — the live revoke_session handler takes only AuthUser, \
         no ConfirmedAction extractor (deviation eng-0481): {headers}"
    );
}

#[tokio::test]
async fn revoke_all_other_sessions_sends_post_with_bearer_auth_and_no_confirm_token() {
    let api = ApiClient::new_for_test(spawn_revoke_mock().await);
    let resp = api
        .revoke_all_other_sessions()
        .await
        .expect("mock request should succeed");

    assert_eq!(resp["method"], "POST", "must be a POST, matching the live router");
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
        "must NOT send X-Confirm-Token — the live revoke_all_other_sessions handler takes \
         only AuthUser, no ConfirmedAction extractor (deviation eng-0481): {headers}"
    );
}

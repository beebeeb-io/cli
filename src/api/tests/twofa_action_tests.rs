//! eng-0479: proves `totp_enable`/`totp_disable` actually PUT the code on
//! the wire in the shape the live server's `CodeRequest { code: String }`
//! (`repos/server/beebeeb-api/src/routes/totp.rs`) expects — `{"code":
//! "..."}` — not just that some HTTP call happens. Same in-process axum
//! mock pattern as `list_pagination_tests` / `client_header_tests` above:
//! the mock echoes the exact JSON body it received back to the caller so
//! the assertion is against bytes actually sent by `ApiClient`.

use axum::routing::post;
use axum::{Json, Router};
use serde_json::Value;

use super::ApiClient;

async fn spawn_body_echo_mock(path: &'static str) -> String {
    let app = Router::new().route(path, post(|Json(body): Json<Value>| async move { Json(body) }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn totp_enable_sends_code_as_the_sole_json_field() {
    let api = ApiClient::new_for_test(spawn_body_echo_mock("/api/v1/auth/2fa/enable").await);
    let echoed = api.totp_enable("654321").await.expect("mock request should succeed");
    assert_eq!(
        echoed,
        serde_json::json!({ "code": "654321" }),
        "enable body must be exactly {{\"code\": \"654321\"}} — got {echoed}"
    );
}

#[tokio::test]
async fn totp_disable_sends_code_as_the_sole_json_field() {
    let api = ApiClient::new_for_test(spawn_body_echo_mock("/api/v1/auth/2fa/disable").await);
    let echoed = api.totp_disable("111222").await.expect("mock request should succeed");
    assert_eq!(
        echoed,
        serde_json::json!({ "code": "111222" }),
        "disable body must be exactly {{\"code\": \"111222\"}} — got {echoed}"
    );
}

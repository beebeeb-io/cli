use super::ApiClient;
use axum::{Json, Router, routing::patch};
use serde_json::{Value, json};

// Echo the actual serialized PATCH body so assertions run in the test,
// rather than panicking inside an HTTP handler and hiding the failure.
async fn client() -> (ApiClient, tokio::task::JoinHandle<()>) {
    let app = Router::new().route(
        "/api/v1/files/:id",
        patch(|Json(body): Json<Value>| async { Json(body) }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (ApiClient::new_for_test(url), server)
}

#[tokio::test]
async fn root_move_serializes_explicit_null_with_and_without_rename() {
    let (api, server) = client().await;
    for name in [None, Some("encrypted-name")] {
        let body = api.move_file("item", name, Some(None)).await.unwrap();
        assert_eq!(
            body.get("parent_id"),
            Some(&Value::Null),
            "root move must contain an explicit parent_id: null"
        );
        assert_eq!(body.get("name_encrypted"), name.map(|s| json!(s)).as_ref());
        assert_eq!(body.as_object().unwrap().len(), 1 + usize::from(name.is_some()));
    }
    server.abort();
}

#[tokio::test]
async fn folder_move_serializes_uuid_with_and_without_rename() {
    let (api, server) = client().await;
    let parent = uuid::Uuid::new_v4();
    for name in [None, Some("encrypted-name")] {
        let body = api.move_file("item", name, Some(Some(parent))).await.unwrap();
        assert_eq!(body.get("parent_id"), Some(&json!(parent)));
        assert_eq!(body.get("name_encrypted"), name.map(|s| json!(s)).as_ref());
        assert_eq!(body.as_object().unwrap().len(), 1 + usize::from(name.is_some()));
    }
    server.abort();
}

#[tokio::test]
async fn rename_only_omits_parent_id() {
    let (api, server) = client().await;
    let body = api.move_file("item", Some("encrypted-name"), None).await.unwrap();
    assert_eq!(body, json!({"name_encrypted": "encrypted-name"}));
    assert!(body.get("parent_id").is_none(), "rename-only must omit parent_id");
    server.abort();
}

/// The unit-test half of the "a test never reaches production" guard (the other half is
/// `BB_REFUSE_NON_LOOPBACK_API`, see `tests/api_guard.rs`): a test client cannot be built for a
/// server that is not on this machine.
#[test]
#[should_panic(expected = "a test must point the client at a local server")]
fn a_test_client_cannot_be_pointed_at_a_public_server() {
    let _ = ApiClient::new_for_test("https://api.beebeeb.io".to_string());
}

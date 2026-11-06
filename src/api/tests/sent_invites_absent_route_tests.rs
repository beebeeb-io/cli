//! Task 2334, delta review P2-1: what a non-answer from `GET /api/v1/shares/invites/sent` means depends on the
//! question. For a push's share check, 404/405/501 is a server without the route (`Value::Null`, read there as
//! "unknown"). For key resolution (`list_sent_invites_for_keys`) only 405 or 501 says the route is absent: a 404
//! can come from a proxy or a misrouted node, so it is an error, which `OwnedKeyResolver` counts as a read that
//! did not answer (`NotNow`), never as "no invite" (which would make a legacy-invite folder's rows unopenable for
//! good and let a push pass over them).

use axum::Router;
use axum::http::StatusCode;
use axum::routing::get;

use super::ApiClient;

/// A server that answers the sent-invite route with `status` and nothing else.
async fn answering(status: u16) -> ApiClient {
    let app = Router::new().route(
        "/api/v1/shares/invites/sent",
        get(move || async move { (StatusCode::from_u16(status).unwrap(), "{}") }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    ApiClient::new_for_test(base)
}

#[tokio::test]
async fn for_key_resolution_a_404_is_a_read_that_did_not_answer() {
    let err = answering(404).await.list_sent_invites_for_keys().await.unwrap_err();
    assert_eq!(err.status, 404, "{}", err.message);
}

#[tokio::test]
async fn for_key_resolution_only_405_or_501_says_the_route_is_absent() {
    for status in [405, 501] {
        let answer = answering(status).await.list_sent_invites_for_keys().await.unwrap();
        assert!(answer.is_null(), "{status}: {answer}");
    }
}

#[tokio::test]
async fn for_a_push_404_405_and_501_all_say_the_route_is_absent() {
    for status in [404, 405, 501] {
        let answer = answering(status).await.list_sent_invites().await.unwrap();
        assert!(answer.is_null(), "{status}: {answer}");
    }
}

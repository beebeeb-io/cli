//! Task 1037: an account without a plan is refused at upload init (and
//! share create) with `409 { error: "plan_required" | "account_lapsed" }`
//! — or, on the older path, `413 quota_exceeded`. `upload_init_typed` /
//! `create_share` turn that into the needs_plan / lapsed notice. The
//! subscription is fetched only on the FAILURE path (a successful init
//! never costs an extra round-trip), only when the code alone can't say
//! everything (`plan_required` needs no lookup), and at most once per
//! `ApiClient` (a `bb sync` of 1000 files doesn't fetch 1000 times).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Value, json};

use super::ApiClient;

/// A mock reply: `None` = success, `Some((status, body))` = refusal.
type Reply = Option<(StatusCode, Value)>;

fn plan_required() -> Reply {
    Some((
        StatusCode::CONFLICT,
        json!({ "error": "plan_required", "message": "Choose a plan to upload files." }),
    ))
}

fn account_lapsed() -> Reply {
    Some((
        StatusCode::CONFLICT,
        json!({ "error": "account_lapsed", "message": "Your account is read-only." }),
    ))
}

fn quota_exceeded() -> Reply {
    // The live server's QuotaExceeded body: no `message`.
    Some((
        StatusCode::PAYLOAD_TOO_LARGE,
        json!({ "error": "quota_exceeded", "limit_bytes": 0, "used_bytes": 0 }),
    ))
}

fn lapsed_sub() -> Value {
    json!({ "plan": "pro", "effective_plan": "none", "account_state": "lapsed",
            "data_deletion_at": "2026-11-27T10:00:00Z", "quota_bytes": 0 })
}

/// `refusal` answers both `POST /uploads/init` and `POST /shares`;
/// `sub`: the subscription body, `None` ⇒ the GET fails with a 500.
/// Returns (base_url, subscription GET counter).
async fn spawn_mock(refusal: Reply, sub: Option<Value>) -> (String, Arc<AtomicUsize>) {
    let sub_calls = Arc::new(AtomicUsize::new(0));
    let counter = sub_calls.clone();
    let init_refusal = refusal.clone();
    let app = Router::new()
        .route(
            "/api/v1/uploads/init",
            post(move || async move {
                match init_refusal {
                    None => (
                        StatusCode::OK,
                        Json(json!({
                            "upload_session_id": "11111111-1111-1111-1111-111111111111",
                            "file_id": "22222222-2222-2222-2222-222222222222",
                            "chunk_size_bytes": 10,
                            "chunk_count": 1,
                        })),
                    ),
                    Some((status, body)) => (status, Json(body)),
                }
            }),
        )
        .route(
            "/api/v1/shares",
            post(move || async move {
                match refusal {
                    None => (StatusCode::OK, Json(json!({ "id": "s1" }))),
                    Some((status, body)) => (status, Json(body)),
                }
            }),
        )
        .route(
            "/api/v1/billing/subscription",
            get(move || {
                let counter = counter.clone();
                let sub = sub.clone();
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    match sub {
                        Some(sub) => (StatusCode::OK, Json(sub)),
                        None => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": "boom" }))),
                    }
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), sub_calls)
}

async fn init(api: &ApiClient) -> Result<super::UploadInit, super::ApiError> {
    api.upload_init_typed(None, "name", None, 10, 10, 1, false, None).await
}

async fn share(api: &ApiClient) -> Result<Value, super::ApiError> {
    let material = crate::commands::share::ShareMaterial {
        token: "t".into(),
        wrapped_file_key: "w".into(),
        owner_wrapped_key: "o".into(),
        owner_wrapped_token: "ot".into(),
        key_fragment: "k".into(),
    };
    api.create_share("22222222-2222-2222-2222-222222222222", None, None, None, &material)
        .await
}

// The mock API is on 127.0.0.1 → the local dev web app, never prod.
const NEEDS_PLAN: &str = "Your account has no plan yet \u{2014} choose one at http://localhost:5173/choose-plan";
const LAPSED_DATED: &str = "Your trial has ended; your vault is read-only and will be deleted on \
                            November 27, 2026. Subscribe at http://localhost:5173/billing?view=change";

#[tokio::test]
async fn plan_required_409_is_explained_without_a_lookup() {
    let (base, sub_calls) = spawn_mock(plan_required(), Some(lapsed_sub())).await;
    let api = ApiClient::new_for_test(base);
    let err = init(&api).await.unwrap_err();
    assert_eq!(err.message, NEEDS_PLAN);
    // Code + status are kept for any caller that classifies on them.
    assert!(err.is_code("plan_required"));
    assert_eq!(err.status, 409);
    assert_eq!(
        sub_calls.load(Ordering::SeqCst),
        0,
        "plan_required needs no subscription lookup"
    );
}

#[tokio::test]
async fn account_lapsed_409_is_explained_with_the_deletion_date_fetched_once() {
    let (base, sub_calls) = spawn_mock(account_lapsed(), Some(lapsed_sub())).await;
    let api = ApiClient::new_for_test(base);
    for _ in 0..3 {
        let err = init(&api).await.unwrap_err();
        assert_eq!(err.message, LAPSED_DATED);
        assert!(err.is_code("account_lapsed"));
        assert_eq!(err.status, 409);
    }
    assert_eq!(
        sub_calls.load(Ordering::SeqCst),
        1,
        "account state must be fetched once per client"
    );
}

#[tokio::test]
async fn account_lapsed_409_still_explains_when_the_lookup_fails() {
    let (base, _) = spawn_mock(account_lapsed(), None).await;
    let api = ApiClient::new_for_test(base);
    let err = init(&api).await.unwrap_err();
    assert_eq!(
        err.message,
        "Your trial has ended; your vault is read-only and will be deleted 60 days after it lapsed. \
         Subscribe at http://localhost:5173/billing?view=change"
    );
}

#[tokio::test]
async fn quota_413_is_still_explained_from_the_subscription() {
    let (base, sub_calls) = spawn_mock(quota_exceeded(), Some(lapsed_sub())).await;
    let api = ApiClient::new_for_test(base);
    let err = init(&api).await.unwrap_err();
    assert_eq!(err.message, LAPSED_DATED);
    assert!(err.is_code("quota_exceeded"));
    assert_eq!(err.status, 413);
    assert_eq!(sub_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn quota_413_on_an_ok_account_keeps_the_servers_error() {
    // Missing account_state (pre-1037 server) ⇒ ok ⇒ nothing to explain.
    let (base, _) = spawn_mock(quota_exceeded(), Some(json!({ "plan": "pro", "quota_bytes": 1000 }))).await;
    let api = ApiClient::new_for_test(base);
    let err = init(&api).await.unwrap_err();
    assert_eq!(err.message, "quota_exceeded");
}

#[tokio::test]
async fn an_unrelated_409_is_not_rewritten_and_costs_no_lookup() {
    let (base, sub_calls) = spawn_mock(
        Some((
            StatusCode::CONFLICT,
            json!({ "error": "upload is already in progress for this file" }),
        )),
        Some(lapsed_sub()),
    )
    .await;
    let api = ApiClient::new_for_test(base);
    let err = init(&api).await.unwrap_err();
    assert_eq!(err.message, "upload is already in progress for this file");
    assert_eq!(sub_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_successful_init_never_fetches_the_subscription() {
    let (base, sub_calls) = spawn_mock(None, Some(lapsed_sub())).await;
    let api = ApiClient::new_for_test(base);
    init(&api).await.expect("init should succeed");
    assert_eq!(
        sub_calls.load(Ordering::SeqCst),
        0,
        "no extra round-trip on the happy path"
    );
}

#[tokio::test]
async fn share_create_maps_plan_required_and_account_lapsed() {
    let (base, _) = spawn_mock(plan_required(), Some(lapsed_sub())).await;
    assert_eq!(share(&ApiClient::new_for_test(base)).await.unwrap_err().message, NEEDS_PLAN);

    let (base, _) = spawn_mock(account_lapsed(), Some(lapsed_sub())).await;
    assert_eq!(share(&ApiClient::new_for_test(base)).await.unwrap_err().message, LAPSED_DATED);
}

#[tokio::test]
async fn a_successful_share_never_fetches_the_subscription() {
    let (base, sub_calls) = spawn_mock(None, Some(lapsed_sub())).await;
    share(&ApiClient::new_for_test(base))
        .await
        .expect("share should succeed");
    assert_eq!(sub_calls.load(Ordering::SeqCst), 0);
}

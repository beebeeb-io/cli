//! Account lifecycle and storage failure checks for the shared resolver.
#[path = "support/shared_folder.rs"]
mod sf;
use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use serde_json::{Value, json};
use sf::*;

fn replay(f: &mut Fixture) {
    f.legacy_invite = true;
    f.custody_status = Some(404);
    f.inventory[0]["encrypted_file_key"] =
        json!(B64.encode(beebeeb_core::encrypt::encrypt_chunk_raw(&folder_key(), item_key().as_bytes()).unwrap()));
    f.rows[1]["owner_wrapped_file_key"] = Value::Null;
    f.requests.clear();
}
fn ok(out: std::process::Output) {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn account_rebind_clears_previous_observations_without_inheriting_them() {
    let h = Harness::new(Fixture::transfer(false)).await;
    ok(h.run(&["ls", "Shared"]).await);
    assert_eq!(h.custody_state()["custody_seen"], json!([ROOT]));
    {
        let mut f = h.state.lock().unwrap();
        f.account_id = Some(BAD.into());
        replay(&mut f);
    }
    let output = h.home.join("other-account.txt");
    ok(h.run(&["pull", FILE, "-o", output.to_str().unwrap()]).await);
    assert_eq!(std::fs::read(output).unwrap(), h.state.lock().unwrap().plaintext);
    assert!(
        !h.account_state_path(ACCOUNT).exists(),
        "rebind must clear the old account"
    );
    assert!(
        !h.account_state_path(BAD).exists(),
        "never-seen account must not inherit observations"
    );
    h.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn logout_offline_clears_only_current_account_observations() {
    let mut h = Harness::new(Fixture::transfer(false)).await;
    ok(h.run(&["ls", "Shared"]).await);
    assert_eq!(h.custody_state()["custody_seen"], json!([ROOT]));
    let other = json!({"account_id":BAD,"custody_seen":[SUB]}).to_string();
    std::fs::write(h.account_state_path(BAD), &other).unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    h.api = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    ok(h.run(&["logout"]).await);
    assert!(!h.account_state_path(ACCOUNT).exists());
    assert!(
        !h.account_state_path(ACCOUNT)
            .parent()
            .unwrap()
            .join("active.json")
            .exists()
    );
    assert_eq!(std::fs::read_to_string(h.account_state_path(BAD)).unwrap(), other);
    h.close().await;
}

/// The store fails closed when it cannot be read, so signing out is the only way to repair it. A damaged
/// binding must therefore never be able to stop `logout` (or `login`, which clears through the same call).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unreadable_binding_does_not_trap_logout() {
    let mut h = Harness::new(Fixture::transfer(false)).await;
    ok(h.run(&["ls", "Shared"]).await);
    let binding = h.account_state_path(ACCOUNT).parent().unwrap().join("active.json");
    std::fs::write(&binding, b"broken-json").unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    h.api = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    ok(h.run(&["logout"]).await);
    assert!(!binding.exists(), "an unreadable binding must be removed by sign-out");
    h.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn corrupt_account_store_refuses_before_legacy_or_content() {
    let h = Harness::new(Fixture::transfer(false)).await;
    ok(h.run(&["ls", "Shared"]).await);
    assert_eq!(h.custody_state()["custody_seen"], json!([ROOT]));
    std::fs::write(h.account_state_path(ACCOUNT), b"broken-json").unwrap();
    replay(&mut h.state.lock().unwrap());
    let output = h.home.join("corrupt.txt");
    let out = h.run(&["pull", FILE, "-o", output.to_str().unwrap()]).await;
    refusal(&h, &out, &output);
    assert_eq!(h.count("GET", &format!("/api/v1/shares/invites/{BAD}/folder-keys")), 0);
    h.close().await;
}

struct BlockWrite {
    upstream: String,
    record: std::path::PathBuf,
}
async fn block_write(
    axum::extract::State(block): axum::extract::State<std::sync::Arc<BlockWrite>>,
    req: axum::extract::Request,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let response = reqwest::Client::new()
        .request(req.method().clone(), format!("{}{}", block.upstream, req.uri()))
        .send()
        .await
        .unwrap();
    // The resolver already checked the empty store, and will now open L6.
    // Replacing the destination with a directory makes persistence fail.
    if req.uri().path() == format!("/api/v1/shared-folders/{ROOT}") {
        std::fs::create_dir(&block.record).unwrap();
    }
    let status = response.status();
    (status, response.bytes().await.unwrap()).into_response()
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_observation_write_refuses_before_inventory_or_content() {
    let mut h = Harness::new(Fixture::transfer(false)).await;
    let block = std::sync::Arc::new(BlockWrite {
        upstream: h.api.clone(),
        record: h.account_state_path(ACCOUNT),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    h.api = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, axum::Router::new().fallback(block_write).with_state(block))
            .await
            .unwrap();
    });
    let output = h.home.join("uncommitted.txt");
    let out = h.run(&["pull", FILE, "-o", output.to_str().unwrap()]).await;
    server.abort();
    let _ = server.await;
    assert_eq!(
        h.count("GET", &format!("/api/v1/shared-folders/{ROOT}")),
        1,
        "instrument must reach L6"
    );
    refusal(&h, &out, &output);
    assert_eq!(h.count("GET", &format!("/api/v1/shared-folders/{ROOT}/keys")), 0);
    h.close().await;
}
fn refusal(h: &Harness, out: &std::process::Output, output: &std::path::Path) {
    assert!(!out.status.success(), "accepted unusable security state");
    assert!(String::from_utf8_lossy(&out.stderr).contains("This folder's keys are unavailable"));
    assert!(!output.exists());
    let f = h.state.lock().unwrap();
    assert_eq!(f.requests.iter().filter(|(m, _, _)| m != "GET").count(), 0);
    assert_eq!(
        f.requests.iter().filter(|(_, p, _)| p.ends_with("/download")).count(),
        0
    );
}
impl Harness {
    async fn run(&self, args: &[&str]) -> std::process::Output {
        tokio::process::Command::new(env!("CARGO_BIN_EXE_bb"))
            .args(args)
            .args(["--api", &self.api, "--json"])
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("BB_REFUSE_NON_LOOPBACK_API", "1")
            .env("BB_NO_UPDATE", "1")
            .env("BB_TRANSPORT", "api")
            .env_remove("APP_URL")
            .output()
            .await
            .unwrap()
    }
}

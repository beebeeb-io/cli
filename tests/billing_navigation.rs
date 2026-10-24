//! Billing navigation stays in the Beebeeb web app. Both public aliases run
//! against a mock that would return a Mollie URL if any API call were made.
//! Each child has an isolated config and cannot open a browser.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::{Json, Router};
use serde_json::json;

static SEQUENCE: AtomicUsize = AtomicUsize::new(0);

struct Fixture {
    home: PathBuf,
    api: String,
    requests: Arc<AtomicUsize>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Fixture {
    fn new(authenticated: bool) -> Self {
        let home = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!(
            "billing-navigation-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&home).unwrap();
        let requests = Arc::new(AtomicUsize::new(0));
        let seen = requests.clone();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let thread = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                ready_tx.send(listener.local_addr().unwrap()).unwrap();
                let app = Router::new().fallback(move || {
                    seen.fetch_add(1, Ordering::SeqCst);
                    async { Json(json!({ "url": "https://www.mollie.com/checkout/fixture" })) }
                });
                axum::serve(listener, app)
                    .with_graceful_shutdown(async {
                        let _ = stopped.await;
                    })
                    .await
                    .unwrap();
            });
        });
        let api = format!("http://{}", ready_rx.recv().unwrap());
        let mut config = json!({ "api_url": api });
        if authenticated {
            config["session_token"] = json!("billing-navigation-fixture-token");
        }
        for dir in [
            home.join("Library/Application Support/beebeeb"),
            home.join(".config/beebeeb"),
            home.join("AppData/Roaming/beebeeb"),
        ] {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("config.json"), serde_json::to_vec(&config).unwrap()).unwrap();
        }
        Self {
            home,
            api,
            requests,
            stop: Some(stop),
            thread: Some(thread),
        }
    }

    fn run(&self, args: &[&str], app_url: Option<&str>) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_bb"));
        command
            .args(args)
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("APPDATA", self.home.join("AppData/Roaming"))
            .env("LOCALAPPDATA", self.home.join("AppData/Local"))
            .env("BB_NO_UPDATE", "1")
            .env("BB_LOGIN_HEADLESS", "1")
            .env("NO_COLOR", "1")
            .env_remove("APP_URL")
            .stdin(Stdio::null());
        if let Some(url) = app_url {
            command.env("APP_URL", url);
        }
        command.output().expect("run bb")
    }

    fn assert_navigation(&self, args: &[&str], app_url: Option<&str>, expected: &str) {
        let output = self.run(args, app_url);
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert_eq!(
            output.status.code(),
            Some(0),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            self.requests.load(Ordering::SeqCst),
            0,
            "billing navigation must not create a payment session"
        );
        assert!(stdout.contains(expected), "expected {expected}, stdout: {stdout}");
        assert!(!stdout.contains("mollie.com"), "provider URL escaped: {stdout}");
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.stop.take().unwrap().send(());
        self.thread.take().unwrap().join().unwrap();
        std::fs::remove_dir_all(&self.home).unwrap();
    }
}

#[test]
fn account_billing_opens_the_web_billing_page_without_a_payment_request() {
    let fixture = Fixture::new(true);
    fixture.assert_navigation(
        &["account", "billing"],
        Some("https://app.beebeeb.io/"),
        "https://app.beebeeb.io/billing",
    );
}

#[test]
fn billing_portal_opens_the_web_billing_page_without_a_payment_request() {
    let fixture = Fixture::new(true);
    fixture.assert_navigation(
        &["billing", "portal"],
        Some("https://app.beebeeb.io/"),
        "https://app.beebeeb.io/billing",
    );
}

#[test]
fn billing_navigation_derives_the_local_web_app() {
    let fixture = Fixture::new(true);
    fixture.assert_navigation(&["account", "billing"], None, "http://localhost:5173/billing");
}

#[test]
fn billing_navigation_honors_the_explicit_web_app() {
    let fixture = Fixture::new(true);
    fixture.assert_navigation(
        &["billing", "portal"],
        Some("https://web.example.test/custom/"),
        "https://web.example.test/custom/billing",
    );
}

#[test]
fn billing_navigation_honors_the_api_override() {
    let fixture = Fixture::new(true);
    // A stale configured API must not win over the command-line override.
    for dir in [
        "Library/Application Support/beebeeb",
        ".config/beebeeb",
        "AppData/Roaming/beebeeb",
    ] {
        let path = fixture.home.join(dir).join("config.json");
        let mut config: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        config["api_url"] = json!("https://api.staging.example.test");
        std::fs::write(path, serde_json::to_vec(&config).unwrap()).unwrap();
    }
    fixture.assert_navigation(
        &["--api", &fixture.api, "account", "billing"],
        None,
        "http://localhost:5173/billing",
    );
}

#[test]
fn billing_navigation_still_requires_local_authentication() {
    let fixture = Fixture::new(false);
    for args in [["account", "billing"], ["billing", "portal"]] {
        let output = fixture.run(&args, None);
        assert_ne!(output.status.code(), Some(0), "unauthenticated billing should fail");
        assert!(String::from_utf8_lossy(&output.stderr).contains("Not logged in"));
    }
    assert_eq!(fixture.requests.load(Ordering::SeqCst), 0);
}

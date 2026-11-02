use bytes::Bytes;
use reqwest::Client;
use serde_json::Value;
use std::error::Error;
use std::sync::atomic::{AtomicI64, Ordering};

use uuid::Uuid;

use crate::config::load_config;

mod direct;
pub(crate) use direct::{ChunkApiError, DirectOffer, GrantReply, UploadCall, now_ms};

static RATE_REMAINING: AtomicI64 = AtomicI64::new(-1);
static RATE_LIMIT: AtomicI64 = AtomicI64::new(-1);
static RATE_RESET: AtomicI64 = AtomicI64::new(0);

fn update_rate_state(headers: &reqwest::header::HeaderMap) {
    if let Some(v) = headers.get("x-ratelimit-remaining").and_then(|v| v.to_str().ok()) {
        if let Ok(n) = v.parse::<i64>() {
            RATE_REMAINING.store(n, Ordering::Relaxed);
        }
    }
    if let Some(v) = headers.get("x-ratelimit-limit").and_then(|v| v.to_str().ok()) {
        if let Ok(n) = v.parse::<i64>() {
            RATE_LIMIT.store(n, Ordering::Relaxed);
        }
    }
    if let Some(v) = headers.get("x-ratelimit-reset").and_then(|v| v.to_str().ok()) {
        if let Ok(n) = v.parse::<i64>() {
            RATE_RESET.store(n, Ordering::Relaxed);
        }
    }
}

async fn pace_if_needed() {
    let remaining = RATE_REMAINING.load(Ordering::Relaxed);
    let limit = RATE_LIMIT.load(Ordering::Relaxed);
    let reset = RATE_RESET.load(Ordering::Relaxed);
    if remaining < 0 || limit <= 0 {
        return;
    }
    let ratio = remaining as f64 / limit as f64;
    if ratio > 0.2 {
        return;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let secs_until_reset = (reset - now).max(1);
    let requests_left = remaining.max(1);
    let pace_ms = ((secs_until_reset as f64 / requests_left as f64) * 1000.0).min(5000.0) as u64;
    if pace_ms > 50 {
        tokio::time::sleep(std::time::Duration::from_millis(pace_ms)).await;
    }
}

/// Shown for any 401 the server answers with its generic `unauthorized` body
/// (no human `message` of its own): the stored session token is expired or was
/// revoked, and the only fix is signing in again. Replaces the bare
/// `error: unauthorized` the CLI used to print (flow "CLI end to end", issue 6).
/// Stable code for the LOCALLY detected wrong step-up password (OPAQUE
/// `client_login_finish` failed). Distinct from the server's `unauthorized`,
/// which on the step-up endpoints means a dead session (task 0856 r2).
pub(crate) const OPAQUE_PASSWORD_MISMATCH: &str = "opaque_password_mismatch";

/// Which step-up method minted a confirmation token (task 1907). Learned from
/// the step-up itself so a caller can tell an OPAQUE account from a legacy one
/// without sending the password to an endpoint that would have to guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmMethod {
    /// `confirm-opaque-start` succeeded: the account has an OPAQUE credential.
    Opaque,
    /// `confirm-opaque-start` answered `409 opaque_unavailable`; the token came
    /// from the legacy plaintext `POST /auth/confirm`.
    LegacyPlaintext,
}

/// What a call that needs a login says when there is none.
pub(crate) const NOT_LOGGED_IN_MESSAGE: &str = "Not logged in. Run `bb login` first.";

pub(crate) const SESSION_EXPIRED_MESSAGE: &str =
    "Your session expired or was revoked. Run `bb login` to sign in again.";

/// The `scheme://host[:port]` a failed request was aimed at — what the user
/// configured (`api_url` / `--api`), without the route path.
fn request_origin(error: &reqwest::Error) -> Option<String> {
    let url = error.url()?;
    let host = url.host_str()?;
    Some(match url.port() {
        Some(port) => format!("{}://{host}:{port}", url.scheme()),
        None => format!("{}://{host}", url.scheme()),
    })
}

fn format_request_error(error: reqwest::Error) -> String {
    // A connect failure (refused, DNS, unreachable network) has one useful
    // reading for a user: the server could not be reached. Name the server so
    // a wrong `--api` / `api_url` is obvious, and skip reqwest's error chain.
    if error.is_connect() {
        return match request_origin(&error) {
            Some(origin) => format!("Can't reach {origin} \u{2014} check your connection or --api"),
            None => "Can't reach the Beebeeb API \u{2014} check your connection or --api".to_string(),
        };
    }
    let mut message = format!("request failed: {error}");
    let mut source = error.source();
    while let Some(err) = source {
        message.push_str(&format!(": {err}"));
        source = err.source();
    }
    message
}

/// Transport-level failures that are safe to retry on an **idempotent** request.
/// Covers connection refused/reset, timeouts, and a connection dropped
/// mid-flight (broken pipe / incomplete message / premature EOF). Deliberately
/// does NOT cover builder errors or HTTP error statuses (those are handled by
/// `parse_response` and must fail fast).
fn is_transient_transport_error(err: &reqwest::Error) -> bool {
    if err.is_timeout() || err.is_connect() {
        return true;
    }
    // reqwest doesn't expose typed predicates for reset/EOF, so scan the source
    // chain for the usual hyper/io messages. Only request/body errors qualify —
    // a builder error won't fix itself on retry.
    if err.is_request() || err.is_body() {
        let mut src: Option<&(dyn Error + 'static)> = Some(err);
        while let Some(e) = src {
            let s = e.to_string().to_lowercase();
            if s.contains("connection reset")
                || s.contains("connection closed")
                || s.contains("connection aborted")
                || s.contains("broken pipe")
                || s.contains("incomplete")
                || s.contains("unexpected end")
                || s.contains("end of file")
                || s.contains("eof")
            {
                return true;
            }
            src = e.source();
        }
    }
    false
}

/// The chunk-upload retry policy (spec move 6): core owns the ladder, the CLI
/// keeps the numbers it has always shipped — 200 ms base doubling per attempt
/// under a 5 s cap, five attempts total.
const CHUNK_RETRY: beebeeb_core::retry::RetryPolicy = beebeeb_core::retry::RetryPolicy {
    max_attempts: 5,
    base_ms: 200,
    cap_ms: 5_000,
    jitter: beebeeb_core::retry::Jitter::None,
};

pub struct ApiClient {
    client: Client,
    base_url: String,
    token: Option<String>,
    /// Task 1037: the account state off `GET /billing/subscription`, fetched
    /// lazily on the first account-state refusal and reused for every later
    /// one this client sees (`None`: the fetch failed). See
    /// `explain_account_refusal`.
    refusal_account_state: tokio::sync::OnceCell<Option<crate::account_state::AccountState>>,
    /// Task 2173: what the server's direct-transfer discovery said, asked
    /// lazily and at most once per 10 minutes per API base (see
    /// `beebeeb_core::direct_transfer::DiscoveryCache` for the rules).
    direct: std::sync::Arc<direct::DirectState>,
    /// Task 2173: the HTTP client for presigned storage links. A client of its
    /// own, never `client`: no default headers (so no `X-Beebeeb-*`), no
    /// cookies, no `Authorization`, redirects refused, https only (loopback
    /// http in development).
    storage_client: std::sync::OnceLock<Option<Client>>,
    /// Task 2173: the same, for storage PUTs. A client of its own again, because a PUT sends a body of up to
    /// 128 MiB and the download client's read timeout is not reset while a request body goes out (see
    /// `build_upload_storage_client`).
    upload_storage_client: std::sync::OnceLock<Option<Client>>,
}

/// Parsed response from `POST /api/v1/uploads/init`. The session id keys the
/// subsequent chunk PUTs + complete; the durable `file_id` identifies the file
/// (and survives version replaces). `chunk_size_bytes` / `chunk_count` are the
/// server-derived plan — the client frames its upload from these, not from its
/// own hint.
#[derive(Debug, Clone)]
pub struct UploadInit {
    pub upload_session_id: String,
    pub file_id: String,
    pub chunk_size_bytes: u64,
    pub chunk_count: u64,
    /// The `transport` field of the 201 body (task 2173): `object-upload-v1` when the server
    /// opened a direct session, absent otherwise (today's chunk PUT route).
    pub transport: Option<String>,
}

impl UploadInit {
    /// Did the server select the direct upload for this session? Core decides what the field means
    /// (`dt::upload_init_selects_direct`): any other value, or none, is today's route.
    pub fn is_direct(&self) -> bool {
        beebeeb_core::direct_transfer::upload_init_selects_direct(self.transport.as_deref())
    }

    fn from_value(v: &Value) -> Result<Self, ApiError> {
        let upload_session_id = v
            .get("upload_session_id")
            .and_then(|x| x.as_str())
            .ok_or("upload init response missing upload_session_id")?
            .to_string();
        let file_id = v
            .get("file_id")
            .or_else(|| v.get("id"))
            .and_then(|x| x.as_str())
            .ok_or("upload init response missing file_id")?
            .to_string();
        let chunk_size_bytes = v.get("chunk_size_bytes").and_then(|x| x.as_u64()).unwrap_or(0);
        let chunk_count = v.get("chunk_count").and_then(|x| x.as_u64()).unwrap_or(0);
        let transport = v.get("transport").and_then(|x| x.as_str()).map(str::to_string);
        Ok(Self {
            upload_session_id,
            file_id,
            chunk_size_bytes,
            chunk_count,
            transport,
        })
    }
}

/// Hard outer bound on one listing walk, so a pathological/hostile listing can't
/// loop unbounded or exhaust memory. Mirrors web `FILE_LIST_HARD_CAP`.
const FILE_LIST_HARD_CAP: usize = 50_000;

/// After a page is appended: has the walk reached `cap`? `Some(truncated)` ends
/// the walk; `None` keeps following the cursor. A truncated listing is not fit
/// to conclude deletions from (task 1915), so it is truncated only when entries
/// were discarded or a cursor remains: exactly `cap` entries on an exhausted
/// cursor is the complete folder.
fn listing_cap_reached(all: &mut Vec<Value>, cap: usize, more_pages: bool) -> Option<bool> {
    if all.len() < cap {
        return None;
    }
    let truncated = all.len() > cap || more_pages;
    all.truncate(cap);
    Some(truncated)
}

/// Writer-provenance headers sent on every request: `X-Beebeeb-Client` names
/// this client (`cli`), `X-Beebeeb-Client-Version` carries the exact string
/// `bb --version` prints (`CARGO_PKG_VERSION`, via clap's `#[command(version)]`
/// with no override — see `src/main.rs`). The server records both on every
/// `object_versions` row (server PR #23 / task 1369) so a blast-radius query
/// can tell which client wrote a version. No user data goes in either value.
const CLIENT_HEADER_NAME: &str = "X-Beebeeb-Client";
const CLIENT_HEADER_VALUE: &str = "cli";
const CLIENT_VERSION_HEADER_NAME: &str = "X-Beebeeb-Client-Version";
const CLIENT_VERSION_HEADER_VALUE: &str = env!("CARGO_PKG_VERSION");

/// Builds the shared `reqwest::Client` with the writer-provenance headers set
/// as `default_headers`, so every request made through it — real traffic and
/// tests alike — carries them without each call site adding them by hand.
fn build_client(timeout: std::time::Duration) -> Client {
    use reqwest::header::{HeaderMap, HeaderValue};
    let mut headers = HeaderMap::new();
    headers.insert(CLIENT_HEADER_NAME, HeaderValue::from_static(CLIENT_HEADER_VALUE));
    headers.insert(
        CLIENT_VERSION_HEADER_NAME,
        HeaderValue::from_static(CLIENT_VERSION_HEADER_VALUE),
    );
    Client::builder()
        .timeout(timeout)
        .default_headers(headers)
        .build()
        .unwrap_or_else(|_| Client::new())
}

mod client;
mod id;
mod shares;
mod tree;
mod upload;
mod versions;

pub(crate) use id::{DirectUploadId, FileId, FolderId, ShareId};


/// A parsed API error: the server's stable machine `error` code (when
/// present) alongside the human `message` and HTTP status.
///
/// **Codex review on PR #33, task 1547 finding 1:** `parse_response` (below)
/// intentionally shows ONLY `message` — never the code — because finding 3
/// on this same task established that contract: the string a user sees must
/// be the server's clean human sentence, with nothing appended (see
/// `error_message_priority_tests`, which asserts the display string
/// byte-for-byte). But several call sites classify failures by the STABLE
/// CODE, not by the display text — `commands::confirm::map_confirm_error`
/// (`session_too_old_for_confirmation`), `commands::{passkey,sessions,
/// twofa}`'s `classify_*` helpers (`unauthorized`). A first attempt at this
/// fix appended the code to `message` as a `"{message} [{code}]"` suffix so
/// substring-matching still worked — that broke finding 3's contract for
/// every typed error whose message doesn't literally contain its code
/// (which is most of them; e.g. `billing_no_payment_method` → "No valid
/// payment method on file…" contains no such substring). Two different
/// contracts (clean display text vs. code-matchable) cannot both live in one
/// `String`, so the handful of call sites that need to classify by code use
/// `parse_response_typed` (below) instead and match `ApiError.code`
/// directly — exact, not substring — while every other call site keeps
/// using `parse_response` and its unchanged, code-free display string.
#[derive(Debug, Clone)]
pub struct ApiError {
    /// The server's stable `error` field, when the body had one. `None` for
    /// network failures, a non-JSON body, or a body with neither `error` nor
    /// `message`. `pub(crate)` (not `pub`) so `commands::*`'s `classify_*`
    /// helpers can match it directly and their tests can construct fixture
    /// values — this binary has no external consumers to hide it from.
    pub(crate) code: Option<String>,
    /// What to show the user: the server's `message`, else its `error`
    /// code, else a `"{status}: {body}"` fallback, else (network failures)
    /// the transport error text. Identical to what `parse_response` returns.
    pub(crate) message: String,
    /// HTTP status, or 0 for a failure that never got a response (read
    /// error, non-JSON body after a 2xx).
    pub(crate) status: u16,
}

impl ApiError {
    /// True if the server's stable `error` code equals `code` exactly. The
    /// primary way callers should classify a failure — exact, not
    /// substring, so a message that happens to mention another code's text
    /// can never cause a false match.
    pub(crate) fn is_code(&self, code: &str) -> bool {
        self.code.as_deref() == Some(code)
    }

    /// Build a fixture with only a `message` (no code) — for `classify_*`
    /// tests exercising the message-substring fallback path, and for the
    /// handful of local, never-hit-the-wire errors (`require_auth`,
    /// `format_request_error`) that have no server-issued code either.
    #[cfg(test)]
    pub(crate) fn test_message(message: &str) -> Self {
        Self {
            code: None,
            message: message.to_string(),
            status: 0,
        }
    }

    /// Build a fixture with a `code` (and matching `message`, the common
    /// real-world shape for a code-only error like `unauthorized`) — for
    /// `classify_*` tests exercising the exact-code path.
    #[cfg(test)]
    pub(crate) fn test_code(code: &str) -> Self {
        Self {
            code: Some(code.to_string()),
            message: code.to_string(),
            status: 0,
        }
    }
}

/// Lets the handful of methods returning `Result<_, ApiError>` keep using
/// `self.require_auth()?` and `.map_err(format_request_error)?` unchanged —
/// both already produce a plain `String`, and `?` converts it via this impl.
impl From<String> for ApiError {
    fn from(message: String) -> Self {
        Self {
            code: None,
            message,
            status: 0,
        }
    }
}

impl From<&str> for ApiError {
    fn from(message: &str) -> Self {
        Self::from(message.to_string())
    }
}

/// True when a v2 upload-session request failed because the server no longer
/// has that session (task 1589). Server PR #120 sweeps a session whose lease
/// expired: its chunk PUT / complete then answer **404** (the row is gone, or
/// kept as `status = 'expired'` on servers that map that to 404 too). Servers
/// from before that mapping answer **400** `upload session is not writable:
/// expired` for a kept-but-expired row — the same meaning, so it is matched
/// here too. Nothing else counts: a 409, a quota error or a 5xx is a real
/// failure the caller must surface, never a reason to re-init.
pub(crate) fn is_upload_session_gone(e: &ApiError) -> bool {
    // The server sends `{ error: <text> }` for the 400; `message` carries
    // it (it falls back to `error` when the body has no `message`). On the
    // proxy routes every 404 is the session.
    upload_session_gone(e.status, true, &e.message)
}

/// The rule behind [`is_upload_session_gone`] and the direct routes' own check, in one place so the two cannot
/// drift apart if the server rewords the 400. `a_404_counts`: whether a 404 means "the session is gone" (always,
/// on the proxy routes; only a JSON-bodied one on the direct routes, where a bare 404 is a node without the route).
pub(crate) fn upload_session_gone(status: u16, a_404_counts: bool, text: &str) -> bool {
    match status {
        404 => a_404_counts,
        400 => text.contains("upload session is not writable: expired"),
        _ => false,
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ApiError {}

/// For call sites that gather an `ApiError` alongside plain-`String`
/// `ApiClient` results (e.g. `commands::account::show`'s `tokio::join!`) and
/// need a uniform `Result<_, ApiError>` to hand to shared display code.
/// Drops the code — same display text `parse_response` would have returned.
impl From<ApiError> for String {
    fn from(e: ApiError) -> String {
        e.message
    }
}

/// `X-Beebeeb-Client-OS` value for this build (`macos|windows|linux`). Any
/// other OS is sent as `unknown`, which the server reads as unrecognised.
fn onboarding_os() -> &'static str {
    match std::env::consts::OS {
        "macos" => "macos",
        "windows" => "windows",
        "linux" => "linux",
        _ => "unknown",
    }
}

async fn parse_response(resp: reqwest::Response) -> Result<Value, ApiError> {
    parse_response_typed(resp).await
}

/// `{ "confirmation_token": ".." }` -> the token (both step-up endpoints answer this).
fn confirmation_token_from(body: Value) -> Result<String, ApiError> {
    body.get("confirmation_token")
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| "server did not return a confirmation_token".to_string().into())
}

/// Same wire parsing as `parse_response`, but keeps the server's stable
/// `error` code alongside the display `message` instead of discarding it.
/// See `ApiError`'s doc comment for why this is a separate function rather
/// than changing `parse_response` itself.
async fn parse_response_typed(resp: reqwest::Response) -> Result<Value, ApiError> {
    let status = resp.status();
    update_rate_state(resp.headers());

    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        let retry_after = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(5);
        let remaining = resp
            .headers()
            .get("x-ratelimit-remaining")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok());

        if !crate::ui::is_quiet() {
            use colored::Colorize;
            eprintln!(
                "  {} rate limited — pausing {}s{}",
                "⏸".custom_color(crate::colors::AMBER),
                retry_after,
                remaining.map_or(String::new(), |r| format!(" ({r} requests remaining)")),
            );
        }

        tokio::time::sleep(std::time::Duration::from_secs(retry_after)).await;
        // `parse_response`'s callers retry-loop on this exact sentinel
        // string (`e == "__rate_limited__"`) — `.map_err(|e| e.message)`
        // above preserves it byte-for-byte, so none of those call sites
        // need to change.
        return Err(ApiError {
            code: None,
            message: "__rate_limited__".to_string(),
            status: status.as_u16(),
        });
    }

    let body = resp.text().await.map_err(|e| ApiError {
        code: None,
        message: format!("failed to read response: {e}"),
        status: status.as_u16(),
    })?;

    if !status.is_success() {
        // One decoder for the server contract (spec move 7): the body is the
        // documented `{ error, message }` pair, and core keeps the stable code,
        // the display text (message, else the bare code, else status+body) and
        // the whole parsed body.
        let server = beebeeb_core::api_error::ServerError::parse_body(status.as_u16(), &body);
        let code = server.code.clone();
        // A 401 without a human `message` is the server's generic
        // `ApiError::Unauthorized` — the session token was rejected. Say so,
        // and say how to fix it. The stable `code` is kept untouched, so the
        // classify_* helpers that tell "wrong 2FA code" / "wrong password"
        // apart from a dead session (they match is_code("unauthorized"))
        // behave exactly as before.
        let message = if status == reqwest::StatusCode::UNAUTHORIZED
            && server.details.get("message").and_then(Value::as_str).is_none()
            && code.as_deref().is_none_or(|c| c == "unauthorized")
        {
            SESSION_EXPIRED_MESSAGE.to_string()
        } else {
            server.message
        };
        return Err(ApiError {
            code,
            message,
            status: status.as_u16(),
        });
    }

    serde_json::from_str(&body).map_err(|e| ApiError {
        code: None,
        message: format!("invalid JSON: {e}"),
        status: status.as_u16(),
    })
}

/// A file's CURRENT version number, from a `GET /api/v1/files/{id}` row. The
/// server reports it inside `download_version` (the version a download gets);
/// a top-level `version_number` is only a fallback for older servers. Task
/// 1951: reading only the top level made every `--replace` after the first send
/// base version 1 and fail as stale.
pub fn current_version_number(file: &Value) -> Option<i64> {
    file.get("download_version")
        .and_then(|d| d.get("version_number"))
        .and_then(Value::as_i64)
        .or_else(|| file.get("version_number").and_then(Value::as_i64))
}

#[cfg(test)]
mod current_version_number_tests {
    use super::current_version_number;
    use serde_json::json;

    #[test]
    fn reads_download_version_first_then_the_top_level() {
        // Today's server: only inside download_version.
        assert_eq!(
            current_version_number(&json!({"download_version": {"version_number": 3}})),
            Some(3)
        );
        // Both present: download_version wins.
        assert_eq!(
            current_version_number(&json!({"version_number": 1, "download_version": {"version_number": 7}})),
            Some(7)
        );
        // Older shape: top level only.
        assert_eq!(current_version_number(&json!({"version_number": 2})), Some(2));
        // A file with no completed version reports none.
        assert_eq!(current_version_number(&json!({"download_version": null})), None);
        assert_eq!(current_version_number(&json!({})), None);
    }
}

#[cfg(test)]
mod api_error_tests {
    use super::ApiError;

    /// `Display` is the user-facing text, unchanged from the old
    /// `Result<_, String>` contract, and `From<ApiError> for String` keeps it.
    #[test]
    fn display_is_the_message() {
        let e = ApiError::from("boom");
        assert_eq!(e.to_string(), "boom");
        assert_eq!(String::from(e), "boom");
    }

    /// A server code is matchable without leaking into the display text.
    #[test]
    fn code_is_matchable_but_not_displayed() {
        let e = ApiError::test_code("unauthorized");
        assert!(e.is_code("unauthorized"));
        assert_eq!(e.to_string(), "unauthorized");
    }
}

#[cfg(test)]
mod list_pagination_tests {
    //! Cursor-walk coverage for the `GET /api/v1/files` 200-cap fix (task 0755).
    //!
    //! Spins up an in-process axum mock that paginates the listing exactly like
    //! the real server (task 0739 keyset contract: `cursor` request param,
    //! `next_cursor` response field that is non-null ONLY while a full `limit`
    //! page is served, null on the last short page). The mock keys pages off an
    //! integer offset cursor over a fixed dataset, so the test proves the REAL
    //! `list_files` / `list_trashed` path (URL building, cursor threading, branch
    //! params, the walk loop) enumerates EVERY entry past the single-page cap,
    //! with no duplicates and no skips. We cannot cheaply seed >200 real files,
    //! so this mock-server integration test stands in for a live demo.

    use std::collections::HashMap;

    use axum::extract::Query;
    use axum::routing::get;
    use axum::{Json, Router};
    use serde_json::json;

    use super::ApiClient;

    const PAGE: usize = 200; // mirrors the server's default page size
    const ACTIVE_TOTAL: usize = 450; // > 2 full pages → 3 pages (200, 200, 50)
    const TRASHED_TOTAL: usize = 250; // > 1 full page → 2 pages (200, 50)

    /// One paginated `GET /api/v1/files` handler. `cursor` is an integer offset
    /// (string) into a `trashed`-selected dataset; `next_cursor` is emitted only
    /// when the page is full — exactly the server's `rows.len() == limit` rule.
    async fn spawn_mock() -> String {
        let app = Router::new().route(
            "/api/v1/files",
            get(|Query(q): Query<HashMap<String, String>>| async move {
                let trashed = q.get("trashed").map(|v| v == "true").unwrap_or(false);
                let total = if trashed { TRASHED_TOTAL } else { ACTIVE_TOTAL };
                let offset: usize = q.get("cursor").and_then(|c| c.parse().ok()).unwrap_or(0);
                let end = (offset + PAGE).min(total);
                let files: Vec<_> = (offset..end)
                    .map(|i| {
                        // Distinct, ordered ids so the test can assert no
                        // dup/skip. `trashed` namespaced so the branches differ.
                        let tag = if trashed { "trash" } else { "file" };
                        json!({ "id": format!("{tag}-{i:05}"), "is_folder": false })
                    })
                    .collect();
                // Full page → there may be more → hand back the next offset.
                let next_cursor = if files.len() == PAGE {
                    Some(end.to_string())
                } else {
                    None
                };
                Json(json!({ "files": files, "next_cursor": next_cursor }))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://{addr}")
    }

    fn ids(v: &serde_json::Value) -> Vec<String> {
        v.get("files")
            .and_then(|f| f.as_array())
            .unwrap()
            .iter()
            .map(|f| f.get("id").and_then(|x| x.as_str()).unwrap().to_string())
            .collect()
    }

    #[tokio::test]
    async fn list_files_walks_every_page_past_the_200_cap() {
        let api = ApiClient::new_for_test(spawn_mock().await);
        let got = ids(&api.list_files(None).await.unwrap());

        // The whole dataset, not a truncated first page.
        assert_eq!(
            got.len(),
            ACTIVE_TOTAL,
            "expected the full {ACTIVE_TOTAL}-entry listing, got {} (single-page truncation regressed)",
            got.len()
        );
        assert!(got.len() > PAGE, "test must exercise > one page");
        // No dup / no skip: ids are exactly file-00000..file-00449 in order.
        let expected: Vec<String> = (0..ACTIVE_TOTAL).map(|i| format!("file-{i:05}")).collect();
        assert_eq!(got, expected, "cursor walk dropped, duplicated, or reordered entries");
    }

    #[tokio::test]
    async fn list_trashed_walks_every_page() {
        let api = ApiClient::new_for_test(spawn_mock().await);
        let got = ids(&api.list_trashed().await.unwrap());

        assert_eq!(
            got.len(),
            TRASHED_TOTAL,
            "trashed listing was truncated at the page cap"
        );
        let expected: Vec<String> = (0..TRASHED_TOTAL).map(|i| format!("trash-{i:05}")).collect();
        assert_eq!(
            got, expected,
            "trashed cursor walk dropped/duplicated/reordered entries"
        );
    }

    #[tokio::test]
    async fn last_page_null_cursor_terminates_the_walk() {
        // A dataset that is an EXACT multiple of PAGE still terminates: the page
        // after the last full one is empty (len 0 != PAGE) → next_cursor null.
        // Covered implicitly above (450 = 200+200+50), but assert the single-page
        // case too: a short first page must not request a second page.
        let api = ApiClient::new_for_test(spawn_mock().await);
        // parent with no children in the mock (offset starts at 0, but a
        // sub-200 dataset): reuse trashed=false but check the terminator logic
        // by confirming exactly ACTIVE_TOTAL (terminated cleanly, no hang/loop).
        let got = ids(&api.list_files(None).await.unwrap());
        assert_eq!(got.len(), ACTIVE_TOTAL);
    }

    // The safety cap (#83 review, P2): a listing is truncated only when entries
    // were discarded or a cursor remains, never just for reaching the cap.
    use super::{FILE_LIST_HARD_CAP, listing_cap_reached};

    fn entries(n: usize) -> Vec<serde_json::Value> {
        vec![serde_json::Value::Null; n]
    }

    #[test]
    fn below_the_cap_keeps_walking() {
        let mut all = entries(FILE_LIST_HARD_CAP - 1);
        assert_eq!(listing_cap_reached(&mut all, FILE_LIST_HARD_CAP, true), None);
    }

    #[test]
    fn exactly_the_cap_with_no_cursor_is_complete() {
        let mut all = entries(FILE_LIST_HARD_CAP);
        assert_eq!(
            listing_cap_reached(&mut all, FILE_LIST_HARD_CAP, false),
            Some(false),
            "50,000 entries and no next_cursor is the whole folder"
        );
        assert_eq!(all.len(), FILE_LIST_HARD_CAP);
    }

    #[test]
    fn exactly_the_cap_with_a_cursor_is_truncated() {
        let mut all = entries(FILE_LIST_HARD_CAP);
        assert_eq!(
            listing_cap_reached(&mut all, FILE_LIST_HARD_CAP, true),
            Some(true),
            "a cursor remains: entries may be missing"
        );
        assert_eq!(all.len(), FILE_LIST_HARD_CAP);
    }

    #[test]
    fn past_the_cap_discards_entries_and_is_truncated() {
        let mut all = entries(FILE_LIST_HARD_CAP + 7);
        assert_eq!(listing_cap_reached(&mut all, FILE_LIST_HARD_CAP, false), Some(true));
        assert_eq!(all.len(), FILE_LIST_HARD_CAP, "the overshoot is cut off");
    }
}

#[cfg(test)]
mod client_header_tests {
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
}

#[cfg(test)]
mod twofa_action_tests {
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
}

#[cfg(test)]
mod session_revoke_request_tests {
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
}

#[cfg(test)]
mod passkey_delete_request_tests {
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
}

/// Task 0485 — regression coverage for a real bug found while wiring `bb
/// billing show` against the live server: `get_billing_usage()` used to
/// alias `get_usage()`, which hits `/api/v1/files/usage` (a DIFFERENT route
/// that returns `plan_limit_bytes`/`plan_name`, never `quota_bytes`). The two
/// mock routes below return DIFFERENT bodies so a regression to the old
/// aliasing shows up as a missing `quota_bytes` field, not a coincidental
/// pass.
#[cfg(test)]
mod billing_usage_route_tests {
    use axum::routing::get;
    use axum::{Json, Router};
    use serde_json::json;

    use super::ApiClient;

    async fn spawn_billing_usage_mock() -> String {
        let app = Router::new()
            .route(
                "/api/v1/billing/usage",
                get(|| async {
                    Json(json!({
                        "used_bytes": 42,
                        "quota_bytes": 1_000_000_000_000i64,
                        "percentage": 0.000042,
                    }))
                }),
            )
            .route(
                "/api/v1/files/usage",
                // A DIFFERENT shape (the real live /files/usage handler,
                // beebeeb-api/src/routes/files.rs::storage_usage) — no
                // `quota_bytes` key. If `get_billing_usage()` ever aliases
                // back to this route, the assertion below fails loudly
                // instead of silently reading a stale field name.
                get(|| async {
                    Json(json!({
                        "used_bytes": 42,
                        "plan_limit_bytes": 1_000_000_000_000i64,
                        "plan_name": "free",
                    }))
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
    async fn get_billing_usage_hits_the_billing_route_not_files_usage() {
        let api = ApiClient::new_for_test(spawn_billing_usage_mock().await);
        let resp = api.get_billing_usage().await.expect("mock request should succeed");

        assert_eq!(
            resp.get("quota_bytes").and_then(|v| v.as_i64()),
            Some(1_000_000_000_000),
            "get_billing_usage() must read /api/v1/billing/usage's quota_bytes field \
             (bb billing show reads this directly — see commands::billing::show), \
             not /api/v1/files/usage's plan_limit_bytes: {resp}"
        );
        assert!(
            resp.get("plan_limit_bytes").is_none(),
            "response carries plan_limit_bytes — get_billing_usage() hit /api/v1/files/usage \
             instead of /api/v1/billing/usage: {resp}"
        );
    }
}

/// Task 0487 — `get_billing_invoices()` / `download_invoice_pdf()` against a
/// mock of the live server's `/api/v1/billing/invoices[/…/pdf]` routes
/// (`beebeeb-api/src/routes/billing.rs::invoices` / `invoice_pdf`).
#[cfg(test)]
mod billing_invoices_route_tests {
    use axum::extract::Path;
    use axum::http::{StatusCode, header};
    use axum::routing::get;
    use axum::{Json, Router};
    use serde_json::json;

    use super::ApiClient;

    async fn spawn_billing_invoices_mock() -> String {
        let app = Router::new()
            .route(
                "/api/v1/billing/invoices",
                get(|| async {
                    // Exact live-server field shape (billing.rs::invoices) —
                    // `number` (not `invoice_number`), `amount_gross_cents`
                    // (not `amount_paid`), ISO date strings (not epoch
                    // seconds). The server sends no provider-configuration
                    // flag in the envelope.
                    Json(json!({
                        "invoices": [{
                            "id": "11111111-1111-1111-1111-111111111111",
                            "number": "BB-2026-0001",
                            "invoice_date": "2026-01-15",
                            "period_start": "2026-01-01",
                            "period_end": "2026-01-31",
                            "currency": "eur",
                            "amount_net_cents": 999,
                            "vat_rate_bps": 2100,
                            "vat_amount_cents": 210,
                            "amount_gross_cents": 1209,
                            "vat_treatment": "domestic",
                            "description": "Pro plan",
                            "status": "paid",
                            "pdf_url": "/api/v1/billing/invoices/11111111-1111-1111-1111-111111111111/pdf",
                        }],
                    }))
                }),
            )
            .route(
                "/api/v1/billing/invoices/:id/pdf",
                get(|Path(id): Path<String>| async move {
                    assert_eq!(id, "11111111-1111-1111-1111-111111111111");
                    (
                        StatusCode::OK,
                        [(header::CONTENT_TYPE, "application/pdf")],
                        b"%PDF-1.4 fake invoice pdf".to_vec(),
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
    async fn get_billing_invoices_reads_the_live_field_shape() {
        let api = ApiClient::new_for_test(spawn_billing_invoices_mock().await);
        let body = api.get_billing_invoices().await.expect("mock request should succeed");
        let invoices = body.get("invoices").and_then(|v| v.as_array()).expect("invoices array");
        assert_eq!(invoices.len(), 1);
        let inv = &invoices[0];
        assert_eq!(inv.get("number").and_then(|v| v.as_str()), Some("BB-2026-0001"));
        assert_eq!(inv.get("amount_gross_cents").and_then(|v| v.as_i64()), Some(1209));
        assert_eq!(inv.get("invoice_date").and_then(|v| v.as_str()), Some("2026-01-15"));
        let envelope = body.as_object().expect("invoices envelope object");
        assert_eq!(
            envelope.keys().collect::<Vec<_>>(),
            vec!["invoices"],
            "the envelope is {{ invoices: [...] }} — the client must parse a body with no \
             provider-configuration flag: {body}"
        );
    }

    #[tokio::test]
    async fn download_invoice_pdf_returns_the_raw_bytes() {
        let api = ApiClient::new_for_test(spawn_billing_invoices_mock().await);
        let bytes = api
            .download_invoice_pdf("11111111-1111-1111-1111-111111111111")
            .await
            .expect("mock pdf download should succeed");
        assert_eq!(bytes, b"%PDF-1.4 fake invoice pdf".to_vec());
    }

    #[tokio::test]
    async fn download_invoice_pdf_surfaces_a_404_for_an_unknown_id() {
        let app = Router::new().route(
            "/api/v1/billing/invoices/:id/pdf",
            get(|| async { StatusCode::NOT_FOUND }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let api = ApiClient::new_for_test(format!("http://{addr}"));
        let err = api
            .download_invoice_pdf("00000000-0000-0000-0000-000000000000")
            .await
            .expect_err("a 404 must surface as an Err, not silently return empty bytes");
        assert!(err.message.contains("404"), "error should mention the status: {err}");
    }
}

/// Task 1547 finding 3 — regression coverage for `parse_response` dropping the
/// server's human-readable `message` field on every typed API error and
/// showing only the raw machine `error` code. `beebeeb-api/src/error.rs`
/// documents an explicit two-field contract (`error`: stable code for the
/// client to match on, `message`: human text for display) for ~20 typed
/// errors (billing_no_payment_method, quota_exceeded, account_suspended,
/// email_unverified, …) — the CLI special-cased none of them and never read
/// `message` at all.
#[cfg(test)]
mod error_message_priority_tests {
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
}

#[cfg(test)]
mod upload_refusal_account_state_tests {
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
}

#[cfg(test)]
mod move_request_tests {
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
}

#[cfg(test)]
mod retry_policy_tests {
    use super::CHUNK_RETRY;

    /// The retry vector: the ladder the CLI has always slept, now read from
    /// core's policy. 200, 400, 800, 1600, 3200 ms for attempts 1..=5, and no
    /// wait at all before the first attempt.
    #[test]
    fn chunk_retry_ladder_matches_the_shipped_one() {
        let ladder: Vec<u64> = (0..=CHUNK_RETRY.max_attempts).map(|retry| CHUNK_RETRY.delay_ms(retry)).collect();
        assert_eq!(ladder, vec![0, 200, 400, 800, 1_600, 3_200]);
        assert_eq!(CHUNK_RETRY.max_attempts, 5);
    }
}

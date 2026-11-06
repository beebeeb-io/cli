//! The HTTP client for the Beebeeb API: the request and reply types, the typed
//! errors, and the submodules for files, shares, billing and the direct-upload
//! transport.

use bytes::Bytes;
use reqwest::Client;
use serde_json::Value;
use std::error::Error;
use std::sync::atomic::{AtomicI64, Ordering};

use uuid::Uuid;

use crate::config::load_config;

mod direct;
pub(crate) use direct::{ChunkApiError, DirectOffer, GrantReply, UploadCall, now_ms};

// Wire types generated from `contracts/openapi.json` (see
// `scripts/openapi-types.sh`). The document is the server's contract; the
// types are the shapes `src/` parses, and the fixture round-trip test in this
// module's tests is what keeps them honest. Not every type has a caller yet:
// the ones the client branches on are used, the rest are the contract it is
// written against.
#[allow(dead_code)]
pub(crate) mod generated;

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
    /// Task 2333: the pool the server placed this upload in, as far as the client may show it (its id and
    /// country, never a datacenter code or a city: `PoolLabel::from_answer`). `None` when the answer names none.
    pub pool: Option<crate::transfer_stats::PoolLabel>,
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
            pool: crate::transfer_stats::PoolLabel::from_answer(v),
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

/// Sentinel `message` an [`ApiError`] carries when the request was rate
/// limited; every retry loop compares against this exact value.
pub const RATE_LIMITED_MESSAGE: &str = "__rate_limited__";

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
pub use upload::InitTransport;
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
        // string (`e.message == RATE_LIMITED_MESSAGE`) — `.map_err(|e| e.message)`
        // above preserves it byte-for-byte, so none of those call sites
        // need to change.
        return Err(ApiError {
            code: None,
            message: RATE_LIMITED_MESSAGE.to_string(),
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
#[path = "api/tests/current_version_number_tests.rs"]
mod current_version_number_tests;

/// CL-P3-11 — every generated wire type accepts its fixture and round-trips,
/// and the one replaced struct produces the same view as the old parser.
#[cfg(test)]
#[path = "api/tests/openapi_types_roundtrip_tests.rs"]
mod openapi_types_roundtrip_tests;

#[cfg(test)]
#[path = "api/tests/api_error_tests.rs"]
mod api_error_tests;

#[cfg(test)]
#[path = "api/tests/list_pagination_tests.rs"]
mod list_pagination_tests;

#[cfg(test)]
#[path = "api/tests/client_header_tests.rs"]
mod client_header_tests;

#[cfg(test)]
#[path = "api/tests/twofa_action_tests.rs"]
mod twofa_action_tests;

#[cfg(test)]
#[path = "api/tests/session_revoke_request_tests.rs"]
mod session_revoke_request_tests;

#[cfg(test)]
#[path = "api/tests/passkey_delete_request_tests.rs"]
mod passkey_delete_request_tests;

/// Task 0485 — regression coverage for a real bug found while wiring `bb
/// billing show` against the live server: `get_billing_usage()` used to
/// alias `get_usage()`, which hits `/api/v1/files/usage` (a DIFFERENT route
/// that returns `plan_limit_bytes`/`plan_name`, never `quota_bytes`). The two
/// mock routes below return DIFFERENT bodies so a regression to the old
/// aliasing shows up as a missing `quota_bytes` field, not a coincidental
/// pass.
#[cfg(test)]
#[path = "api/tests/billing_usage_route_tests.rs"]
mod billing_usage_route_tests;

/// Task 0487 — `get_billing_invoices()` / `download_invoice_pdf()` against a
/// mock of the live server's `/api/v1/billing/invoices[/…/pdf]` routes
/// (`beebeeb-api/src/routes/billing.rs::invoices` / `invoice_pdf`).
#[cfg(test)]
#[path = "api/tests/billing_invoices_route_tests.rs"]
mod billing_invoices_route_tests;

/// Task 1547 finding 3 — regression coverage for `parse_response` dropping the
/// server's human-readable `message` field on every typed API error and
/// showing only the raw machine `error` code. `beebeeb-api/src/error.rs`
/// documents an explicit two-field contract (`error`: stable code for the
/// client to match on, `message`: human text for display) for ~20 typed
/// errors (billing_no_payment_method, quota_exceeded, account_suspended,
/// email_unverified, …) — the CLI special-cased none of them and never read
/// `message` at all.
#[cfg(test)]
#[path = "api/tests/error_message_priority_tests.rs"]
mod error_message_priority_tests;

/// Task 2334, delta review P2-1: a 404 on the sent-invite list is "no route" for a push's share check, but a
/// read that did not answer for key resolution (only 405/501 say the route is absent there).
#[cfg(test)]
#[path = "api/tests/sent_invites_absent_route_tests.rs"]
mod sent_invites_absent_route_tests;

#[cfg(test)]
#[path = "api/tests/upload_refusal_account_state_tests.rs"]
mod upload_refusal_account_state_tests;

#[cfg(test)]
#[path = "api/tests/move_request_tests.rs"]
mod move_request_tests;

#[cfg(test)]
#[path = "api/tests/retry_policy_tests.rs"]
mod retry_policy_tests;

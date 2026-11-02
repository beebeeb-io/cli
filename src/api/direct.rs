//! The API calls of direct S3 transfers v1 (task 2173): discovery, download
//! grants and the pinned per-chunk fallback route, plus the HTTP client that
//! fetches presigned storage links.
//!
//! The DECISIONS (what an answer means, which links may be followed, when to
//! renew or fall back) live in `beebeeb_core::direct_transfer`, shared with
//! every other client; this file only makes the requests and reports what it
//! saw. Nothing here prints a URL: errors from the HTTP stack are run through
//! `dt::scrub` before they leave this module.
//!
//! Owns: the module's public surface, re-exported from the children below.
//! Must not: hold logic — `http` builds storage clients, `retry` owns the
//! pinned-route backoff, `state` the cached verdicts, `client` the requests.

use std::time::Duration;

use beebeeb_core::direct_transfer as dt;
use reqwest::Client;

use super::ApiClient;
use super::{NOT_LOGGED_IN_MESSAGE, SESSION_EXPIRED_MESSAGE, pace_if_needed, update_rate_state};

/// How long a lookup refused for a login (a 401 or 403 on the capabilities route) is answered from memory instead of
/// asking again. Long enough that a revoked session costs one request, not one per file; short enough that a
/// transient refusal (a front proxy, a node out of step) does not park direct transfers for the trust period. Another
/// login is never held by it.
pub(crate) const REFUSAL_HOLD: Duration = Duration::from_secs(60);

// ───────────────────────────── direct uploads (task 2173) ─────────────────────────────

mod http;
pub(crate) use http::now_ms;
use http::*;

mod retry;
pub(crate) use retry::ChunkApiError;
use retry::*;

mod state;
use state::*;
pub(crate) use state::{DirectOffer, DirectState, GrantReply, Refusal};

mod client;
pub(crate) use client::UploadCall;

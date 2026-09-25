//! Task 1552 — CLI mirror of web PR #79
//! (`repos/web/src/lib/signup-email-code.ts`) and mobile PR #110
//! (`repos/mobile/src/lib/signup-email-code.ts`): verify the email with a
//! code BEFORE the account is created (server task 1525, no more phantom
//! signups for an email that already has an account).
//!
//! Pure decision/formatting functions only — no network, no stdin — so
//! they're plain `cargo test`-able. `commands::signup` does the I/O and
//! calls into these.
//!
//! Server contract (task 1525, final head `3c5f706`):
//! `POST /auth/signup/email-start {email}` → ALWAYS `202`, byte-identical
//! body for a new vs. an existing email (anti-enumeration). New email → an
//! 8-digit code email; existing email → a "you already have an account"
//! notice, no code — the caller has no way to tell the two apart from the
//! response alone. `POST /auth/signup/email-verify {email, code}` →
//! `200 {signup_ticket}` | `400` (wrong/expired/reused/attempt-cap-exhausted,
//! deliberately undifferentiated — no signal about which). Resend: each
//! `/email-start` while a code is already live ADDS a fresh one, up to
//! [`MAX_LIVE_CODES_PER_EMAIL`] simultaneously live; past the cap it is
//! still a 202 with no new mail. OPAQUE register-start/finish both carry an
//! optional `signup_ticket` body field; register-finish CONSUMES it
//! atomically with the account INSERT, so a missing/wrong-email/expired/
//! consumed ticket renders as `403 {"error": "signup_ticket_invalid"}`.
//!
//! **`is_ticket_invalid_error` note** (carried over from mobile PR #110,
//! verified against a live server in this task's own lane — see the task
//! file's Notes for the transcript): the 403 body carries BOTH the stable
//! `error` code AND a human-readable `message`
//! ("Verify your email again to get a new signup link."). Detection MUST
//! key off HTTP status 403 + the stable `error` code (`ApiError::is_code`),
//! never the message text — a message-substring check is fragile to copy
//! changes and, per mobile's note, some client HTTP helpers don't even
//! surface the code once a `message` is present. `ApiError::is_code` here
//! does surface it (this crate's `parse_response_typed` reads `error`
//! straight off the JSON body), so this is a belt-and-suspenders choice
//! documented for cross-client consistency, not a workaround for a local
//! bug.

use crate::api::ApiError;

/// The server's code length
/// (`beebeeb-api::routes::auth::generate_verification_code` — widened
/// 6→8 digits, task 1525's server-side "Deviation flagged" note).
pub const EMAIL_CODE_LENGTH: usize = 8;

/// Mirrors the server's `MAX_LIVE_CODES_PER_EMAIL`
/// (`signup_email_challenge.rs`, round 3) — at most this many codes may be
/// simultaneously live for one email. A resend past the cap still 202s but
/// sends nothing new — the oldest live code keeps working.
pub const MAX_LIVE_CODES_PER_EMAIL: u32 = 3;

/// Strip everything but digits and clip to `max_len`. Used on every raw
/// terminal line the user types for the code — a pasted code often carries
/// surrounding whitespace or stray text copied along with it from the email
/// ("Your code: 12345678"), so this is deliberately permissive about the
/// input shape as long as the digits themselves are intact and in order.
pub fn sanitize_code(raw: &str, max_len: usize) -> String {
    raw.chars().filter(|c| c.is_ascii_digit()).take(max_len).collect()
}

/// Whether `err` means "this server predates task 1525 and has no
/// `/signup/email-start` route at all" — the capability-detection signal
/// `commands::signup` uses to skip the code step entirely and fall back to
/// the pre-1552 flow (OPAQUE registration with no ticket) rather than
/// showing a code prompt a stale server can never satisfy. Every OTHER
/// error (400 bad email, 429 rate limited, network failure, 5xx) means the
/// route DOES exist — the failure is surfaced on-screen instead (mirrors
/// web's `isLegacyFallbackError` / mobile's function of the same name).
pub fn is_legacy_fallback_error(err: &ApiError) -> bool {
    err.status == 404
}

/// Whether `err` is the server's
/// `403 {"error":"signup_ticket_invalid", "message": "..."}` — a
/// previously-valid ticket was rejected on register-start/finish (expired
/// mid-flow, wrong email, already consumed). See this module's doc comment
/// for why this keys on the stable `error` CODE, never the `message` text.
pub fn is_ticket_invalid_error(err: &ApiError) -> bool {
    err.status == 403 && err.is_code("signup_ticket_invalid")
}

/// Best-effort copy for the Nth resend request (1-indexed — the initial
/// `/email-start` call `bb signup` itself makes is NOT resend #1) since the
/// code prompt started. Mirrors web's `resendCopyForAttempt`: the server's
/// round-3 contract gives the client NO signal to distinguish "a fresh code
/// was actually mailed" from "no-op'd at the live-code cap" (both are a
/// plain 202) — so past `max_live_codes - 1` resends this stops claiming a
/// fresh send rather than repeating a possibly-false "code resent" message.
/// Returns `(message, likely_sent)`.
pub fn resend_copy_for_attempt(resend_attempt: u32, max_live_codes: u32) -> (String, bool) {
    if resend_attempt <= max_live_codes.saturating_sub(1) {
        (
            "Sent a new code — any code from the last 15 minutes works.".to_string(),
            true,
        )
    } else {
        (
            "Already requested the maximum number of codes for now — any code from the last 15 minutes still works."
                .to_string(),
            false,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ApiError`'s fields are `pub(crate)`, so this same-crate test module
    /// can build fixtures directly with an arbitrary `status` — the
    /// existing `test_code`/`test_message` helpers on `ApiError` always set
    /// `status: 0`, which can't exercise `is_legacy_fallback_error`
    /// (needs 404) or `is_ticket_invalid_error` (needs 403) at all.
    fn err(status: u16, code: Option<&str>) -> ApiError {
        ApiError {
            code: code.map(|c| c.to_string()),
            message: code.unwrap_or("").to_string(),
            status,
        }
    }

    #[test]
    fn sanitize_code_strips_non_digits_and_clips() {
        assert_eq!(sanitize_code("12345678", 8), "12345678");
        assert_eq!(sanitize_code("Code: 12-34 56.78!", 8), "12345678");
        assert_eq!(sanitize_code("123456789999", 8), "12345678");
        assert_eq!(sanitize_code("", 8), "");
        assert_eq!(sanitize_code("  1 2  ", 8), "12");
        assert_eq!(sanitize_code("abc", 8), "");
    }

    #[test]
    fn legacy_fallback_is_404_only() {
        assert!(is_legacy_fallback_error(&err(404, None)));
        assert!(!is_legacy_fallback_error(&err(400, Some("bad_request"))));
        assert!(!is_legacy_fallback_error(&err(403, Some("signup_ticket_invalid"))));
        assert!(!is_legacy_fallback_error(&err(0, None)));
        assert!(!is_legacy_fallback_error(&err(200, None)));
    }

    #[test]
    fn ticket_invalid_requires_403_and_the_exact_code_not_the_message() {
        assert!(is_ticket_invalid_error(&err(403, Some("signup_ticket_invalid"))));
        // No code at all (e.g. a network-transport ApiError) must not match.
        assert!(!is_ticket_invalid_error(&err(403, None)));
        // Right code, wrong status — must not match (the code alone isn't proof).
        assert!(!is_ticket_invalid_error(&err(404, Some("signup_ticket_invalid"))));
        // Right status, a DIFFERENT real 403 code on the same routes
        // (pilot_key_required) — must not match.
        assert!(!is_ticket_invalid_error(&err(403, Some("pilot_key_required"))));
    }

    #[test]
    fn resend_copy_claims_sent_until_the_live_code_cap_then_stops_claiming() {
        let (msg1, sent1) = resend_copy_for_attempt(1, MAX_LIVE_CODES_PER_EMAIL);
        let (_msg2, sent2) = resend_copy_for_attempt(2, MAX_LIVE_CODES_PER_EMAIL);
        let (msg3, sent3) = resend_copy_for_attempt(3, MAX_LIVE_CODES_PER_EMAIL);
        assert!(sent1, "first resend (2nd overall live code) must claim a real send");
        assert!(
            sent2,
            "second resend (3rd overall live code, at the cap) must still claim a real send"
        );
        assert!(
            !sent3,
            "third resend would be a 4th live code — past MAX_LIVE_CODES_PER_EMAIL, must not claim a fresh send"
        );
        assert!(msg1.contains("Sent a new code"));
        assert!(msg3.contains("maximum number"));
        assert_ne!(msg1, msg3);
    }
}

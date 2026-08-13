//! Unicode-block ASCII QR rendering for the `bb 2fa setup` flow.
//!
//! Uses `qrcode::render::unicode::Dense1x2` so each character row carries
//! two QR modules vertically. An `otpauth://` URI with a base32 secret
//! encodes to a 25x25 module Version 2 QR; at Dense1x2 that's 13 lines of
//! 25 characters — well within 80 columns.
//!
//! Error correction is medium (`EcLevel::M`) — the default for the crate.

use qrcode::QrCode;
use qrcode::render::unicode;

/// Render an `otpauth://` URI as a unicode-block QR code suitable for terminal
/// display. Returns an empty string if QR generation fails (extremely unlikely
/// — `otpauth://` URIs always fit in a Version 2 QR).
///
/// NOT currently called from the `bb 2fa setup` command flow despite the
/// module doc above — `commands/account.rs` prints the `otpauth://` URI as
/// text only. Tested and working; just not wired in. Flagged 2026-08-13
/// during a CI cleanup pass rather than silently wiring it in (a real UX
/// decision, not a lint fix).
#[allow(dead_code)]
pub fn render_otpauth(uri: &str) -> String {
    match QrCode::new(uri.as_bytes()) {
        Ok(code) => code
            .render::<unicode::Dense1x2>()
            .quiet_zone(true)
            .module_dimensions(1, 1)
            .build(),
        Err(_) => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_a_typical_totp_uri() {
        let uri = "otpauth://totp/Beebeeb:guus@devidee.nl\
                   ?secret=KJSXG43LEBQXG4RAINVHK4TPNZ2HG2LDOJSWG\
                   &issuer=Beebeeb&algorithm=SHA1&digits=6&period=30";
        let qr = render_otpauth(uri);
        assert!(!qr.is_empty(), "render returned empty string");
        // Width sanity: every line ≤ 80 columns.
        for line in qr.lines() {
            // unicode block chars are 1 column wide in monospace terminals
            assert!(
                line.chars().count() <= 80,
                "QR line is {} cols, should be <= 80: {line}",
                line.chars().count()
            );
        }
        // Height sanity: at least 10 lines (Version 2 QR is 25 modules = ≥12 lines @ Dense1x2).
        assert!(qr.lines().count() >= 10, "QR has fewer lines than expected");
    }

    #[test]
    fn renders_empty_for_garbage_that_cannot_encode() {
        // qrcode can encode arbitrary bytes; the only failure mode is when the
        // payload is too large for the maximum QR version (~2,953 bytes for
        // alphanumeric). Confirm graceful return.
        let huge = "x".repeat(10_000);
        let qr = render_otpauth(&huge);
        assert_eq!(qr, "", "expected empty string for oversized input");
    }
}

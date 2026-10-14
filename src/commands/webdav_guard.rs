//! Request guard for `bb webdav` (task 1790, finding M31).
//!
//! The gateway serves the DECRYPTED vault on a loopback port. A loopback bind alone does
//! not protect it: a web page can reach it through DNS rebinding, any other account on the
//! machine can connect, and a plain `fetch()` can send PUT/DELETE. Every request therefore
//! passes four checks, in this order, before any handler runs:
//!
//! 1. `Host` must be exactly this gateway (`127.0.0.1:<port>` or `localhost:<port>`).
//!    A rebinding page carries its own hostname, so it is refused.
//! 2. No `Origin` header at all. Browsers attach one to cross-origin writes; WebDAV clients
//!    (Finder, rclone, Cyberduck, davfs2) never do.
//! 3. HTTP Basic with the per-run random password printed at startup (any user name).
//!    This is what closes the "another local account" hole, which Host checks cannot.
//! 4. Writes (PUT, DELETE, MKCOL, MOVE, COPY, PROPPATCH) are refused unless `--read-write`.
//!
//! The password is never logged; only its SHA-256 digest is kept after startup.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use rand::RngCore;
use sha2::{Digest, Sha256};

/// Methods that change the vault (or would, on a server that implemented them).
pub fn is_write_method(method: &Method) -> bool {
    matches!(
        method.as_str(),
        "PUT" | "DELETE" | "MKCOL" | "MOVE" | "COPY" | "PROPPATCH"
    )
}

/// 32 random bytes from the OS CSPRNG (256 bits), base64url without padding (43 chars).
/// URL-safe so it can sit in `http://bb:<secret>@localhost:<port>/` unescaped.
pub fn generate_secret() -> String {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

#[derive(Debug, PartialEq, Eq)]
pub enum Rejection {
    ForeignHost,
    ForeignOrigin,
    Unauthorized,
    ReadOnly,
}

impl Rejection {
    fn label(&self) -> &'static str {
        match self {
            Rejection::ForeignHost => "foreign Host header",
            Rejection::ForeignOrigin => "Origin header present",
            Rejection::Unauthorized => "missing or wrong credential",
            Rejection::ReadOnly => "write refused (read-only)",
        }
    }

    fn into_response(self) -> Response {
        match self {
            Rejection::ForeignHost => (StatusCode::FORBIDDEN, "bb webdav: unexpected Host header\n").into_response(),
            Rejection::ForeignOrigin => (
                StatusCode::FORBIDDEN,
                "bb webdav: requests from web pages are not accepted\n",
            )
                .into_response(),
            Rejection::Unauthorized => Response::builder()
                .status(StatusCode::UNAUTHORIZED)
                .header(
                    header::WWW_AUTHENTICATE,
                    "Basic realm=\"Beebeeb vault\", charset=\"UTF-8\"",
                )
                .body(Body::from("bb webdav: credential required\n"))
                .expect("static 401 response"),
            Rejection::ReadOnly => (
                StatusCode::FORBIDDEN,
                "bb webdav is read-only; restart it with --read-write to allow changes\n",
            )
                .into_response(),
        }
    }
}

pub struct Guard {
    port: u16,
    secret_digest: [u8; 32],
    read_only: bool,
    verbose: bool,
}

impl Guard {
    pub fn new(port: u16, secret: &str, read_only: bool, verbose: bool) -> Self {
        Self {
            port,
            secret_digest: Sha256::digest(secret.as_bytes()).into(),
            read_only,
            verbose,
        }
    }

    /// Is `authority` (a `Host` header value or a request-URI authority) this gateway?
    fn host_allowed(&self, authority: &str) -> bool {
        let a = authority.to_ascii_lowercase();
        let port = self.port;
        if a == format!("127.0.0.1:{port}") || a == format!("localhost:{port}") {
            return true;
        }
        // Clients omit the port when it is the scheme default.
        port == 80 && (a == "127.0.0.1" || a == "localhost")
    }

    fn credential_ok(&self, headers: &HeaderMap) -> bool {
        let mut values = headers.get_all(header::AUTHORIZATION).iter();
        let (Some(value), None) = (values.next(), values.next()) else {
            return false;
        };
        let Ok(value) = value.to_str() else { return false };
        let Some((scheme, encoded)) = value.split_once(' ') else {
            return false;
        };
        if !scheme.eq_ignore_ascii_case("basic") {
            return false;
        }
        let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(encoded.trim()) else {
            return false;
        };
        let Some(colon) = decoded.iter().position(|b| *b == b':') else {
            return false;
        };
        let given: [u8; 32] = Sha256::digest(&decoded[colon + 1..]).into();
        constant_time_eq(&given, &self.secret_digest)
    }

    /// The four checks, in order. `uri_authority` is the authority of an absolute-form
    /// request target, if the client sent one.
    pub fn check(&self, method: &Method, headers: &HeaderMap, uri_authority: Option<&str>) -> Result<(), Rejection> {
        let mut hosts = headers.get_all(header::HOST).iter();
        let host = hosts.next();
        if hosts.next().is_some() {
            return Err(Rejection::ForeignHost);
        }
        match host {
            Some(h) => {
                if !h.to_str().is_ok_and(|h| self.host_allowed(h)) {
                    return Err(Rejection::ForeignHost);
                }
            }
            // HTTP/2 carries the authority in the request URI instead of a Host header.
            None if uri_authority.is_some_and(|a| self.host_allowed(a)) => {}
            None => return Err(Rejection::ForeignHost),
        }
        if let Some(a) = uri_authority
            && !self.host_allowed(a)
        {
            return Err(Rejection::ForeignHost);
        }
        if headers.contains_key(header::ORIGIN) {
            return Err(Rejection::ForeignOrigin);
        }
        if !self.credential_ok(headers) {
            return Err(Rejection::Unauthorized);
        }
        if self.read_only && is_write_method(method) {
            return Err(Rejection::ReadOnly);
        }
        Ok(())
    }
}

/// Compare two fixed-length digests without an early exit on the first differing byte.
/// Comparing digests (not the raw secret) also hides the secret's length.
fn constant_time_eq(a: &[u8; 32], b: &[u8; 32]) -> bool {
    a.iter().zip(b.iter()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// axum middleware: refuse the request before any handler (or body read) if it fails
/// [`Guard::check`]. Logs only method, path and reason in `--verbose`; never a header.
pub async fn enforce(State(guard): State<Arc<Guard>>, req: Request, next: Next) -> Response {
    let authority = req.uri().authority().map(|a| a.as_str().to_string());
    match guard.check(req.method(), req.headers(), authority.as_deref()) {
        Ok(()) => next.run(req).await,
        Err(rejection) => {
            if guard.verbose {
                eprintln!(
                    "  WebDAV refused {} {}: {}",
                    req.method(),
                    req.uri().path(),
                    rejection.label()
                );
            }
            rejection.into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    const SECRET: &str = "s3cret-value";

    fn guard(read_only: bool) -> Guard {
        Guard::new(7878, SECRET, read_only, false)
    }

    fn basic(user: &str, pass: &str) -> HeaderValue {
        let b = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pass}"));
        HeaderValue::from_str(&format!("Basic {b}")).unwrap()
    }

    fn headers(host: &str, auth: Option<HeaderValue>, origin: Option<&str>) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(header::HOST, HeaderValue::from_str(host).unwrap());
        if let Some(a) = auth {
            h.insert(header::AUTHORIZATION, a);
        }
        if let Some(o) = origin {
            h.insert(header::ORIGIN, HeaderValue::from_str(o).unwrap());
        }
        h
    }

    #[test]
    fn good_request_passes() {
        let h = headers("localhost:7878", Some(basic("bb", SECRET)), None);
        assert_eq!(guard(true).check(&Method::GET, &h, None), Ok(()));
        // any user name
        let h = headers("127.0.0.1:7878", Some(basic("whoever", SECRET)), None);
        assert_eq!(guard(true).check(&Method::GET, &h, None), Ok(()));
    }

    #[test]
    fn host_must_be_this_gateway() {
        let g = guard(false);
        for host in [
            "evil.example",
            "evil.example:7878",
            "localhost",
            "localhost:7879",
            "127.0.0.1",
            "127.0.0.1.evil.example:7878",
            "localhost.:7878",
            "[::1]:7878",
            "0.0.0.0:7878",
        ] {
            let h = headers(host, Some(basic("bb", SECRET)), None);
            assert_eq!(g.check(&Method::GET, &h, None), Err(Rejection::ForeignHost), "{host}");
        }
    }

    #[test]
    fn missing_or_duplicate_host_is_refused() {
        let g = guard(false);
        let mut h = HeaderMap::new();
        h.insert(header::AUTHORIZATION, basic("bb", SECRET));
        assert_eq!(g.check(&Method::GET, &h, None), Err(Rejection::ForeignHost));
        // ...unless the request URI carries the right authority (HTTP/2).
        assert_eq!(g.check(&Method::GET, &h, Some("localhost:7878")), Ok(()));
        let mut h = headers("localhost:7878", Some(basic("bb", SECRET)), None);
        h.append(header::HOST, HeaderValue::from_static("evil.example"));
        assert_eq!(g.check(&Method::GET, &h, None), Err(Rejection::ForeignHost));
        // A right Host header with a foreign absolute-form target is refused.
        let h = headers("localhost:7878", Some(basic("bb", SECRET)), None);
        assert_eq!(
            g.check(&Method::GET, &h, Some("evil.example")),
            Err(Rejection::ForeignHost)
        );
    }

    #[test]
    fn host_without_port_is_allowed_only_on_port_80() {
        let g = Guard::new(80, SECRET, false, false);
        let h = headers("localhost", Some(basic("bb", SECRET)), None);
        assert_eq!(g.check(&Method::GET, &h, None), Ok(()));
    }

    #[test]
    fn any_origin_is_refused_even_with_a_valid_credential() {
        for origin in ["http://evil.example", "null", "http://localhost:7878"] {
            let h = headers("localhost:7878", Some(basic("bb", SECRET)), Some(origin));
            assert_eq!(
                guard(false).check(&Method::PUT, &h, None),
                Err(Rejection::ForeignOrigin),
                "{origin}"
            );
        }
    }

    #[test]
    fn credential_must_match() {
        let g = guard(false);
        let none = headers("localhost:7878", None, None);
        assert_eq!(g.check(&Method::GET, &none, None), Err(Rejection::Unauthorized));
        for bad in [
            basic("bb", "wrong"),
            basic("bb", ""),
            basic("bb", &format!("{SECRET}x")),
            basic("bb", &SECRET[..SECRET.len() - 1]),
            HeaderValue::from_static("Bearer s3cret-value"),
            HeaderValue::from_static("Basic !!!not-base64!!!"),
            HeaderValue::from_static("Basic"),
        ] {
            let h = headers("localhost:7878", Some(bad.clone()), None);
            assert_eq!(g.check(&Method::GET, &h, None), Err(Rejection::Unauthorized), "{bad:?}");
        }
        // The secret as the user name does not count; a colon in the password does.
        let h = headers("localhost:7878", Some(basic(SECRET, "")), None);
        assert_eq!(g.check(&Method::GET, &h, None), Err(Rejection::Unauthorized));
        let g2 = Guard::new(7878, "a:b", false, false);
        let h = headers("localhost:7878", Some(basic("bb", "a:b")), None);
        assert_eq!(g2.check(&Method::GET, &h, None), Ok(()));
        // Scheme is case-insensitive.
        let b = base64::engine::general_purpose::STANDARD.encode(format!("bb:{SECRET}"));
        let h = headers(
            "localhost:7878",
            Some(HeaderValue::from_str(&format!("basic {b}")).unwrap()),
            None,
        );
        assert_eq!(g.check(&Method::GET, &h, None), Ok(()));
    }

    #[test]
    fn only_one_authorization_header_is_accepted() {
        let mut h = headers("localhost:7878", Some(basic("bb", SECRET)), None);
        h.append(header::AUTHORIZATION, basic("bb", "other"));
        assert_eq!(guard(false).check(&Method::GET, &h, None), Err(Rejection::Unauthorized));
    }

    #[test]
    fn writes_are_refused_when_read_only() {
        let h = headers("localhost:7878", Some(basic("bb", SECRET)), None);
        for m in ["PUT", "DELETE", "MKCOL", "MOVE", "COPY", "PROPPATCH"] {
            let m = Method::from_bytes(m.as_bytes()).unwrap();
            assert_eq!(guard(true).check(&m, &h, None), Err(Rejection::ReadOnly), "{m}");
            assert_eq!(guard(false).check(&m, &h, None), Ok(()), "{m}");
        }
        for m in ["GET", "HEAD", "OPTIONS", "PROPFIND", "LOCK", "UNLOCK"] {
            let m = Method::from_bytes(m.as_bytes()).unwrap();
            assert_eq!(guard(true).check(&m, &h, None), Ok(()), "{m}");
        }
    }

    #[test]
    fn order_is_host_then_origin_then_credential_then_read_only() {
        let g = guard(true);
        let h = headers("evil.example", None, Some("http://evil.example"));
        assert_eq!(g.check(&Method::PUT, &h, None), Err(Rejection::ForeignHost));
        let h = headers("localhost:7878", None, Some("http://evil.example"));
        assert_eq!(g.check(&Method::PUT, &h, None), Err(Rejection::ForeignOrigin));
        let h = headers("localhost:7878", None, None);
        assert_eq!(g.check(&Method::PUT, &h, None), Err(Rejection::Unauthorized));
    }

    #[test]
    fn secrets_are_long_random_and_url_safe() {
        let a = generate_secret();
        let b = generate_secret();
        assert_ne!(a, b);
        assert_eq!(a.len(), 43, "32 bytes of base64url without padding");
        assert!(a.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'));
    }

    #[test]
    fn constant_time_eq_compares_every_byte() {
        let a = [1u8; 32];
        let mut b = a;
        assert!(constant_time_eq(&a, &b));
        for i in 0..32 {
            b[i] ^= 0x80;
            assert!(!constant_time_eq(&a, &b), "byte {i}");
            b[i] ^= 0x80;
        }
    }
}

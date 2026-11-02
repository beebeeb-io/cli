//! `lock` items split out of `webdav.rs`.

use super::*;

/// An active WebDAV lock.  Stubs only — no distributed coordination.
/// Exists solely to satisfy Finder/LibreOffice lock expectations.
#[derive(Clone)]
pub(super) struct LockEntry {
    /// `urn:uuid:<UUID>` token that the client echoes back on writes.
    pub(super) token: String,
    /// Lock owner string (from the request XML, for display only).
    #[allow(dead_code)]
    pub(super) owner: String,
    /// Whether the lock is exclusive (true) or shared (false).
    pub(super) exclusive: bool,
    /// When the lock expires (Instant).
    pub(super) expires_at: Instant,
}

/// Lock store: path → active lock.
pub(super) type LockStore = Mutex<HashMap<String, LockEntry>>;

/// Parse the lock scope from the LOCK request XML body.
/// Returns `(exclusive, owner_string)` — tolerant of malformed bodies.
pub(super) fn parse_lock_body(body: &[u8]) -> (bool, String) {
    let text = std::str::from_utf8(body).unwrap_or("");
    // Extract exclusivity from the XML (simple string search — no full XML parse)
    let exclusive = !text.contains("shared");
    // Try to extract owner from <D:owner><D:href>...</D:href></D:owner>
    let owner = if let Some(start) = text.find("<D:href>") {
        let after = &text[start + 8..];
        if let Some(end) = after.find("</D:href>") {
            after[..end].trim().to_string()
        } else {
            "unknown".to_string()
        }
    } else {
        "client".to_string()
    };
    (exclusive, owner)
}

/// Parse `Timeout: Second-N` or `Timeout: Infinite` header.
pub(super) fn parse_timeout_header(headers: &HeaderMap) -> u64 {
    headers
        .get("timeout")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Second-").and_then(|rest| rest.parse::<u64>().ok()))
        .unwrap_or(300) // 5 minutes default
}

/// Parse `If: (<urn:uuid:TOKEN>)` header and return the raw token string.
pub(super) fn parse_if_token(if_header: &str) -> Option<String> {
    // Format: (<urn:uuid:...>) or (<urn:uuid:...> [etag])
    let inner = if_header.trim().trim_start_matches('(').trim_end_matches(')');
    if inner.is_empty() {
        return None;
    }
    // Take the first bracketed token
    let token = inner.split_whitespace().next()?;
    Some(token.trim_matches(|c: char| c == '<' || c == '>').to_string())
}

/// LOCK /path — issue a synthetic lock token.
pub(super) async fn lock_response(state: &Arc<DavState>, path: &str, body: &[u8], timeout_secs: u64) -> Response {
    let (exclusive, owner) = parse_lock_body(body);
    let token_uuid = Uuid::new_v4();
    let token = format!("urn:uuid:{token_uuid}");
    let scope_label = if exclusive { "exclusive" } else { "shared" };

    // Clean up expired locks lazily
    {
        let mut locks = state.locks.lock().await;
        locks.retain(|_, v| v.expires_at > Instant::now());
        // Check for conflict: existing exclusive lock on same path
        if let Some(existing) = locks.get(path) {
            if existing.expires_at > Instant::now() && existing.exclusive {
                return (StatusCode::LOCKED, "resource is already exclusively locked").into_response();
            }
        }
        locks.insert(
            path.to_string(),
            LockEntry {
                token: token.clone(),
                owner: owner.clone(),
                exclusive,
                expires_at: Instant::now() + Duration::from_secs(timeout_secs),
            },
        );
    }

    let href = xml_escape(path);
    let owner_escaped = xml_escape(&owner);
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <D:prop xmlns:D=\"DAV:\">\n\
           <D:lockdiscovery>\n\
             <D:activelock>\n\
               <D:locktype><D:write/></D:locktype>\n\
               <D:lockscope><D:{scope_label}/></D:lockscope>\n\
               <D:depth>0</D:depth>\n\
               <D:owner><D:href>{owner_escaped}</D:href></D:owner>\n\
               <D:timeout>Second-{timeout_secs}</D:timeout>\n\
               <D:locktoken><D:href>{token}</D:href></D:locktoken>\n\
               <D:lockroot><D:href>{href}</D:href></D:lockroot>\n\
             </D:activelock>\n\
           </D:lockdiscovery>\n\
         </D:prop>"
    );

    let lock_token_header = format!("<{token}>");
    let mut headers = HeaderMap::new();
    headers.insert("Content-Type", "application/xml; charset=utf-8".parse().unwrap());
    headers.insert(
        "Lock-Token",
        lock_token_header.parse().unwrap_or_else(|_| "".parse().unwrap()),
    );
    (StatusCode::OK, headers, body).into_response()
}

/// UNLOCK /path — remove the lock identified by the Lock-Token header.
pub(super) async fn unlock_response(state: &Arc<DavState>, path: &str, lock_token_header: &str) -> Response {
    // Extract the URN from the angle-bracket wrapper: <urn:uuid:...>
    let token = lock_token_header.trim().trim_matches(|c| c == '<' || c == '>');

    let mut locks = state.locks.lock().await;
    if let Some(entry) = locks.get(path) {
        if entry.token == token {
            locks.remove(path);
            return (StatusCode::NO_CONTENT, "").into_response();
        }
        // Token mismatch — still return 204 (permissive stub)
    }
    // Lock not found — return 204 anyway (idempotent)
    (StatusCode::NO_CONTENT, "").into_response()
}

/// Verify that a locked resource's token matches what the client provided.
///
/// If the resource is not locked, the request proceeds normally (no lock needed).
/// If the resource IS locked and the client provides no/wrong token → 423 Locked.
/// If the resource IS locked and the client provides the correct token → OK.
///
/// This is permissive: if the lock has expired, we treat it as unlocked.
pub(super) async fn check_lock(state: &Arc<DavState>, path: &str, client_token: Option<&str>) -> Result<(), Response> {
    let mut locks = state.locks.lock().await;
    let entry = match locks.get(path) {
        Some(e) => e.clone(),
        None => return Ok(()), // not locked
    };

    if entry.expires_at <= Instant::now() {
        locks.remove(path); // expired
        return Ok(());
    }

    match client_token {
        Some(tok) if tok == entry.token => Ok(()),
        Some(_) => Err((StatusCode::LOCKED, "lock token mismatch").into_response()),
        None => Err((StatusCode::LOCKED, "resource is locked — provide If: (<token>) header").into_response()),
    }
}

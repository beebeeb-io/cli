//! `server` items split out of `webdav.rs`.

use super::*;

pub async fn run(port: u16, read_write: bool, cache_ttl: u64, no_cache: bool, verbose: bool) -> Result<(), String> {
    let read_only = !read_write;
    let config = load_config();

    if config.session_token.is_none() {
        return Err("Not logged in. Run `bb login` first.".to_string());
    }

    let mk_b64 = config.master_key.ok_or("No master key found. Run `bb login` first.")?;
    let mk_bytes: Zeroizing<Vec<u8>> = Zeroizing::new(
        base64::engine::general_purpose::STANDARD
            .decode(&mk_b64)
            .map_err(|e| format!("invalid master key in config: {e}"))?,
    );
    if mk_bytes.len() != 32 {
        return Err(format!("master key must be 32 bytes, got {}", mk_bytes.len()));
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&mk_bytes);
    let master_key = beebeeb_core::kdf::MasterKey::from_bytes(arr);

    let effective_ttl = if no_cache || cache_ttl == 0 {
        Duration::ZERO
    } else {
        Duration::from_secs(cache_ttl)
    };

    let state = Arc::new(DavState {
        api: ApiClient::from_config(),
        master_key,
        read_only,
        dir_cache: Mutex::new(HashMap::new()),
        cache_ttl: effective_ttl,
        locks: Mutex::new(HashMap::new()),
        request_count: std::sync::atomic::AtomicU64::new(0),
        error_count: std::sync::atomic::AtomicU64::new(0),
        verbose,
    });

    // Bind first (once; a taken port fails here, clearly), then derive the guard from the
    // port actually bound so `--port 0` works too.
    let listener = bind_loopback(port).await?;
    let port = listener.local_addr().map_err(|e| format!("local_addr: {e}"))?.port();
    let secret = webdav_guard::generate_secret();
    let guard = Arc::new(Guard::new(port, &secret, read_only, verbose));
    let router = build_router(state, guard);

    let ro_label = if read_only { "read-only" } else { "read-write" };
    let cache_label = if no_cache || cache_ttl == 0 {
        "disabled".to_string()
    } else {
        format!("{cache_ttl}s TTL")
    };
    println!();
    println!(
        "  {} {} {}",
        "\u{25CF}".custom_color(crate::colors::GREEN_OK),
        "WebDAV gateway".custom_color(crate::colors::INK_WARM),
        ro_label.custom_color(crate::colors::INK_DIM),
    );
    println!(
        "  {}  {}",
        "url     ".custom_color(crate::colors::INK_DIM),
        format!("http://localhost:{port}").custom_color(crate::colors::AMBER),
    );
    println!(
        "  {}  {}",
        "user    ".custom_color(crate::colors::INK_DIM),
        "bb (any name works)".custom_color(crate::colors::INK_DIM),
    );
    // The one place the credential is printed. It is random for this run only and is
    // never written to disk or logged.
    println!(
        "  {}  {}",
        "password".custom_color(crate::colors::INK_DIM),
        secret.custom_color(crate::colors::AMBER),
    );
    println!(
        "  {}  {}",
        "mount   ".custom_color(crate::colors::INK_DIM),
        format!("http://bb:{secret}@localhost:{port}/").custom_color(crate::colors::AMBER),
    );
    println!(
        "  {}  {}",
        "mode    ".custom_color(crate::colors::INK_DIM),
        ro_label.custom_color(crate::colors::INK),
    );
    println!(
        "  {}  {}",
        "cache   ".custom_color(crate::colors::INK_DIM),
        cache_label.custom_color(crate::colors::INK_DIM),
    );
    if !verbose {
        println!(
            "  {}  {}",
            "logging ".custom_color(crate::colors::INK_DIM),
            "compact (use --verbose for full request log)".custom_color(crate::colors::INK_DIM),
        );
    }
    if read_only {
        println!(
            "  {}",
            "Read-only: uploads, deletes and renames are refused. Restart with --read-write to allow them."
                .custom_color(crate::colors::INK_DIM),
        );
    } else {
        println!(
            "  {}",
            "Read-write: anything holding the password can change or delete files in your vault."
                .custom_color(crate::colors::AMBER),
        );
    }
    println!();
    println!(
        "  {}",
        format!(
            "# Finder: Go \u{2192} Connect to Server \u{2192} http://localhost:{port} (it asks for the password above)"
        )
        .custom_color(crate::colors::INK_SAGE),
    );
    println!(
        "  {}",
        "# rclone / Cyberduck: use the url, user and password above".custom_color(crate::colors::INK_SAGE),
    );
    if let Some(hint) = windows_explorer_hint() {
        println!("  {}", hint.custom_color(crate::colors::AMBER));
    }
    println!("  {}", "# press Ctrl+C to stop".custom_color(crate::colors::INK_SAGE),);
    println!();

    axum::serve(listener, router)
        .await
        .map_err(|e| format!("server error: {e}"))
}

/// One startup line for Windows only. Explorer's WebDAV redirector refuses HTTP Basic over
/// plain HTTP by default (the `BasicAuthLevel` policy), and the gateway deliberately keeps
/// its per-run password and serves plain HTTP on loopback, so Explorer cannot connect.
/// Compiled everywhere (so it is tested on every platform); returns `None` off Windows.
pub(super) fn windows_explorer_hint() -> Option<&'static str> {
    if cfg!(windows) {
        Some(WINDOWS_EXPLORER_HINT)
    } else {
        None
    }
}

pub(super) const WINDOWS_EXPLORER_HINT: &str = "Windows Explorer is not supported by `bb webdav` (it refuses password login over plain HTTP). Use the Beebeeb desktop app, rclone, Cyberduck or WinSCP.";

/// Bind `127.0.0.1:<port>` and nothing else. There is deliberately no way to pick another
/// address; the post-bind check makes that a property of the code, not of the literal.
pub(super) async fn bind_loopback(port: u16) -> Result<tokio::net::TcpListener, String> {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let listener = tokio::net::TcpListener::bind(addr).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::AddrInUse {
            format!("failed to bind {addr}: port {port} is already in use. Stop the other program (an earlier `bb webdav`?) or pick another port with --port.")
        } else {
            format!("failed to bind {addr}: {e}")
        }
    })?;
    let bound = listener.local_addr().map_err(|e| format!("local_addr: {e}"))?;
    if !bound.ip().is_loopback() {
        return Err(format!("refusing to serve the vault on non-loopback address {bound}"));
    }
    Ok(listener)
}

/// The gateway's router: every route, behind the guard.
pub(super) fn build_router(state: Arc<DavState>, guard: Arc<Guard>) -> Router {
    Router::new()
        .route("/", any(handle_webdav))
        .route("/*path", any(handle_webdav))
        .with_state(state)
        .layer(axum::middleware::from_fn_with_state(guard, webdav_guard::enforce))
}

pub(super) struct DavState {
    pub(super) api: ApiClient,
    pub(super) master_key: beebeeb_core::kdf::MasterKey,
    pub(super) read_only: bool,
    pub(super) dir_cache: DirCache,
    /// Zero duration = caching disabled.
    pub(super) cache_ttl: Duration,
    /// In-memory WebDAV lock store (stub — single-instance only).
    pub(super) locks: LockStore,
    /// Total requests served (atomic for lock-free increment).
    pub(super) request_count: std::sync::atomic::AtomicU64,
    /// Total error responses (atomic for lock-free increment).
    pub(super) error_count: std::sync::atomic::AtomicU64,
    /// Log every request (--verbose).
    pub(super) verbose: bool,
}

pub(super) async fn handle_webdav(State(state): State<Arc<DavState>>, req: Request) -> Response {
    let method = req.method().clone();
    let uri = req.uri().clone();
    let headers = req.headers().clone();
    let path = uri.path().to_string();

    // Suppress noisy logging for macOS/Windows metadata files (DS_Store,
    // ._ resource forks, Spotlight indices, etc.)  These generate many
    // PROPFIND 404s that clutter the console without adding signal.
    let is_metadata = is_os_metadata_path(&path);
    state.request_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if state.verbose && !is_metadata {
        eprintln!(
            "  {} {} {}",
            "WebDAV".custom_color(crate::colors::INK_DIM),
            method.to_string().custom_color(crate::colors::INK),
            path.custom_color(crate::colors::INK_DIM),
        );
    }

    // Reject macOS/Windows metadata files that Finder and Explorer create.
    // These pollute the vault with system junk that users never want stored.
    if is_metadata {
        let is_propfind = method.as_str() == "PROPFIND";
        if is_propfind {
            // Return an empty 207 so Finder doesn't spam retries.
            let body = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
                        <D:multistatus xmlns:D=\"DAV:\"/>";
            let mut headers = HeaderMap::new();
            headers.insert("Content-Type", "application/xml; charset=utf-8".parse().unwrap());
            return (StatusCode::MULTI_STATUS, headers, body).into_response();
        }
        return (StatusCode::FORBIDDEN, "OS metadata files are not stored").into_response();
    }

    // Defence in depth: the guard middleware already refuses writes in read-only mode.
    if webdav_guard::is_write_method(&method) && state.read_only {
        return (
            StatusCode::FORBIDDEN,
            "bb webdav is read-only; restart it with --read-write to allow changes",
        )
            .into_response();
    }

    // Extract the If: (<token>) header once (used by PUT/DELETE to verify locks)
    let if_token = headers.get("if").and_then(|v| v.to_str().ok()).and_then(parse_if_token);

    let response = match method {
        Method::OPTIONS => options_response(&state),
        ref m if m.as_str() == "PROPFIND" => {
            let depth = headers.get("depth").and_then(|v| v.to_str().ok()).unwrap_or("1");
            propfind_response(&state, &path, depth).await
        }
        Method::GET => get_response(&state, &path, &headers).await,
        Method::HEAD => head_response(&state, &path).await,
        Method::PUT => {
            let if_match = headers
                .get("if-match")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string());
            // Verify lock if the path is locked and client provides a token
            if let Err(r) = check_lock(&state, &path, if_token.as_deref()).await {
                return r;
            }
            let body_bytes = match axum::body::to_bytes(req.into_body(), usize::MAX).await {
                Ok(b) => b.to_vec(),
                Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
            };
            put_response(&state, &path, body_bytes, if_match.as_deref()).await
        }
        Method::DELETE => {
            let if_match = headers
                .get("if-match")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string());
            if let Err(r) = check_lock(&state, &path, if_token.as_deref()).await {
                return r;
            }
            delete_response(&state, &path, if_match.as_deref()).await
        }
        ref m if m.as_str() == "MKCOL" => mkcol_response(&state, &path).await,
        ref m if m.as_str() == "MOVE" => {
            let destination = headers
                .get("destination")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();
            move_response(&state, &path, &destination).await
        }
        ref m if m.as_str() == "LOCK" => {
            let timeout_secs = parse_timeout_header(&headers);
            let body_bytes = axum::body::to_bytes(req.into_body(), 64 * 1024)
                .await
                .map(|b| b.to_vec())
                .unwrap_or_default();
            lock_response(&state, &path, &body_bytes, timeout_secs).await
        }
        ref m if m.as_str() == "UNLOCK" => {
            let lock_token = headers
                .get("lock-token")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();
            unlock_response(&state, &path, &lock_token).await
        }
        _ => (StatusCode::METHOD_NOT_ALLOWED, "").into_response(),
    };

    // Track errors for compact logging
    if response.status().is_client_error() || response.status().is_server_error() {
        state.error_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    response
}

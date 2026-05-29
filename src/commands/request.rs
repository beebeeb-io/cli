//! `bb request` — create and manage **file requests**: account-less links that
//! let anyone upload an encrypted file *into* your vault.
//!
//! File requests are the inverse of sharing. The owner creates a request, which
//! mints its own X25519 keypair. The *private* half is wrapped under the owner's
//! master key (via `beebeeb_core::file_request`) and stored server-side as opaque
//! bytes; the *public* half rides only in the link fragment
//! (`…/r/<token>#<R_pub>`) and is never sent to the server. A stranger opens the
//! link, seals a per-file content key to `R_pub` in their own client, and the
//! file lands directly in the folder the owner chose. Only the owner can decrypt.
//!
//! All crypto goes through the shared `core` crate — this module never hand-rolls
//! X25519 / HKDF / AES.
//!
//! ## Key-derivation context (cross-client contract — see note below)
//!
//! `beebeeb_core::file_request::wrap_request_private` and `seal_to_request` take a
//! "context" byte string (the spec calls it `request_id` / `file_id`) that is
//! mixed into the HKDF `info`. The server, however, generates the request `id`,
//! the `token`, and each uploaded `file_id` **after** the relevant client crypto
//! has already happened (the create POST must already carry the wrapped key; the
//! uploader seals before the server assigns a file id). So none of those
//! server-side identifiers are available to the wrapping / sealing party at the
//! moment they are needed.
//!
//! The only context value any client can use consistently — at wrap time *and*
//! unwrap time, at seal time *and* open time — is therefore a fixed constant. We
//! use the empty context for both. Per-request uniqueness is still guaranteed by
//! the random keypair (wrap) and the random ephemeral keypair + nonce (seal);
//! the HKDF context here is defence-in-depth, not load-bearing. **Every client
//! (web/cli/mobile) MUST use the same constant or owners cannot decrypt what they
//! receive** — this is tracked as a cross-client decision for the lead.

use base64::Engine as _;
use beebeeb_core::file_request;
use beebeeb_core::kdf::{FileKey, MasterKey};
use beebeeb_core::opaque::derive_x25519_public;
use colored::Colorize;
use rand::RngCore;
use zeroize::Zeroizing;

use crate::api::ApiClient;
use crate::ui;

/// HKDF context for wrapping a request's X25519 private key under the master key.
/// Empty: the server-assigned request id/token do not exist at wrap time.
const REQUEST_WRAP_CONTEXT: &[u8] = b"";
/// HKDF context for sealing a per-file content key to a request public key.
/// Empty: the server-assigned file id does not exist at seal time.
const REQUEST_SEAL_CONTEXT: &[u8] = b"";

/// Default base used to build the shareable link when the server response does
/// not carry a `request_url` (it normally does). Mirrors `share.rs`.
fn app_url() -> String {
    std::env::var("APP_URL").unwrap_or_else(|_| "https://app.beebeeb.io".to_string())
}

/// Base64-url (no pad) — used for the `#<R_pub>` link fragment and the token.
fn b64url() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
}

/// Standard base64 — used for the wrapped private key + nonce sent to the server
/// (its list endpoint echoes these back in the same alphabet).
fn b64std() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

/// Generate a fresh random X25519 request keypair `(R_priv, R_pub)`.
/// Any 32 random bytes is a valid X25519 scalar (clamping is internal).
fn generate_request_keypair() -> (Zeroizing<[u8; 32]>, [u8; 32]) {
    let mut r_priv = Zeroizing::new([0u8; 32]);
    rand::rngs::OsRng.fill_bytes(&mut *r_priv);
    let r_pub = derive_x25519_public(&r_priv);
    (r_priv, r_pub)
}

/// Assemble the shareable link from the request URL and the request public key.
/// The `#<R_pub>` fragment is base64url; it never reaches the server.
fn build_link(request_url: &str, r_pub: &[u8; 32]) -> String {
    format!("{}#{}", request_url, b64url().encode(r_pub))
}

/// Parse a human duration ("7d", "24h", "30m", "2w", or a bare integer of days)
/// into seconds. Used for `--expires`.
fn parse_expiry_secs(s: &str) -> Result<i64, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty expiry".to_string());
    }
    let (num, unit_secs): (&str, i64) = if let Some(rest) = s.strip_suffix('w') {
        (rest, 7 * 24 * 3600)
    } else if let Some(rest) = s.strip_suffix('d') {
        (rest, 24 * 3600)
    } else if let Some(rest) = s.strip_suffix('h') {
        (rest, 3600)
    } else if let Some(rest) = s.strip_suffix('m') {
        (rest, 60)
    } else {
        // Bare number → days.
        (s, 24 * 3600)
    };
    let n: i64 = num
        .trim()
        .parse()
        .map_err(|_| format!("invalid expiry: {s} (use e.g. 7d, 24h, 30m, 2w)"))?;
    if n <= 0 {
        return Err(format!("expiry must be positive: {s}"));
    }
    Ok(n * unit_secs)
}

/// Parse a human size ("100MB", "2GB", "500MiB", "1048576") into bytes.
/// Decimal suffixes (KB/MB/GB/TB) are powers of 1000; binary suffixes
/// (KiB/MiB/GiB/TiB) are powers of 1024. A bare number is bytes.
fn parse_size_bytes(s: &str) -> Result<i64, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty size".to_string());
    }
    let upper = s.to_ascii_uppercase();
    let units: &[(&str, i64)] = &[
        ("TIB", 1024i64.pow(4)),
        ("GIB", 1024i64.pow(3)),
        ("MIB", 1024i64.pow(2)),
        ("KIB", 1024),
        ("TB", 1000i64.pow(4)),
        ("GB", 1000i64.pow(3)),
        ("MB", 1000i64.pow(2)),
        ("KB", 1000),
        ("B", 1),
    ];
    for (suffix, mult) in units {
        if let Some(rest) = upper.strip_suffix(suffix) {
            let n: f64 = rest.trim().parse().map_err(|_| format!("invalid size: {s}"))?;
            if n < 0.0 {
                return Err(format!("size must be positive: {s}"));
            }
            return Ok((n * *mult as f64).round() as i64);
        }
    }
    // Bare number → bytes.
    s.parse::<i64>()
        .map_err(|_| format!("invalid size: {s} (use e.g. 100MB, 2GB, 500MiB)"))
}

/// Resolve `--folder` to a target folder UUID. Accepts a UUID directly, or a
/// top-level folder name (decrypted locally to match `bb push --folder`).
async fn resolve_folder(api: &ApiClient, master_key: &MasterKey, folder: &str) -> Result<uuid::Uuid, String> {
    if let Ok(uuid) = folder.parse::<uuid::Uuid>() {
        return Ok(uuid);
    }
    // Try to match a root-level folder by decrypted name.
    let resp = api.list_files(None).await?;
    let files = resp
        .get("files")
        .and_then(|v| v.as_array())
        .ok_or("could not list folders")?;
    let mut matches: Vec<uuid::Uuid> = Vec::new();
    for f in files {
        let is_folder = f.get("is_folder").and_then(|v| v.as_bool()).unwrap_or(false);
        if !is_folder {
            continue;
        }
        let id = f.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let name_enc = f.get("name_encrypted").and_then(|v| v.as_str()).unwrap_or("");
        if id.is_empty() || name_enc.is_empty() {
            continue;
        }
        if let Some(name) = crate::crypto::decrypt_name(master_key, id, name_enc) {
            if name == folder {
                if let Ok(uuid) = id.parse::<uuid::Uuid>() {
                    matches.push(uuid);
                }
            }
        }
    }
    match matches.len() {
        0 => Err(format!("no folder found named '{folder}' (pass a folder UUID instead)")),
        1 => Ok(matches[0]),
        _ => Err(format!(
            "multiple folders named '{folder}' — pass a folder UUID instead"
        )),
    }
}

/// `bb request create` — mint a file-request link.
#[allow(clippy::too_many_arguments)]
pub async fn create(
    folder: Option<String>,
    max_files: Option<u32>,
    max_bytes: Option<String>,
    expires: Option<String>,
    title: Option<String>,
    desc: Option<String>,
) -> Result<(), String> {
    let api = ApiClient::from_config();
    api.require_auth()?;
    let master_key = crate::commands::push::load_master_key()?;

    let title = title.unwrap_or_else(|| "File request".to_string());
    if title.trim().is_empty() {
        return Err("title must not be empty".to_string());
    }

    let target_folder_id = match folder {
        Some(ref f) => Some(resolve_folder(&api, &master_key, f).await?),
        None => None,
    };
    let max_total_bytes = match max_bytes {
        Some(ref s) => Some(parse_size_bytes(s)?),
        None => None,
    };
    let expires_in_secs = match expires {
        Some(ref s) => Some(parse_expiry_secs(s)?),
        None => None,
    };

    // 1. Generate the request keypair. 2. Wrap R_priv under the master key.
    let (r_priv, r_pub) = generate_request_keypair();
    let (wrapped, nonce) = file_request::wrap_request_private(&master_key, REQUEST_WRAP_CONTEXT, &r_priv)
        .map_err(|e| format!("failed to wrap request key: {e}"))?;
    let wrapped_b64 = b64std().encode(&wrapped);
    let nonce_b64 = b64std().encode(&nonce);

    // 3. POST — the server stores only the wrapped private key + nonce.
    let result = api
        .create_file_request(
            &title,
            desc.as_deref(),
            target_folder_id,
            max_files,
            max_total_bytes,
            expires_in_secs,
            &wrapped_b64,
            &nonce_b64,
        )
        .await?;

    let id = result.get("id").and_then(|v| v.as_str()).unwrap_or("(unknown)");
    let token = result.get("token").and_then(|v| v.as_str()).unwrap_or("");
    let request_url = result
        .get("request_url")
        .and_then(|v| v.as_str())
        .map(String::from)
        .unwrap_or_else(|| format!("{}/r/{}", app_url().trim_end_matches('/'), token));
    let expires_at = result.get("expires_at").and_then(|v| v.as_str());

    // 4. Build the link by appending the public key in the fragment.
    let link = build_link(&request_url, &r_pub);

    if ui::is_json() {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "id": id,
                "token": token,
                "link": link,
                "title": title,
                "target_folder_id": target_folder_id,
                "max_files": max_files,
                "max_total_bytes": max_total_bytes,
                "expires_at": expires_at,
            }))
            .unwrap_or_default()
        );
        return Ok(());
    }
    if ui::is_quiet() {
        println!("{link}");
        return Ok(());
    }

    println!();
    println!(
        "  {}",
        "\u{2713} File request created".custom_color(crate::colors::GREEN_OK)
    );
    println!(
        "  {} {}",
        "link      ".custom_color(crate::colors::INK_DIM),
        link.custom_color(crate::colors::AMBER),
    );
    println!(
        "  {} {}",
        "title     ".custom_color(crate::colors::INK_DIM),
        title.custom_color(crate::colors::INK),
    );
    if let Some(b) = max_total_bytes {
        println!(
            "  {} {}",
            "max bytes ".custom_color(crate::colors::INK_DIM),
            ui::human_size(b as u64).custom_color(crate::colors::INK),
        );
    }
    if let Some(n) = max_files {
        println!(
            "  {} {}",
            "max files ".custom_color(crate::colors::INK_DIM),
            n.to_string().custom_color(crate::colors::INK),
        );
    }
    let expires_display = match expires_at {
        Some(e) => ui::relative_time(e),
        None => "never".to_string(),
    };
    println!(
        "  {} {}",
        "expires   ".custom_color(crate::colors::INK_DIM),
        expires_display.custom_color(crate::colors::INK),
    );
    println!(
        "  {} {}",
        "id        ".custom_color(crate::colors::INK_DIM),
        id.custom_color(crate::colors::INK_WARM),
    );
    println!();
    println!(
        "  {}",
        "Anyone with this link can upload into your vault. Beebeeb never sees".custom_color(crate::colors::INK_DIM),
    );
    println!(
        "  {}",
        "the files in the clear — only you hold the key to decrypt them.".custom_color(crate::colors::INK_DIM),
    );
    println!(
        "  {}",
        format!("# close anytime:  bb request rm {id}").custom_color(crate::colors::INK_SAGE),
    );

    Ok(())
}

/// `bb request list` — show the owner's file requests with their links.
pub async fn list() -> Result<(), String> {
    let api = ApiClient::from_config();
    api.require_auth()?;
    let master_key = crate::commands::push::load_master_key()?;

    let result = api.list_file_requests().await?;
    let requests = result
        .get("file_requests")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    if ui::is_json() {
        // Enrich each entry with the rebuilt link so JSON consumers get it too.
        let enriched: Vec<serde_json::Value> = requests
            .iter()
            .map(|r| {
                let mut obj = r.clone();
                if let Some(link) = rebuild_link(&master_key, r) {
                    if let Some(map) = obj.as_object_mut() {
                        map.insert("link".to_string(), serde_json::Value::String(link));
                    }
                }
                obj
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&enriched).unwrap_or_default());
        return Ok(());
    }

    if requests.is_empty() {
        if !ui::is_quiet() {
            println!("  {}", "no file requests".custom_color(crate::colors::INK_DIM));
        }
        return Ok(());
    }

    if !ui::is_quiet() {
        println!(
            "  {}",
            format!(
                "  {:<24}  {:<10}  {:<10}  {:<10}  {}",
                "title", "files", "bytes", "status", "expires"
            )
            .custom_color(crate::colors::INK_DIM),
        );
    }

    for r in &requests {
        let title = r.get("title").and_then(|v| v.as_str()).unwrap_or("(untitled)");
        let files_received = r.get("files_received").and_then(|v| v.as_u64()).unwrap_or(0);
        let max_files = r.get("max_files").and_then(|v| v.as_u64());
        let total_bytes = r.get("total_bytes_received").and_then(|v| v.as_u64()).unwrap_or(0);
        let closed = r.get("closed").and_then(|v| v.as_bool()).unwrap_or(false);
        let expires_raw = r.get("expires_at").and_then(|v| v.as_str());

        let is_expired = expires_raw
            .map(|e| {
                chrono::DateTime::parse_from_rfc3339(e)
                    .map(|dt| dt < chrono::Utc::now())
                    .unwrap_or(false)
            })
            .unwrap_or(false);

        let (status_icon, status_label, name_color) = if closed {
            ("\u{2717}", "closed", crate::colors::INK_DIM)
        } else if is_expired {
            ("\u{25CB}", "expired", crate::colors::INK_DIM)
        } else {
            ("\u{25CF}", "open", crate::colors::INK_WARM)
        };

        let files_display = match max_files {
            Some(m) => format!("{files_received}/{m}"),
            None => format!("{files_received}"),
        };
        let expires_display = match expires_raw {
            Some(e) => ui::relative_time(e),
            None => "never".to_string(),
        };
        let link = rebuild_link(&master_key, r).unwrap_or_else(|| "(no link)".to_string());

        if ui::is_quiet() {
            println!("{link}");
            continue;
        }

        let title_trunc: String = title.chars().take(24).collect();
        println!(
            "  {} {:<24}  {:<10}  {:<10}  {:<10}  {}",
            status_icon.custom_color(if closed || is_expired {
                crate::colors::INK_DIM
            } else {
                crate::colors::GREEN_OK
            }),
            title_trunc.custom_color(name_color),
            files_display.custom_color(crate::colors::INK),
            ui::human_size(total_bytes).custom_color(crate::colors::INK_DIM),
            status_label.custom_color(crate::colors::INK_DIM),
            expires_display.custom_color(crate::colors::INK_DIM),
        );
        println!(
            "    {} {}",
            "link".custom_color(crate::colors::INK_DIM),
            link.custom_color(crate::colors::AMBER),
        );
    }

    Ok(())
}

/// Rebuild a request's shareable link from a list-endpoint row by unwrapping the
/// request private key (under the master key) and deriving its public half.
fn rebuild_link(master_key: &MasterKey, request: &serde_json::Value) -> Option<String> {
    let request_url = request.get("request_url").and_then(|v| v.as_str())?;
    let wrapped_b64 = request.get("wrapped_private_key").and_then(|v| v.as_str())?;
    let nonce_b64 = request.get("wrap_nonce").and_then(|v| v.as_str())?;
    let wrapped = decode_any_b64(wrapped_b64)?;
    let nonce = decode_any_b64(nonce_b64)?;
    let r_priv = file_request::unwrap_request_private(master_key, REQUEST_WRAP_CONTEXT, &wrapped, &nonce).ok()?;
    let r_pub = derive_x25519_public(&r_priv);
    Some(build_link(request_url, &r_pub))
}

/// Decode base64 accepting standard or url-safe alphabets, padded or not.
fn decode_any_b64(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    base64::engine::general_purpose::STANDARD
        .decode(s)
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(s))
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(s))
        .or_else(|_| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(s))
        .ok()
}

// ───────────────────────────── Receiving (decrypt) ─────────────────────────
//
// Files uploaded *via a file request* are not encrypted under a master-derived
// file key — the anonymous uploader sealed a random content key `C` to the
// request's public key. The owner recovers `C` by unwrapping the request private
// key (`R_priv`, under the master key) and running ECDH against the uploader's
// ephemeral public key. The server marks these rows by populating
// `file_request_id` + `sender_ephemeral_pubkey` + `wrapped_content_key` (all null
// on normal uploads).

/// The request-upload fields the server attaches to a received file row.
struct RequestUploadFields {
    request_id: uuid::Uuid,
    e_pub: [u8; 32],
    wrapped_content_key: Vec<u8>,
}

/// Extract + validate the request-upload fields from a `GET /files` row. Returns
/// `None` for a normal (non-request) file, i.e. when the fields are absent/null.
fn request_upload_fields(file: &serde_json::Value) -> Option<RequestUploadFields> {
    let request_id = file
        .get("file_request_id")
        .and_then(|v| v.as_str())?
        .parse::<uuid::Uuid>()
        .ok()?;
    let e_pub_bytes = decode_any_b64(file.get("sender_ephemeral_pubkey").and_then(|v| v.as_str())?)?;
    if e_pub_bytes.len() != 32 {
        return None;
    }
    let mut e_pub = [0u8; 32];
    e_pub.copy_from_slice(&e_pub_bytes);
    let wrapped_content_key = decode_any_b64(file.get("wrapped_content_key").and_then(|v| v.as_str())?)?;
    Some(RequestUploadFields {
        request_id,
        e_pub,
        wrapped_content_key,
    })
}

/// Cheap check: does this file row look like a file-request upload?
pub fn is_request_upload(file: &serde_json::Value) -> bool {
    file.get("file_request_id").and_then(|v| v.as_str()).is_some()
        && file.get("sender_ephemeral_pubkey").and_then(|v| v.as_str()).is_some()
        && file.get("wrapped_content_key").and_then(|v| v.as_str()).is_some()
}

/// Resolves the per-file content key for request-uploaded files. Fetches the
/// owner's file requests once (for the wrapped private keys), then unwraps each
/// `R_priv` lazily and caches it — so listing many files from the same request
/// only unwraps once.
pub struct RequestKeyResolver {
    /// request_id → (wrapped_private_key_b64, wrap_nonce_b64) from `GET /file-requests`.
    wrapped: std::collections::HashMap<uuid::Uuid, (String, String)>,
    /// Cache of unwrapped request private keys.
    privs: std::collections::HashMap<uuid::Uuid, Zeroizing<[u8; 32]>>,
}

impl RequestKeyResolver {
    /// Build by fetching `GET /api/v1/file-requests`. Empty (but valid) when the
    /// user owns no requests.
    pub async fn load(api: &ApiClient) -> Result<Self, String> {
        let result = api.list_file_requests().await?;
        let mut wrapped = std::collections::HashMap::new();
        if let Some(items) = result.get("file_requests").and_then(|v| v.as_array()) {
            for r in items {
                let id = r
                    .get("id")
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse::<uuid::Uuid>().ok());
                let w = r.get("wrapped_private_key").and_then(|v| v.as_str());
                let n = r.get("wrap_nonce").and_then(|v| v.as_str());
                if let (Some(id), Some(w), Some(n)) = (id, w, n) {
                    wrapped.insert(id, (w.to_string(), n.to_string()));
                }
            }
        }
        Ok(Self {
            wrapped,
            privs: std::collections::HashMap::new(),
        })
    }

    /// Unwrap (and cache) the request private key for `request_id`.
    fn request_priv(&mut self, master_key: &MasterKey, request_id: uuid::Uuid) -> Option<Zeroizing<[u8; 32]>> {
        if let Some(p) = self.privs.get(&request_id) {
            return Some(p.clone());
        }
        let (w_b64, n_b64) = self.wrapped.get(&request_id)?;
        let wrapped = decode_any_b64(w_b64)?;
        let nonce = decode_any_b64(n_b64)?;
        let r_priv = file_request::unwrap_request_private(master_key, REQUEST_WRAP_CONTEXT, &wrapped, &nonce).ok()?;
        self.privs.insert(request_id, r_priv.clone());
        Some(r_priv)
    }

    /// Recover the content key `C` (as a `FileKey`) for a request-uploaded file
    /// row. Returns `None` for normal files or if the key material is missing /
    /// the row's request is unknown to this owner.
    pub fn content_key(&mut self, master_key: &MasterKey, file: &serde_json::Value) -> Option<FileKey> {
        let fields = request_upload_fields(file)?;
        let r_priv = self.request_priv(master_key, fields.request_id)?;
        let c = file_request::open_request_upload(
            &r_priv,
            &fields.e_pub,
            REQUEST_SEAL_CONTEXT,
            &fields.wrapped_content_key,
        )
        .ok()?;
        Some(FileKey::from_bytes(*c))
    }
}

/// `bb request rm <id>` — close (default) or hard-delete (`--delete`) a request.
pub async fn rm(id: String, delete: bool) -> Result<(), String> {
    let api = ApiClient::from_config();
    api.require_auth()?;

    if delete {
        // NOTE: the server does not yet expose a DELETE route (only `/close`).
        // Attempt it for forward-compat; surface a clear error if unsupported.
        match api.delete_file_request(&id).await {
            Ok(_) => {
                println!(
                    "  {} {}",
                    "Deleted".custom_color(crate::colors::GREEN_OK),
                    format!("· request {id} removed").custom_color(crate::colors::INK_DIM),
                );
                Ok(())
            }
            Err(e) => Err(format!(
                "hard delete failed ({e}). The server may not support deleting \
                 file requests yet — try `bb request rm {id}` (without --delete) to close it."
            )),
        }
    } else {
        api.close_file_request(&id).await?;
        println!(
            "  {} {}",
            "Closed".custom_color(crate::colors::GREEN_OK),
            format!("· request {id} no longer accepts uploads").custom_color(crate::colors::INK_DIM),
        );
        Ok(())
    }
}

/// Parsed components of a file-request link: `<base>/r/<token>#<R_pub>`.
struct ParsedLink {
    token: String,
    r_pub: [u8; 32],
}

/// Parse a file-request link into its token and request public key. The public
/// key lives only in the `#` fragment; a link without it is incomplete.
fn parse_request_link(url: &str) -> Result<ParsedLink, String> {
    let (base, fragment) = url
        .split_once('#')
        .ok_or("this link is incomplete — it has no #<public-key> fragment")?;
    if fragment.trim().is_empty() {
        return Err("this link is incomplete — the #<public-key> fragment is empty".to_string());
    }
    // token is the path segment after "/r/".
    let token = base
        .rsplit_once("/r/")
        .map(|(_, t)| t.trim_end_matches('/'))
        .filter(|t| !t.is_empty())
        .ok_or("could not find the request token in the link (expected …/r/<token>)")?
        .to_string();

    let pub_bytes = b64url()
        .decode(fragment.trim())
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(fragment.trim()))
        .or_else(|_| base64::engine::general_purpose::STANDARD.decode(fragment.trim()))
        .map_err(|_| "the public key in the link is not valid base64".to_string())?;
    if pub_bytes.len() != 32 {
        return Err(format!(
            "the public key in the link must be 32 bytes, got {}",
            pub_bytes.len()
        ));
    }
    let mut r_pub = [0u8; 32];
    r_pub.copy_from_slice(&pub_bytes);
    Ok(ParsedLink { token, r_pub })
}

/// A sealed file ready for upload: the metadata JSON string and the indexed,
/// already-encrypted chunks for the multipart body.
type SealedFile = (String, Vec<(u32, Vec<u8>)>);

/// Seal + encrypt a single file for a request. Returns the metadata JSON and the
/// raw encrypted chunks ready for multipart upload. Pure (no I/O) so it is unit
/// testable and the crypto can be proven without a live server.
fn seal_file_for_request(
    r_pub: &[u8; 32],
    filename: &str,
    mime: Option<&str>,
    plaintext: &[u8],
    chunk_size: usize,
) -> Result<SealedFile, String> {
    // Random per-file content key C.
    let mut content_key = Zeroizing::new([0u8; 32]);
    rand::rngs::OsRng.fill_bytes(&mut *content_key);
    let file_key = FileKey::from_bytes(*content_key);

    // Encrypt the filename (ZK-safe JSON envelope, matching `encrypt_name`).
    let name_meta = serde_json::json!({ "name": filename, "mime_type": mime }).to_string();
    let name_blob =
        beebeeb_core::encrypt::encrypt_metadata(&file_key, &name_meta).map_err(|e| format!("encrypt filename: {e}"))?;
    let name_encrypted = serde_json::to_string(&name_blob).map_err(|e| format!("serialize filename: {e}"))?;

    // Encrypt the file body as raw chunks (nonce ‖ ciphertext), matching the
    // platform's V2 / web chunk format so the owner can reuse the raw decrypt path.
    let chunk_size = chunk_size.max(1);
    let mut chunks: Vec<(u32, Vec<u8>)> = Vec::new();
    if plaintext.is_empty() {
        let blob =
            beebeeb_core::encrypt::encrypt_chunk_raw(&file_key, &[]).map_err(|e| format!("encrypt chunk: {e}"))?;
        chunks.push((0, blob));
    } else {
        for (idx, chunk) in plaintext.chunks(chunk_size).enumerate() {
            let blob = beebeeb_core::encrypt::encrypt_chunk_raw(&file_key, chunk)
                .map_err(|e| format!("encrypt chunk: {e}"))?;
            chunks.push((idx as u32, blob));
        }
    }

    // Seal the content key C to the request public key.
    let sealed = file_request::seal_to_request(r_pub, REQUEST_SEAL_CONTEXT, &content_key)
        .map_err(|e| format!("seal content key: {e}"))?;

    let metadata = serde_json::json!({
        "name_encrypted": name_encrypted,
        "size_bytes": plaintext.len() as i64,
        "sender_ephemeral_pubkey": b64std().encode(sealed.e_pub),
        "wrapped_key": b64std().encode(&sealed.wrapped_key),
    })
    .to_string();

    Ok((metadata, chunks))
}

/// `bb request send <url> <file>...` — upload file(s) to a request, account-less.
pub async fn send(url: String, files: Vec<std::path::PathBuf>) -> Result<(), String> {
    if files.is_empty() {
        return Err("no files to send".to_string());
    }
    let parsed = parse_request_link(&url)?;
    // Uploads go to the configured API server's public endpoint, regardless of
    // the link's (web app) host.
    let api = ApiClient::from_config();

    let mut delivered = 0u32;
    for path in &files {
        let data = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
        let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or("file").to_string();
        let mime = mime_guess::from_path(path).first_raw();

        // Chunk by the same adaptive ladder the rest of the CLI uses.
        let plan = beebeeb_types::plan_chunks(data.len() as u64, beebeeb_types::ChunkProfile::Desktop);
        let chunk_size = plan.chunk_size_bytes.max(1) as usize;

        let (metadata, chunks) = seal_file_for_request(&parsed.r_pub, &filename, mime, &data, chunk_size)?;
        let result = api.upload_to_file_request(&parsed.token, &metadata, &chunks).await?;

        delivered += 1;
        if ui::is_json() {
            println!("{}", serde_json::to_string_pretty(&result).unwrap_or_default());
        } else if !ui::is_quiet() {
            let received = result.get("files_received").and_then(|v| v.as_u64());
            println!(
                "  {} {}{}",
                "\u{2713}".custom_color(crate::colors::GREEN_OK),
                format!("{filename} ({})", ui::human_size(data.len() as u64)).custom_color(crate::colors::INK),
                received
                    .map(|n| format!("  ·  {n} received")
                        .custom_color(crate::colors::INK_DIM)
                        .to_string())
                    .unwrap_or_default(),
            );
        }
    }

    if !ui::is_quiet() && !ui::is_json() {
        println!(
            "  {}",
            format!("Delivered {delivered} file(s) · sealed to the request key, end-to-end encrypted")
                .custom_color(crate::colors::INK_SAGE),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use beebeeb_core::kdf::derive_master_key;

    fn test_master_key() -> MasterKey {
        derive_master_key("file-request-cli-test", b"sixteen-byte-salt").unwrap()
    }

    #[test]
    fn parse_expiry_units() {
        assert_eq!(parse_expiry_secs("7d").unwrap(), 7 * 24 * 3600);
        assert_eq!(parse_expiry_secs("24h").unwrap(), 24 * 3600);
        assert_eq!(parse_expiry_secs("30m").unwrap(), 30 * 60);
        assert_eq!(parse_expiry_secs("2w").unwrap(), 2 * 7 * 24 * 3600);
        assert_eq!(parse_expiry_secs("5").unwrap(), 5 * 24 * 3600); // bare → days
        assert!(parse_expiry_secs("0d").is_err());
        assert!(parse_expiry_secs("abc").is_err());
    }

    #[test]
    fn parse_sizes() {
        assert_eq!(parse_size_bytes("1000").unwrap(), 1000);
        assert_eq!(parse_size_bytes("1KB").unwrap(), 1000);
        assert_eq!(parse_size_bytes("1KiB").unwrap(), 1024);
        assert_eq!(parse_size_bytes("100MB").unwrap(), 100_000_000);
        assert_eq!(parse_size_bytes("2GiB").unwrap(), 2 * 1024 * 1024 * 1024);
        assert!(parse_size_bytes("notasize").is_err());
    }

    #[test]
    fn link_assembly_roundtrips_through_parse() {
        // build_link + parse_request_link are inverses for token + R_pub.
        let (_priv, r_pub) = generate_request_keypair();
        let request_url = "https://app.beebeeb.io/r/ABCtoken_123-xyz";
        let link = build_link(request_url, &r_pub);
        assert!(link.contains("/r/ABCtoken_123-xyz#"));

        let parsed = parse_request_link(&link).unwrap();
        assert_eq!(parsed.token, "ABCtoken_123-xyz");
        assert_eq!(parsed.r_pub, r_pub);
    }

    #[test]
    fn parse_link_rejects_missing_fragment() {
        assert!(parse_request_link("https://app.beebeeb.io/r/tok").is_err());
        assert!(parse_request_link("https://app.beebeeb.io/r/tok#").is_err());
    }

    #[test]
    fn keypair_wrap_unwrap_roundtrip_matches_create_then_list() {
        // Mirrors the create → list flow: wrap R_priv at create, then unwrap it
        // from the list response (base64) and re-derive R_pub for the link.
        let mk = test_master_key();
        let (r_priv, r_pub) = generate_request_keypair();

        let (wrapped, nonce) = file_request::wrap_request_private(&mk, REQUEST_WRAP_CONTEXT, &r_priv).unwrap();
        let wrapped_b64 = b64std().encode(&wrapped);
        let nonce_b64 = b64std().encode(&nonce);

        // Simulate the list endpoint echoing these back.
        let row = serde_json::json!({
            "request_url": "https://app.beebeeb.io/r/tok123",
            "wrapped_private_key": wrapped_b64,
            "wrap_nonce": nonce_b64,
        });
        let link = rebuild_link(&mk, &row).expect("link rebuilds");
        let expected = build_link("https://app.beebeeb.io/r/tok123", &r_pub);
        assert_eq!(link, expected);
    }

    #[test]
    fn send_seal_then_owner_open_recovers_file() {
        // End-to-end crypto without a server: owner creates a keypair, uploader
        // seals a file to R_pub, owner unwraps R_priv and decrypts name + body.
        let mk = test_master_key();
        let (r_priv, r_pub) = generate_request_keypair();
        let (wrapped, nonce) = file_request::wrap_request_private(&mk, REQUEST_WRAP_CONTEXT, &r_priv).unwrap();

        let body = b"the quick brown fox jumps over the lazy dog, twice over.".repeat(4);
        let (metadata, chunks) = seal_file_for_request(&r_pub, "secret.txt", Some("text/plain"), &body, 16).unwrap();

        // Parse what the uploader would POST.
        let meta: serde_json::Value = serde_json::from_str(&metadata).unwrap();
        let e_pub_bytes = b64std()
            .decode(meta["sender_ephemeral_pubkey"].as_str().unwrap())
            .unwrap();
        let mut e_pub = [0u8; 32];
        e_pub.copy_from_slice(&e_pub_bytes);
        let wrapped_key = b64std().decode(meta["wrapped_key"].as_str().unwrap()).unwrap();
        let name_encrypted = meta["name_encrypted"].as_str().unwrap();
        assert_eq!(meta["size_bytes"].as_i64().unwrap(), body.len() as i64);

        // Owner side: unwrap R_priv, open the content key, decrypt name + chunks.
        let recovered_priv = file_request::unwrap_request_private(&mk, REQUEST_WRAP_CONTEXT, &wrapped, &nonce).unwrap();
        let content_key =
            file_request::open_request_upload(&recovered_priv, &e_pub, REQUEST_SEAL_CONTEXT, &wrapped_key).unwrap();
        let file_key = FileKey::from_bytes(*content_key);

        // Decrypt filename.
        let blob: beebeeb_types::EncryptedBlob = serde_json::from_str(name_encrypted).unwrap();
        let decrypted_meta = beebeeb_core::encrypt::decrypt_metadata(&file_key, &blob).unwrap();
        let meta_json: serde_json::Value = serde_json::from_str(&decrypted_meta).unwrap();
        assert_eq!(meta_json["name"].as_str().unwrap(), "secret.txt");

        // Decrypt the body via the shared raw-chunk path.
        let mut all = Vec::new();
        for (_idx, c) in &chunks {
            all.extend_from_slice(c);
        }
        let plaintext = crate::crypto::try_decrypt_all_chunks(&file_key, &all, chunks.len() as u32).expect("decrypts");
        assert_eq!(plaintext, body);
    }

    #[test]
    fn receive_decrypts_request_uploaded_file_row() {
        // Full receiving path: an uploader seals a file; the server stores the
        // request fields on the file row; the owner's RequestKeyResolver recovers
        // the content key from a `GET /files`-shaped row and decrypts name + body.
        let mk = test_master_key();
        let request_id = "11111111-2222-3333-4444-555555555555".parse::<uuid::Uuid>().unwrap();
        let (r_priv, r_pub) = generate_request_keypair();
        let (wrapped, nonce) = file_request::wrap_request_private(&mk, REQUEST_WRAP_CONTEXT, &r_priv).unwrap();

        // Uploader side (mirrors `bb request send`).
        let body = b"received over a file request, sealed to the request key.".repeat(3);
        let (metadata, chunks) =
            seal_file_for_request(&r_pub, "received.bin", Some("application/octet-stream"), &body, 24).unwrap();
        let meta: serde_json::Value = serde_json::from_str(&metadata).unwrap();

        // Server-shaped file row (what GET /files returns for a request upload).
        let file_row = serde_json::json!({
            "id": "99999999-8888-7777-6666-555555555555",
            "name_encrypted": meta["name_encrypted"].as_str().unwrap(),
            "chunk_count": chunks.len(),
            "is_folder": false,
            "file_request_id": request_id.to_string(),
            "sender_ephemeral_pubkey": meta["sender_ephemeral_pubkey"].as_str().unwrap(),
            "wrapped_content_key": meta["wrapped_key"].as_str().unwrap(),
        });
        assert!(is_request_upload(&file_row));

        // A normal file row must NOT be treated as a request upload.
        let normal_row = serde_json::json!({
            "id": "00000000-0000-0000-0000-000000000000",
            "name_encrypted": "{}",
            "file_request_id": serde_json::Value::Null,
        });
        assert!(!is_request_upload(&normal_row));

        // Owner side: build a resolver seeded with the wrapped private key (as the
        // list endpoint would return it) and recover the content key.
        let mut resolver = RequestKeyResolver {
            wrapped: std::collections::HashMap::from([(
                request_id,
                (b64std().encode(&wrapped), b64std().encode(&nonce)),
            )]),
            privs: std::collections::HashMap::new(),
        };
        let content_key = resolver.content_key(&mk, &file_row).expect("recovers content key");

        // Name decrypts via the request content key.
        let name =
            crate::crypto::decrypt_name_with_key(&content_key, file_row["name_encrypted"].as_str().unwrap()).unwrap();
        assert_eq!(name, "received.bin");

        // Body decrypts via the shared raw-chunk path.
        let mut all = Vec::new();
        for (_idx, c) in &chunks {
            all.extend_from_slice(c);
        }
        let plaintext = crate::crypto::try_decrypt_all_chunks(&content_key, &all, chunks.len() as u32).unwrap();
        assert_eq!(plaintext, body);

        // R_priv is cached after the first resolve.
        assert!(resolver.privs.contains_key(&request_id));

        // An unknown request id yields no key (graceful, not a panic).
        let orphan_row = serde_json::json!({
            "file_request_id": "deadbeef-0000-0000-0000-000000000000",
            "sender_ephemeral_pubkey": meta["sender_ephemeral_pubkey"].as_str().unwrap(),
            "wrapped_content_key": meta["wrapped_key"].as_str().unwrap(),
        });
        assert!(resolver.content_key(&mk, &orphan_row).is_none());
    }
}

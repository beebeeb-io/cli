//! `bb account` — profile show, email change, export, delete.
//!
//! Routes used:
//!   GET    /api/v1/auth/me
//!   GET    /api/v1/billing/subscription
//!   GET    /api/v1/account/security-score   (note: /account, not /auth/account)
//!   GET    /api/v1/account/sessions
//!   GET    /api/v1/auth/passkeys
//!   GET    /api/v1/me/region
//!
//!   POST   /api/v1/me/email/start            (OPAQUE)
//!   POST   /api/v1/me/email/finish           (OPAQUE)
//!   PUT    /api/v1/auth/account/email        (legacy password accounts)
//!
//!   POST   /api/v1/auth/account/export
//!   GET    /api/v1/auth/account/export/{id}
//!   GET    /api/v1/auth/account/export/{id}/download
//!
//!   DELETE /api/v1/auth/account
//!
//! All mutating endpoints require X-Confirm-Token from POST /api/v1/auth/confirm.

use std::path::PathBuf;

use crate::api::ApiClient;

pub async fn show() -> Result<(), String> {
    // Full implementation in Task 4.
    let _api = ApiClient::from_config();
    Err("bb account show — not implemented yet".to_string())
}

pub async fn update_email(_new_email: String) -> Result<(), String> {
    Err("bb account update — not implemented yet".to_string())
}

pub async fn export_start() -> Result<(), String> {
    Err("bb account export — not implemented yet".to_string())
}

pub async fn export_status(_job_id: String) -> Result<(), String> {
    Err("bb account export status — not implemented yet".to_string())
}

pub async fn export_download(_job_id: String, _output: Option<PathBuf>) -> Result<(), String> {
    Err("bb account export download — not implemented yet".to_string())
}

pub async fn delete(_confirm: String) -> Result<(), String> {
    Err("bb account delete — not implemented yet".to_string())
}

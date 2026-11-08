//! Wire types generated from `contracts/openapi.json` — do not edit by hand.
//!
//! Generator: typify 0.8.0 (pinned; chosen 2026-11-02).
//! Source: the server's `refactor/modular-split` at 1cf4b6d9c78118c7d2b49c9efd5537514c8274c1
//! (`contracts/SERVER_REV`), vendored verbatim as `contracts/openapi.json`.
//!
//! Regenerate with `scripts/openapi-types.sh --write`; CI runs
//! `scripts/openapi-types.sh --check` and fails on any drift.

#[doc = "Billing plan definition."]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct BillingPlan {
    pub features: ::std::vec::Vec<::std::string::String>,
    pub id: ::std::string::String,
    pub name: ::std::string::String,
    #[doc = "Task 1743: OMITTED (not zero) when the caller's purchase-surface policy row hides\nprices (a store build, an unknown or header-less caller). A client must treat a\nmissing price as \"do not show one\", never as free."]
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub price_eur: ::std::option::Option<f64>,
    #[doc = "Omitted under a hidden-price row, like `price_eur`."]
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub price_yearly_eur: ::std::option::Option<f64>,
    #[doc = "Task 1743: DISPLAY policy, not access control. True when the plan is publicly\npurchasable AND this client's policy row has a purchase surface. `/billing/checkout`\ndoes not enforce it."]
    pub purchasable_here: bool,
    pub storage_bytes: i64,
    pub storage_label: ::std::string::String,
    #[doc = "Task 1743: the server's own tier order (free 0 < starter < basic < pro < business),\nthe order the upgrade cap uses."]
    pub tier_rank: i32,
}
#[doc = "Result of a bulk Trash request."]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct BulkTrashResponse {
    #[doc = "Owned IDs that were already in Trash before this request."]
    pub already_trashed: ::std::vec::Vec<::uuid::Uuid>,
    #[doc = "IDs that do not exist or are not owned by the authenticated user."]
    pub missing: ::std::vec::Vec<::uuid::Uuid>,
    #[doc = "Owned, non-trashed IDs moved to Trash by this request."]
    pub trashed: ::std::vec::Vec<::uuid::Uuid>,
}
#[doc = "One chunk of a legacy upload (`PUT /files/{id}/chunks/{index}`). `skipped`\nis present only when the chunk was already stored."]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct ChunkUploadResponse {
    pub index: i32,
    pub size: i64,
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub skipped: ::std::option::Option<bool>,
}
#[doc = "A completed legacy upload (`POST /files/{id}/upload/complete`)."]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct CompleteUploadResponse {
    pub chunk_count: i32,
    pub created_at: ::std::string::String,
    pub id: ::uuid::Uuid,
    pub is_uploading: bool,
    pub name_encrypted: ::std::string::String,
    pub size_bytes: i64,
    pub storage_location: StorageLocation,
    pub updated_at: ::std::string::String,
    pub uploaded_at: ::std::string::String,
}
#[doc = "Single-use step-up confirmation token."]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct ConfirmationResponse {
    #[doc = "Raw token, shown once; only its SHA-256 hash is stored."]
    pub confirmation_token: ::std::string::String,
    pub expires_at: ::std::string::String,
}
#[doc = "The files already present in a target folder (`POST /files/check-conflict`)."]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct ConflictCheckResponse {
    pub files: ::std::vec::Vec<ConflictFile>,
}
#[doc = "One same-name file the conflict check reports."]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct ConflictFile {
    pub id: ::uuid::Uuid,
    pub name_encrypted: ::std::string::String,
}
#[doc = "One chunk of a [`DownloadVersion`]."]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct DownloadChunk {
    pub index: i32,
    pub size_bytes: i64,
}
#[doc = "One recorded object version, as the clients receive it."]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct DownloadVersion {
    pub chunk_count: i32,
    pub chunk_size_bytes: i64,
    pub chunks: ::std::vec::Vec<DownloadChunk>,
    pub ciphertext_size_bytes: i64,
    pub object_version_id: ::uuid::Uuid,
    pub plaintext_size_bytes: i64,
    pub version_number: i32,
}
#[doc = "Generic error response."]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct ErrorResponse {
    pub error: ::std::string::String,
}
#[doc = "The account's file counters (`GET /files/count`)."]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct FileCountResponse {
    pub total_bytes: i64,
    pub total_files: i64,
    pub total_folders: i64,
    pub trashed_bytes: i64,
    pub trashed_files: i64,
}
#[doc = "A file or folder's full metadata (`GET /files/{id}`). Fields that only apply\nto files (the storage location, file-request envelope) or only to the owner\n(`share_count`) are absent for the callers they do not apply to; a present\n`null` means the field applies but has no value."]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct FileDetailResponse {
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub acted_via_share_id: ::std::option::Option<::uuid::Uuid>,
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub actor_name_encrypted: ::std::option::Option<::std::string::String>,
    pub chunk_count: i32,
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub chunk_size_bytes: ::std::option::Option<i64>,
    pub created_at: ::std::string::String,
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub download_version: ::std::option::Option<DownloadVersion>,
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub file_count: ::std::option::Option<i64>,
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub file_request_id: ::std::option::Option<::uuid::Uuid>,
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub folder_count: ::std::option::Option<i64>,
    pub has_large_thumbnail: bool,
    pub has_preview: bool,
    pub id: ::uuid::Uuid,
    pub is_folder: bool,
    pub is_trashed: bool,
    pub is_uploading: bool,
    pub name_encrypted: ::std::string::String,
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub note_encrypted: ::std::option::Option<::std::string::String>,
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub owner_wrapped_file_key: ::std::option::Option<::std::string::String>,
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub parent_id: ::std::option::Option<::uuid::Uuid>,
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub preview_bytes: ::std::option::Option<i64>,
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub preview_format: ::std::option::Option<::std::string::String>,
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub preview_px: ::std::option::Option<i32>,
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub sender_ephemeral_pubkey: ::std::option::Option<::std::string::String>,
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub share_count: ::std::option::Option<i64>,
    pub size_bytes: i64,
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub storage_location: ::std::option::Option<StorageLocation>,
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub thumbnail_bytes: ::std::option::Option<i32>,
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub thumbnail_encrypted: ::std::option::Option<::std::string::String>,
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub total_bytes: ::std::option::Option<i64>,
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub trashed_at: ::std::option::Option<::std::string::String>,
    pub updated_at: ::std::string::String,
    pub uploaded_at: ::std::string::String,
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub uploaded_by: ::std::option::Option<::uuid::Uuid>,
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub wrapped_content_key: ::std::option::Option<::std::string::String>,
}
#[doc = "File listing response."]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct FileListResponse {
    pub files: ::std::vec::Vec<FileMetadata>,
    #[doc = "Opaque keyset-pagination cursor (task 0739). Non-null only when a full\n`limit` page was served — pass it back as the `cursor` query param to\nfetch the next page. `null` means this was the last page."]
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub next_cursor: ::std::option::Option<::std::string::String>,
}
#[doc = "File or folder metadata."]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct FileMetadata {
    pub chunk_count: i32,
    pub created_at: ::std::string::String,
    pub id: ::uuid::Uuid,
    pub is_folder: bool,
    pub is_trashed: bool,
    #[doc = "AES-256-GCM encrypted filename (JSON-serialised `EncryptedBlob`)."]
    pub name_encrypted: ::std::string::String,
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub parent_id: ::std::option::Option<::uuid::Uuid>,
    pub size_bytes: i64,
    #[doc = "Storage location for transparency."]
    pub storage_location: StorageLocation,
    pub updated_at: ::std::string::String,
}
#[doc = "`GET /files` — one page of the caller's files."]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct FilesListResponse {
    pub files: ::std::vec::Vec<ListedFile>,
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub next_cursor: ::std::option::Option<::std::string::String>,
}
#[doc = "One invoice row in `GET /api/v1/billing/invoices`."]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct InvoiceSummary {
    pub amount_gross_cents: i64,
    pub amount_net_cents: i64,
    pub currency: ::std::string::String,
    pub description: ::std::string::String,
    pub id: ::uuid::Uuid,
    pub invoice_date: ::std::string::String,
    pub number: ::std::string::String,
    #[doc = "Relative path of the invoice PDF (`/api/v1/billing/invoices/{id}/pdf`)."]
    pub pdf_url: ::std::string::String,
    pub period_end: ::std::string::String,
    pub period_start: ::std::string::String,
    pub status: ::std::string::String,
    pub vat_amount_cents: i64,
    pub vat_rate_bps: i32,
    pub vat_treatment: ::std::string::String,
}
#[doc = "The caller's invoice list."]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct InvoicesResponse {
    pub invoices: ::std::vec::Vec<InvoiceSummary>,
}
#[doc = "The ended trial behind `SubscriptionResponse.last_trial`."]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct LastTrial {
    #[doc = "Whether a real payment settled since the trial started (a `trial_checkout:`\nmandate payment or an admin `manual_` invoice does not count)."]
    pub charged: bool,
    #[doc = "RFC 3339 time the unpaid end lapsed or downgraded the row."]
    pub ended_at: ::std::string::String,
    #[doc = "`no_card`, `mandated` or `promo`."]
    pub kind: ::std::string::String,
}
#[doc = "One row of `GET /files`."]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct ListedFile {
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub acted_via_share_id: ::std::option::Option<::uuid::Uuid>,
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub actor_name_encrypted: ::std::option::Option<::std::string::String>,
    pub chunk_count: i32,
    pub created_at: ::std::string::String,
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub download_version: ::std::option::Option<DownloadVersion>,
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub file_request_id: ::std::option::Option<::uuid::Uuid>,
    pub has_large_thumbnail: bool,
    pub has_preview: bool,
    pub has_thumbnail: bool,
    pub id: ::uuid::Uuid,
    pub is_folder: bool,
    pub is_media: bool,
    pub is_starred: bool,
    pub is_trashed: bool,
    pub is_uploading: bool,
    pub name_encrypted: ::std::string::String,
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub note_encrypted: ::std::option::Option<::std::string::String>,
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub owner_wrapped_file_key: ::std::option::Option<::std::string::String>,
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub parent_id: ::std::option::Option<::uuid::Uuid>,
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub preview_bytes: ::std::option::Option<i64>,
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub preview_format: ::std::option::Option<::std::string::String>,
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub preview_px: ::std::option::Option<i32>,
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub sender_ephemeral_pubkey: ::std::option::Option<::std::string::String>,
    pub share_count: i64,
    pub size_bytes: i64,
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub storage_location: ::std::option::Option<StorageLocation>,
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub thumbnail_bytes: ::std::option::Option<i32>,
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub thumbnail_encrypted: ::std::option::Option<::std::string::String>,
    pub updated_at: ::std::string::String,
    pub uploaded_at: ::std::string::String,
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub uploaded_by: ::std::option::Option<::uuid::Uuid>,
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub wrapped_content_key: ::std::option::Option<::std::string::String>,
}
#[doc = "Authenticated user profile."]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct MeResponse {
    #[doc = "The impersonating admin, while `is_impersonation` is true."]
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub admin_user_id: ::std::option::Option<::uuid::Uuid>,
    pub created_at: ::std::string::String,
    pub email: ::std::string::String,
    pub email_verified: bool,
    #[doc = "Set while the account is frozen; null otherwise."]
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub frozen_at: ::std::option::Option<::std::string::String>,
    #[doc = "True when the calling session was minted by an admin impersonation."]
    pub is_impersonation: bool,
    pub role: ::std::string::String,
    pub totp_enabled: bool,
    pub user_id: ::uuid::Uuid,
}
#[doc = "Single-message acknowledgement."]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct MessageResponse {
    pub message: ::std::string::String,
}
#[doc = "Acknowledgement for an idempotent write (`{\"ok\": true}`)."]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct OkResponse {
    pub ok: bool,
}
#[doc = "Billing plan list."]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct PlansResponse {
    pub plans: ::std::vec::Vec<BillingPlan>,
}
#[doc = "One row of the picker a client renders. The list is data, never a client\nconstant: a country appears as `live` only while one of its `live`\ndatacenters has a healthy pool with a live client, and `coming_soon` rows\ncome from `datacenters.status`.\n\nNever carries a city, a datacenter id, the hosting provider, an endpoint or\na pool id (canon: docs/canon/data-residency.md, task 2050;\n`tests/regions_provider_leak.rs`, `tests/storage_country_2050.rs`)."]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct RegionChoice {
    #[doc = "The country name for a country entry, `null` for the region entry."]
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub country: ::std::option::Option<::std::string::String>,
    #[doc = "Upper-case ISO code (`\"DE\"`) for a country entry, `null` for the region entry."]
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub country_code: ::std::option::Option<::std::string::String>,
    #[doc = "What `PUT /me/region` takes and `GET /me/region` returns: `\"europe\"` or a\nlower-case ISO country code (`\"de\"`, `\"fi\"`, `\"nl\"`)."]
    pub id: ::std::string::String,
    #[doc = "True only on the \"Europe\" entry: the placement a user with no choice gets."]
    pub is_default: bool,
    #[doc = "`\"region\"` (the default \"any data center\" entry) or `\"country\"`."]
    pub kind: ::std::string::String,
    #[doc = "Display name: `\"Europe\"` for the region entry (from the continent code,\n[`crate::storage::continent_display_name`]), the English country name for\na country (`\"Germany\"`)."]
    pub name: ::std::string::String,
    #[doc = "`status == \"live\"`; the one flag a client needs to enable the row."]
    pub selectable: bool,
    #[doc = "`\"live\"` or `\"coming_soon\"`."]
    pub status: ::std::string::String,
}
#[doc = "`RegionPreferenceWithChoices`"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct RegionPreferenceWithChoices {
    pub available_regions: ::std::vec::Vec<::serde_json::Value>,
    pub choices: ::std::vec::Vec<RegionChoice>,
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub preferred_region: ::std::option::Option<::std::string::String>,
    pub region_strict: bool,
}
#[doc = "A restore (`POST /files/{id}/restore`). A group too large to restore in one\nrequest answers `202` with the job fields; the synchronous answer omits them."]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct RestoreFileResponse {
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub complete: ::std::option::Option<bool>,
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub job_id: ::std::option::Option<::uuid::Uuid>,
    pub message: ::std::string::String,
    pub rehomed_to_root: bool,
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub remaining: ::std::option::Option<i64>,
    pub restored_count: u64,
}
#[doc = "One live session of the current account."]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct SessionListItem {
    pub created_at: ::std::string::String,
    pub expires_at: ::std::string::String,
    pub id: ::uuid::Uuid,
    #[doc = "True for the session that made this request."]
    pub is_current: bool,
}
#[doc = "Live sessions of the current account, newest first."]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct SessionListResponse {
    pub sessions: ::std::vec::Vec<SessionListItem>,
}
#[doc = "The joined file fields of a listed share."]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct ShareFileRef {
    pub name_encrypted: ::std::string::String,
    pub size_bytes: i64,
}
#[doc = "Share link list item (returned by `GET /shares/mine`)."]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct ShareListItem {
    pub created_at: ::std::string::String,
    pub download_count: i32,
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub expires_at: ::std::option::Option<::std::string::String>,
    pub file: ShareFileRef,
    pub file_id: ::uuid::Uuid,
    pub has_passphrase: bool,
    pub id: ::uuid::Uuid,
    pub item_count: i64,
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub last_opened_at: ::std::option::Option<::std::string::String>,
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub max_opens: ::std::option::Option<i32>,
    pub open_count: i32,
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub owner_wrapped_key: ::std::option::Option<::std::string::String>,
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub owner_wrapped_token: ::std::option::Option<::std::string::String>,
    pub revoked: bool,
    pub share_type: ::std::string::String,
}
#[doc = "Share list response."]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct ShareListResponse {
    pub shares: ::std::vec::Vec<ShareListItem>,
}
#[doc = "Share link information (single-file share)."]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct ShareResponse {
    pub created_at: ::std::string::String,
    pub double_encrypted: bool,
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub expires_at: ::std::option::Option<::std::string::String>,
    pub has_passphrase: bool,
    pub id: ::uuid::Uuid,
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub max_opens: ::std::option::Option<i32>,
    pub share_type: ::std::string::String,
    #[doc = "Opaque token included in the shareable URL fragment."]
    pub token: ::std::string::String,
}
#[doc = "Storage location information: the broad region and the country, never the\nhosting provider (brand/legal rule, docs/canon/data-residency.md; the\n`provider` field was removed in task 1947, the city deprecated in task 2050)."]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct StorageLocation {
    #[doc = "DEPRECATED (task 2050): datacenter city, kept one release for shipped clients."]
    pub city: ::std::string::String,
    #[doc = "Country name, e.g. `\"Germany\"`; null when unknown (task 2050)."]
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub country: ::std::option::Option<::std::string::String>,
    #[doc = "ISO 3166-1 alpha-2, e.g. `\"DE\"`; null when unknown (task 2050)."]
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub country_code: ::std::option::Option<::std::string::String>,
    #[doc = "Broad geographic area, e.g. `\"europe\"`."]
    pub region: ::std::string::String,
}
#[doc = "The account's storage usage (`GET /files/usage`)."]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct StorageUsageResponse {
    pub plan_limit_bytes: i64,
    pub plan_name: ::std::string::String,
    pub used_bytes: i64,
}
#[doc = "Active subscription details."]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct SubscriptionResponse {
    pub billing_cycle: ::std::string::String,
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub current_period_end: ::std::option::Option<::std::string::String>,
    #[doc = "Task 1601 — additive. The plan the user is entitled to RIGHT NOW:\n`plan` when `status` is an entitled status (see\n`plan_catalog::ACTIVE_SUBSCRIBER_STATUSES` — for `status = \"cancelling\"`\nspecifically, only while `current_period_end` is set and still in the\nfuture), else `\"free\"`. Prefer this over deriving from `plan` + `status`\nyourself."]
    pub effective_plan: ::std::string::String,
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub last_trial: ::std::option::Option<LastTrial>,
    pub plan: ::std::string::String,
    pub quota_bytes: i64,
    pub status: ::std::string::String,
    pub used_bytes: i64,
}
#[doc = "A thumbnail upload (`PUT /files/{id}/thumbnail[/{variant}]`)."]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct ThumbnailUploadResponse {
    pub message: ::std::string::String,
    pub variant: ::std::string::String,
}
#[doc = "Personal Access Token list item (no raw token value)."]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct TokenListItem {
    pub created_at: ::std::string::String,
    #[doc = "Expiry timestamp; null for a non-expiring token."]
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub expires_at: ::std::option::Option<::std::string::String>,
    pub id: ::uuid::Uuid,
    #[doc = "False once revoked or expired."]
    pub is_active: bool,
    #[doc = "Last time the token authenticated a request; null if never used."]
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub last_used_at: ::std::option::Option<::std::string::String>,
    pub name: ::std::string::String,
    #[doc = "Revocation timestamp; null while active."]
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub revoked_at: ::std::option::Option<::std::string::String>,
    #[doc = "Always `[]`: scopes are not enforced yet."]
    pub scopes: ::std::vec::Vec<::std::string::String>,
}
#[doc = "Token list response."]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct TokenListResponse {
    pub tokens: ::std::vec::Vec<TokenListItem>,
}
#[doc = "A single move to Trash (`DELETE /files/{id}`)."]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct TrashFileResponse {
    pub message: ::std::string::String,
    pub trashed_count: u64,
}
#[doc = "The metadata a rename/note edit returns (`PATCH /files/{id}`)."]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct UpdateFileResponse {
    pub chunk_count: i32,
    pub created_at: ::std::string::String,
    pub id: ::uuid::Uuid,
    pub is_folder: bool,
    pub is_trashed: bool,
    pub is_uploading: bool,
    pub name_encrypted: ::std::string::String,
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub note_encrypted: ::std::option::Option<::std::string::String>,
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub parent_id: ::std::option::Option<::uuid::Uuid>,
    pub size_bytes: i64,
    pub updated_at: ::std::string::String,
    pub uploaded_at: ::std::string::String,
}
#[doc = "Storage usage summary."]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct UsageResponse {
    #[doc = "Fraction 0.0–1.0 of quota consumed."]
    pub percentage: f64,
    pub quota_bytes: i64,
    #[doc = "Configured trash retention period in days (task 1549 finding 3) — the\nlive, admin-configurable value from `admin_settings.trash_retention_days`\n(default 30, range 1–365), so clients stop hardcoding \"30 days\"."]
    pub trash_retention_days: i32,
    pub used_bytes: i64,
}
#[doc = " Error types."]
pub mod error {
    #[doc = r" Error from a `TryFrom` or `FromStr` implementation."]
    pub struct ConversionError(::std::borrow::Cow<'static, str>);
    impl ::std::error::Error for ConversionError {}
    impl ::std::fmt::Display for ConversionError {
        fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> Result<(), ::std::fmt::Error> {
            ::std::fmt::Display::fmt(&self.0, f)
        }
    }
    impl ::std::fmt::Debug for ConversionError {
        fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> Result<(), ::std::fmt::Error> {
            ::std::fmt::Debug::fmt(&self.0, f)
        }
    }
    impl From<&'static str> for ConversionError {
        fn from(value: &'static str) -> Self {
            Self(value.into())
        }
    }
    impl From<String> for ConversionError {
        fn from(value: String) -> Self {
            Self(value.into())
        }
    }
}

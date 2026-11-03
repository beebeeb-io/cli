//! CL-P3-11 — the generated wire types accept the recorded responses and
//! re-serialise to the same value.
//!
//! `tests/fixtures/openapi/wire.json` holds one document-shaped response per
//! generated type, committed next to `contracts/openapi.json`. The repository
//! holds no live capture for these routes, so each fixture was transcribed
//! once from the document's required fields and is pinned here: the test fails
//! both when a generated shape stops accepting its fixture and when generated
//! serde attributes start dropping or renaming fields on the way back out.
//!
//! The last assertion is the replacement proof: the one hand-written struct the
//! document covers (`ShareEntry`) is built from the generated `ShareListItem`
//! and from the old lenient parser, and the two views must be identical.

use super::generated as g;
use serde_json::Value;

const FIXTURES: &str = include_str!("../../../tests/fixtures/openapi/wire.json");

macro_rules! roundtrip {
    ($fixtures:expr, $name:literal, $ty:ty) => {{
        let raw = $fixtures[$name].clone();
        assert!(!raw.is_null(), "no fixture for {}", $name);
        let typed: $ty = serde_json::from_value(raw.clone())
            .unwrap_or_else(|e| panic!("{} does not accept its fixture: {e}", $name));
        assert_eq!(
            serde_json::to_value(&typed).unwrap(),
            raw,
            "{} did not round-trip",
            $name
        );
    }};
}

#[test]
fn generated_types_roundtrip_the_recorded_fixtures() {
    let fixtures: Value = serde_json::from_str(FIXTURES).expect("fixtures are valid JSON");

    roundtrip!(fixtures, "BillingPlan", g::BillingPlan);
    roundtrip!(fixtures, "BulkTrashResponse", g::BulkTrashResponse);
    roundtrip!(fixtures, "ChunkUploadResponse", g::ChunkUploadResponse);
    roundtrip!(fixtures, "CompleteUploadResponse", g::CompleteUploadResponse);
    roundtrip!(fixtures, "ConfirmationResponse", g::ConfirmationResponse);
    roundtrip!(fixtures, "ConflictCheckResponse", g::ConflictCheckResponse);
    roundtrip!(fixtures, "ConflictFile", g::ConflictFile);
    roundtrip!(fixtures, "DownloadChunk", g::DownloadChunk);
    roundtrip!(fixtures, "DownloadVersion", g::DownloadVersion);
    roundtrip!(fixtures, "ErrorResponse", g::ErrorResponse);
    roundtrip!(fixtures, "FileCountResponse", g::FileCountResponse);
    roundtrip!(fixtures, "FileDetailResponse", g::FileDetailResponse);
    roundtrip!(fixtures, "FileListResponse", g::FileListResponse);
    roundtrip!(fixtures, "FileMetadata", g::FileMetadata);
    roundtrip!(fixtures, "FilesListResponse", g::FilesListResponse);
    roundtrip!(fixtures, "InvoiceSummary", g::InvoiceSummary);
    roundtrip!(fixtures, "InvoicesResponse", g::InvoicesResponse);
    roundtrip!(fixtures, "LastTrial", g::LastTrial);
    roundtrip!(fixtures, "ListedFile", g::ListedFile);
    roundtrip!(fixtures, "MeResponse", g::MeResponse);
    roundtrip!(fixtures, "MessageResponse", g::MessageResponse);
    roundtrip!(fixtures, "OkResponse", g::OkResponse);
    roundtrip!(fixtures, "PlansResponse", g::PlansResponse);
    roundtrip!(fixtures, "RegionChoice", g::RegionChoice);
    roundtrip!(fixtures, "RegionPreferenceWithChoices", g::RegionPreferenceWithChoices);
    roundtrip!(fixtures, "RestoreFileResponse", g::RestoreFileResponse);
    roundtrip!(fixtures, "SessionListItem", g::SessionListItem);
    roundtrip!(fixtures, "SessionListResponse", g::SessionListResponse);
    roundtrip!(fixtures, "ShareFileRef", g::ShareFileRef);
    roundtrip!(fixtures, "ShareListItem", g::ShareListItem);
    roundtrip!(fixtures, "ShareListResponse", g::ShareListResponse);
    roundtrip!(fixtures, "ShareResponse", g::ShareResponse);
    roundtrip!(fixtures, "StorageLocation", g::StorageLocation);
    roundtrip!(fixtures, "StorageUsageResponse", g::StorageUsageResponse);
    roundtrip!(fixtures, "SubscriptionResponse", g::SubscriptionResponse);
    roundtrip!(fixtures, "ThumbnailUploadResponse", g::ThumbnailUploadResponse);
    roundtrip!(fixtures, "TokenListItem", g::TokenListItem);
    roundtrip!(fixtures, "TokenListResponse", g::TokenListResponse);
    roundtrip!(fixtures, "TrashFileResponse", g::TrashFileResponse);
    roundtrip!(fixtures, "UpdateFileResponse", g::UpdateFileResponse);
    roundtrip!(fixtures, "UsageResponse", g::UsageResponse);

    // The replacement: the generated row and the hand-written parser produce
    // the same display view for the same response.
    let master_key =
        beebeeb_core::kdf::derive_master_key("test-password", b"test-salt-16bytes").expect("test master key");
    let row = fixtures["ShareListItem"].clone();
    let (from_wire, from_legacy) = crate::commands::share::share_entry_views(&row, &master_key);
    assert_eq!(
        from_wire, from_legacy,
        "the generated ShareListItem must produce the old ShareEntry view"
    );
}

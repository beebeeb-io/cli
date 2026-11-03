use super::current_version_number;
use serde_json::json;

#[test]
fn reads_download_version_first_then_the_top_level() {
    // Today's server: only inside download_version.
    assert_eq!(
        current_version_number(&json!({"download_version": {"version_number": 3}})),
        Some(3)
    );
    // Both present: download_version wins.
    assert_eq!(
        current_version_number(&json!({"version_number": 1, "download_version": {"version_number": 7}})),
        Some(7)
    );
    // Older shape: top level only.
    assert_eq!(current_version_number(&json!({"version_number": 2})), Some(2));
    // A file with no completed version reports none.
    assert_eq!(current_version_number(&json!({"download_version": null})), None);
    assert_eq!(current_version_number(&json!({})), None);
}

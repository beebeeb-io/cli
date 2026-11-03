use super::ApiError;

/// `Display` is the user-facing text, unchanged from the old
/// `Result<_, String>` contract, and `From<ApiError> for String` keeps it.
#[test]
fn display_is_the_message() {
    let e = ApiError::from("boom");
    assert_eq!(e.to_string(), "boom");
    assert_eq!(String::from(e), "boom");
}

/// A server code is matchable without leaking into the display text.
#[test]
fn code_is_matchable_but_not_displayed() {
    let e = ApiError::test_code("unauthorized");
    assert!(e.is_code("unauthorized"));
    assert_eq!(e.to_string(), "unauthorized");
}

//! L13 (SF-0001): an owner create writes only under the id this client chose.
//!
//! Every create (upload init, folder create, webdav PUT/MKCOL) mints its own
//! id, proposes it, and then compares the id the server echoed in the answer
//! with it — right after the answer, before any chunk is PUT, any name is
//! sealed under it, any rename happens or the id is used as a parent. A server
//! that answers a different id could otherwise make the client seal a new item
//! under an existing row's derived key (a holder of that key reads it).
//!
//! A difference is a typed [`ServerChangedItemId`] and a plain message; nothing
//! is uploaded under the echoed id.

use uuid::Uuid;

/// The one sentence the user sees when an answer echoed a different item.
pub const CHANGED_ID_MESSAGE: &str = "The server answered with a different item. Nothing was uploaded under it.";

/// The typed abort: the server's answer named a different item than the one
/// this client minted for the write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerChangedItemId {
    /// The id this client minted and sent.
    pub expected: Uuid,
    /// The id the answer carried, as it was written (unparsable included).
    pub received: Option<String>,
}

impl std::fmt::Display for ServerChangedItemId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(CHANGED_ID_MESSAGE)
    }
}

impl std::error::Error for ServerChangedItemId {}

/// Compare the id an init, create or completion answer echoed with the id this
/// client minted. The echo is compared byte for byte with the canonical
/// lowercase hyphenated v4 spelling — an uppercase, braced, simple or
/// `urn:uuid:` spelling of the same id is a different answer, because a client
/// that accepted it would have to trust a second parser to agree with the
/// first. A missing or unparsable echo is a difference, and so is an `expected`
/// that is not a v4 id this client could have minted (fail closed). Returns the
/// client's own id on a match, so callers keep using it.
pub fn verify_echoed_id(expected: Uuid, echoed: Option<&str>) -> Result<Uuid, ServerChangedItemId> {
    let canonical = (expected.get_version_num() == 4).then(|| expected.hyphenated().to_string());
    match (canonical.as_deref(), echoed) {
        (Some(canonical), Some(got)) if got == canonical => Ok(expected),
        _ => Err(ServerChangedItemId {
            expected,
            received: echoed.map(str::to_string),
        }),
    }
}

/// The `id` a JSON answer echoes, if any. The v2 init answers `file_id`; folder
/// creates and completion answer `id`.
pub fn echoed_id(answer: &serde_json::Value) -> Option<&str> {
    answer.get("id").and_then(|v| v.as_str())
}

/// Create one owner folder named under `new_id` (minted by the caller), require
/// the answer to echo that same id, and return it. The name is sealed under the
/// client's id, never under the one the answer carries.
pub async fn create_owned_folder(
    api: &crate::api::ApiClient,
    master_key: &beebeeb_core::kdf::MasterKey,
    name: &str,
    parent: Option<Uuid>,
    new_id: Uuid,
) -> Result<Uuid, String> {
    let name_encrypted = beebeeb_core::encrypt::encrypt_name(master_key, &new_id.to_string(), name, None)
        .map_err(|e| format!("failed to encrypt folder name: {e}"))?;
    let answer = api.create_folder(&name_encrypted, parent, Some(new_id)).await?;
    verify_echoed_id(new_id, echoed_id(&answer)).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The canonical v4 id the tests mint: lowercase, hyphenated.
    const MINE: &str = "3f2b1c9d-8a7e-4f6b-9c5d-4e3a2b1c0d9e";

    fn mine() -> Uuid {
        Uuid::parse_str(MINE).expect("test id parses")
    }

    #[test]
    fn matching_echo_is_accepted_and_returns_the_clients_id() {
        let id = mine();
        assert_eq!(verify_echoed_id(id, Some(MINE)), Ok(id));
    }

    #[test]
    fn the_same_id_in_another_spelling_is_a_different_answer() {
        // Uuid::parse accepts all of these; the L13 check must not.
        let id = mine();
        for echoed in [
            MINE.to_uppercase(),
            format!("{{{MINE}}}"),
            id.simple().to_string(),
            format!("urn:uuid:{MINE}"),
            format!("{MINE} "),
        ] {
            let err = verify_echoed_id(id, Some(&echoed)).unwrap_err();
            assert_eq!(err.to_string(), CHANGED_ID_MESSAGE, "{echoed:?}");
        }
    }

    #[test]
    fn an_id_that_is_not_v4_is_refused_even_when_echoed_verbatim() {
        // A v1 id and the nil id are not ids this client could have minted.
        for not_v4 in [
            Uuid::nil(),
            Uuid::parse_str("3f2b1c9d-8a7e-1f6b-9c5d-4e3a2b1c0d9e").unwrap(),
        ] {
            let echoed = not_v4.hyphenated().to_string();
            let err = verify_echoed_id(not_v4, Some(&echoed)).unwrap_err();
            assert_eq!(err.to_string(), CHANGED_ID_MESSAGE, "{not_v4}");
        }
    }

    #[test]
    fn a_different_echo_aborts_with_the_plain_message() {
        let mine = mine();
        let other = Uuid::from_u128(7);
        let err = verify_echoed_id(mine, Some(&other.to_string())).unwrap_err();
        assert_eq!(err.expected, mine);
        assert_eq!(err.received.as_deref(), Some(other.to_string().as_str()));
        assert_eq!(err.to_string(), CHANGED_ID_MESSAGE);
    }

    #[test]
    fn a_missing_or_unparsable_echo_aborts() {
        let mine = mine();
        for echoed in [None, Some("not-a-uuid"), Some("")] {
            let err = verify_echoed_id(mine, echoed).unwrap_err();
            assert_eq!(err.to_string(), CHANGED_ID_MESSAGE, "{echoed:?}");
        }
    }

    #[test]
    fn echoed_id_reads_a_json_answer() {
        assert_eq!(echoed_id(&serde_json::json!({"id": "abc"})), Some("abc"));
        assert_eq!(echoed_id(&serde_json::json!({"file_id": "abc"})), None);
        assert_eq!(echoed_id(&serde_json::json!({"id": 3})), None);
    }
}

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

/// Mint the id a create proposes: a random (v4) id, the only kind
/// [`verify_minted_echo`] accepts.
pub fn mint() -> Uuid {
    Uuid::new_v4()
}

/// Compare the id an init, create or completion answer echoed with the id this
/// write proposed (minted here for a create, the listed row's id for a
/// replace). The echo is compared byte for byte with the canonical lowercase
/// hyphenated spelling of that id: an uppercase, braced, simple or `urn:uuid:`
/// spelling of the same id is a different answer, because a client that
/// accepted it would have to trust a second parser to agree with the first. A
/// missing or unparsable echo is a difference (fail closed). Returns the
/// client's own id on a match, so callers keep using it.
pub fn verify_echoed_id(expected: Uuid, echoed: Option<&str>) -> Result<Uuid, ServerChangedItemId> {
    match echoed {
        Some(got) if got == expected.hyphenated().to_string() => Ok(expected),
        _ => Err(ServerChangedItemId {
            expected,
            received: echoed.map(str::to_string),
        }),
    }
}

/// [`verify_echoed_id`] for an id this client MINTED for a create: it must also
/// be a random (v4, RFC 4122 variant) id, the only kind [`mint`] makes. Anything
/// else is refused even when echoed verbatim (fail closed: a caller passed an id
/// it did not mint). A replace proposes an id it listed, not one it minted, and
/// uses [`verify_echoed_id`].
pub fn verify_minted_echo(minted: Uuid, echoed: Option<&str>) -> Result<Uuid, ServerChangedItemId> {
    let v4 = minted.get_version() == Some(uuid::Version::Random) && minted.get_variant() == uuid::Variant::RFC4122;
    if !v4 {
        return Err(ServerChangedItemId {
            expected: minted,
            received: echoed.map(str::to_string),
        });
    }
    verify_echoed_id(minted, echoed)
}

/// Who chose the id a write proposes, which decides how its echo is checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Proposed {
    /// A create: this client minted the id ([`verify_minted_echo`]).
    Minted(Uuid),
    /// A replace: the id of the row this client listed ([`verify_echoed_id`]).
    Listed(Uuid),
}

impl Proposed {
    /// The proposed id.
    pub fn id(self) -> Uuid {
        match self {
            Proposed::Minted(id) | Proposed::Listed(id) => id,
        }
    }

    /// Check an answer's echo against the proposed id.
    pub fn verify(self, echoed: Option<&str>) -> Result<Uuid, ServerChangedItemId> {
        match self {
            Proposed::Minted(id) => verify_minted_echo(id, echoed),
            Proposed::Listed(id) => verify_echoed_id(id, echoed),
        }
    }
}

/// The `id` a JSON answer echoes, if any. The v2 init answers `file_id`; folder
/// creates and completion answer `id`.
pub fn echoed_id(answer: &serde_json::Value) -> Option<&str> {
    answer.get("id").and_then(|v| v.as_str())
}

/// Create one owner folder: mint its id here, seal the name under it, require
/// the answer to echo that same id, and return it. The name is sealed under the
/// client's id, never under the one the answer carries.
pub async fn create_owned_folder(
    api: &crate::api::ApiClient,
    master_key: &beebeeb_core::kdf::MasterKey,
    name: &str,
    parent: Option<Uuid>,
) -> Result<Uuid, String> {
    let new_id = mint();
    let name_encrypted = beebeeb_core::encrypt::encrypt_name(master_key, &new_id.to_string(), name, None)
        .map_err(|e| format!("failed to encrypt folder name: {e}"))?;
    let answer = api.create_folder(&name_encrypted, parent, Some(new_id)).await?;
    verify_minted_echo(new_id, echoed_id(&answer)).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The canonical v4 id the tests mint: lowercase, hyphenated.
    const MINE: &str = "3f2b1c9d-8a7e-4f6b-9c5d-4e3a2b1c0d9e";

    fn mine() -> Uuid {
        Uuid::parse_str(MINE).expect("test id parses")
    }

    /// One of the two comparisons.
    type Verify = fn(Uuid, Option<&str>) -> Result<Uuid, ServerChangedItemId>;

    /// Both comparisons: the one for any proposed id, and the one for an id
    /// this client minted.
    fn both() -> [(&'static str, Verify); 2] {
        [
            ("verify_echoed_id", verify_echoed_id),
            ("verify_minted_echo", verify_minted_echo),
        ]
    }

    #[test]
    fn matching_echo_is_accepted_and_returns_the_clients_id() {
        for (which, verify) in both() {
            assert_eq!(verify(mine(), Some(MINE)), Ok(mine()), "{which}");
        }
    }

    #[test]
    fn the_same_id_in_another_spelling_is_a_different_answer() {
        // Uuid::parse accepts all of these; the L13 check must not.
        let id = mine();
        for (which, verify) in both() {
            for echoed in [
                MINE.to_uppercase(),
                format!("{{{MINE}}}"),
                id.simple().to_string(),
                format!("urn:uuid:{MINE}"),
                format!("{MINE} "),
            ] {
                let err = verify(id, Some(&echoed)).unwrap_err();
                assert_eq!(err.to_string(), CHANGED_ID_MESSAGE, "{which}: {echoed:?}");
            }
        }
    }

    #[test]
    fn a_minted_id_that_is_not_v4_is_refused_even_when_echoed_verbatim() {
        // A v1 id, a v4-numbered id of another variant and the nil id are not
        // ids this client could have minted.
        for not_v4 in [
            Uuid::nil(),
            Uuid::parse_str("3f2b1c9d-8a7e-1f6b-9c5d-4e3a2b1c0d9e").unwrap(),
            Uuid::parse_str("3f2b1c9d-8a7e-4f6b-1c5d-4e3a2b1c0d9e").unwrap(),
        ] {
            let echoed = not_v4.hyphenated().to_string();
            let err = verify_minted_echo(not_v4, Some(&echoed)).unwrap_err();
            assert_eq!(err.to_string(), CHANGED_ID_MESSAGE, "{not_v4}");
        }
    }

    #[test]
    fn a_listed_id_is_compared_by_spelling_only() {
        // A replace proposes the row it listed, which this client did not
        // mint: its echo is checked exactly, but its version is not ours to
        // require.
        let listed = Uuid::parse_str("3f2b1c9d-8a7e-1f6b-9c5d-4e3a2b1c0d9e").unwrap();
        let echoed = listed.hyphenated().to_string();
        assert_eq!(verify_echoed_id(listed, Some(&echoed)), Ok(listed));
        assert!(verify_echoed_id(listed, Some(&echoed.to_uppercase())).is_err());
    }

    #[test]
    fn proposed_checks_a_minted_id_as_minted_and_a_listed_id_as_listed() {
        let v1 = Uuid::parse_str("3f2b1c9d-8a7e-1f6b-9c5d-4e3a2b1c0d9e").unwrap();
        let echoed = v1.hyphenated().to_string();
        assert!(
            Proposed::Minted(v1).verify(Some(&echoed)).is_err(),
            "not an id this client mints"
        );
        assert_eq!(Proposed::Listed(v1).verify(Some(&echoed)), Ok(v1));
        assert_eq!(Proposed::Minted(mine()).verify(Some(MINE)), Ok(mine()));
        assert!(Proposed::Listed(mine()).verify(Some(&MINE.to_uppercase())).is_err());
    }

    #[test]
    fn mint_makes_ids_the_minted_check_accepts() {
        for _ in 0..32 {
            let id = mint();
            assert_eq!(verify_minted_echo(id, Some(&id.to_string())), Ok(id));
        }
    }

    #[test]
    fn a_different_echo_aborts_with_the_plain_message() {
        let mine = mine();
        let other = Uuid::from_u128(7);
        for (which, verify) in both() {
            let err = verify(mine, Some(&other.to_string())).unwrap_err();
            assert_eq!(err.expected, mine, "{which}");
            assert_eq!(err.received.as_deref(), Some(other.to_string().as_str()), "{which}");
            assert_eq!(err.to_string(), CHANGED_ID_MESSAGE, "{which}");
        }
    }

    #[test]
    fn a_missing_or_unparsable_echo_aborts() {
        let mine = mine();
        for (which, verify) in both() {
            for echoed in [None, Some("not-a-uuid"), Some("")] {
                let err = verify(mine, echoed).unwrap_err();
                assert_eq!(err.to_string(), CHANGED_ID_MESSAGE, "{which}: {echoed:?}");
            }
        }
    }

    #[test]
    fn echoed_id_reads_a_json_answer() {
        assert_eq!(echoed_id(&serde_json::json!({"id": "abc"})), Some("abc"));
        assert_eq!(echoed_id(&serde_json::json!({"file_id": "abc"})), None);
        assert_eq!(echoed_id(&serde_json::json!({"id": 3})), None);
    }

    fn master() -> beebeeb_core::kdf::MasterKey {
        beebeeb_core::kdf::MasterKey::from_bytes([0u8; 32])
    }

    /// A8 (every folder create: mkdir, push, sync, webdav MKCOL): the answer
    /// echoes a different id. The create aborts with the plain message and no
    /// id is returned for use as a parent.
    #[tokio::test]
    async fn folder_create_echoing_a_different_id_aborts() {
        let other = Uuid::new_v4();
        let state = crate::test_server::shared(crate::test_server::Mock {
            folder_echo: Some(other.to_string()),
            ..Default::default()
        });
        let api = crate::api::ApiClient::new_for_test(crate::test_server::spawn(state.clone()).await);
        let err = create_owned_folder(&api, &master(), "Folder", None)
            .await
            .expect_err("must abort");
        assert_eq!(err, CHANGED_ID_MESSAGE);
        let sent = state.lock().unwrap().folder_requests.clone();
        assert_eq!(sent.len(), 1, "one create");
        assert_ne!(sent[0], other.to_string(), "the client proposed its own id");
    }

    /// Normal flow: the client mints a canonical v4 id, sends it, and the
    /// answer's echo of it is what the caller gets.
    #[tokio::test]
    async fn folder_create_echoing_the_clients_id_succeeds() {
        let state = crate::test_server::shared(crate::test_server::Mock::default());
        let api = crate::api::ApiClient::new_for_test(crate::test_server::spawn(state.clone()).await);
        let id = create_owned_folder(&api, &master(), "Folder", None)
            .await
            .expect("succeeds");
        assert_eq!(id.get_version(), Some(uuid::Version::Random));
        assert_eq!(state.lock().unwrap().folder_requests, vec![id.hyphenated().to_string()]);
    }
}

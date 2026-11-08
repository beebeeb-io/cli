//! Task 2334, review #3: inside a folder the owner shares, a same-name folder a collaborator made has a
//! foreign id and a name sealed under the shared folder's key, not under the owner's master key. A re-push of
//! that folder must find it through the same key resolver `bb ls` uses, and write into it, never make a
//! second one beside it.
//!
//! Black box: the real `bb` against the shared-folder mock of `tests/support/shared_folder.rs`, which serves
//! the owner's custody of ROOT and ROOT's child-key inventory. Scratch `HOME`, loopback only.
#[path = "support/shared_folder.rs"]
mod sf;
use serde_json::json;
use sf::*;

async fn bb(h: &Harness, args: &[&str]) -> std::process::Output {
    let (home, api) = (h.home.clone(), h.api.clone());
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    tokio::task::spawn_blocking(move || {
        std::process::Command::new(env!("CARGO_BIN_EXE_bb"))
            .args(["--api", &api])
            .args(args)
            .current_dir(&home)
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("BB_REFUSE_NON_LOOPBACK_API", "1")
            .env("BB_NO_UPDATE", "1")
            .env("BB_TRANSPORT", "api")
            .env("NO_COLOR", "1")
            .env_remove("APP_URL")
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap()
    })
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_collaborators_same_name_folder_in_a_shared_folder_is_reused() {
    let mut f = Fixture::basic();
    let mut nested = row(
        F_SUB,
        Some(ROOT),
        "Nested",
        RowFacts {
            folder: true,
            foreign: true,
        },
    );
    nested["acted_via_share_id"] = json!(null);
    nested["uploaded_by"] = json!(BAD);
    // The real listing always carries it: no public link on this folder.
    nested["share_count"] = json!(0);
    f.rows.push(nested);
    f.inventory
        .push(json!({"file_id":F_SUB,"encrypted_file_key":wrap(F_SUB,true)}));
    // The completion answers the id this push minted (not the fixture's guest file).
    f.review_complete_requested_id = true;
    let h = Harness::new(f).await;
    std::fs::create_dir_all(h.home.join("Nested")).unwrap();
    std::fs::write(h.home.join("Nested/new.txt"), b"a new file").unwrap();

    let out = bb(&h, &["push", "--parent", ROOT, "Nested"]).await;

    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert_eq!(
        h.count("POST", "/api/v1/files/folder"),
        0,
        "no second Nested is made beside the collaborator's: {stderr}"
    );
    let upload = h.state.lock().unwrap().upload_request.clone();
    assert_eq!(
        upload["parent_id"],
        json!(F_SUB),
        "the file goes into the collaborator's Nested"
    );
    h.close().await;
}

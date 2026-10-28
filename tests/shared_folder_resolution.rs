#[path = "support/shared_folder.rs"]
mod sf;
use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use serde_json::json;
use sf::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owner_wrap_after_move() {
    let mut f = Fixture::default();
    let mut r = row(FILE, None, "moved.txt", false, true);
    r["acted_via_share_id"] = json!(null);
    r["owner_wrapped_file_key"] =
        json!(B64.encode(beebeeb_core::owned_keys::wrap_key_to_owner(&master(), FILE, &item_key()).unwrap()));
    f.rows.push(r);
    let h = Harness::new(f).await;
    let v = h.json(&["ls"]).await;
    assert_eq!(v["files"][0]["name"], "moved.txt");
    assert_eq!(h.count("GET", "/api/v1/shared-folders"), 0);
    h.close().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nested_guest_and_member_names() {
    let mut f = Fixture::basic();
    f.rows[1]["parent_id"] = json!(SUB);
    let mut sub = row(SUB, Some(ROOT), "Nested", true, true);
    sub["acted_via_share_id"] = json!(null);
    sub["uploaded_by"] = json!(BAD);
    f.rows.push(sub);
    f.inventory
        .push(json!({"file_id":SUB,"encrypted_file_key":wrap(SUB,true)}));
    let h = Harness::new(f).await;
    let v = h.json(&["ls", "Shared/Nested"]).await;
    let resolved_id = v["files"][0]["id"].as_str().unwrap();
    assert_eq!(resolved_id, FILE);
    assert_eq!(v["files"][0]["name"], "guest.txt");
    assert_eq!(h.count("GET", &format!("/api/v1/shared-folders/{ROOT}/keys")), 1);
    h.close().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn canonical_enrichment_wire_shapes() {
    for array in [false, true] {
        let mut f = Fixture::default();
        let mut canonical = row(FILE, None, "canonical.txt", false, true);
        let frame = beebeeb_core::owned_keys::wrap_key_to_owner(&master(), FILE, &item_key()).unwrap();
        canonical["owner_wrapped_file_key"] = if array { json!(frame) } else { json!(B64.encode(frame)) };
        let mut sparse = canonical.clone();
        sparse.as_object_mut().unwrap().remove("owner_wrapped_file_key");
        sparse.as_object_mut().unwrap().remove("acted_via_share_id");
        sparse.as_object_mut().unwrap().remove("uploaded_by");
        f.rows.push(sparse);
        f.canonical.insert(FILE.into(), canonical);
        let h = Harness::new(f).await;
        let v = h.json(&["ls"]).await;
        assert_eq!(v["files"][0]["name"], "canonical.txt");
        assert_eq!(h.count("GET", &format!("/api/v1/files/{FILE}")), 1);
        h.close().await;
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cursor_ties_and_bad_rows() {
    let mut f = Fixture::basic();
    let target = f.inventory.pop().unwrap();
    for n in 0..500 {
        f.inventory
            .push(json!({"file_id":uuid::Uuid::from_u128(n+1).to_string(),"encrypted_file_key":"malformed"}));
    }
    f.inventory.push(target);
    let mut bad = row(BAD, Some(ROOT), "not plaintext", false, true);
    bad["name_encrypted"] = json!("unsafe-plaintext");
    f.rows.push(bad);
    let h = Harness::new(f).await;
    let v = h.json(&["ls", "Shared"]).await;
    assert!(v["files"].as_array().unwrap().iter().any(|r| r["name"] == "guest.txt"));
    let unreadable_visible_rows = v["files"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["name"].as_str().is_some_and(|s| s.contains("can't open")))
        .count();
    assert_eq!(unreadable_visible_rows, 1);
    assert_eq!(h.count("GET", &format!("/api/v1/shared-folders/{ROOT}/keys")), 2);
    h.close().await;
    let mut f = Fixture::basic();
    f.cycle = true;
    let h = Harness::new(f).await;
    let v = h.json(&["ls", "Shared"]).await;
    assert!(v["files"][0]["name"].as_str().unwrap().contains("can't open"));
    assert!(h.count("GET", &format!("/api/v1/shared-folders/{ROOT}/keys")) <= 2);
    h.close().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn legacy_and_request_compatibility() {
    let mut f = Fixture::default();
    let mut legacy = row(FILE, None, "string.txt", false, false);
    f.rows.push(legacy.clone());
    let binary = beebeeb_core::kdf::derive_file_key(&master(), uuid::Uuid::parse_str(BAD).unwrap().as_bytes());
    legacy["id"] = json!(BAD);
    legacy["name_encrypted"] = json!(name(&binary, "binary.txt"));
    f.rows.push(legacy);
    let h = Harness::new(f).await;
    let v = h.json(&["ls"]).await;
    assert_eq!(v["files"][0]["name"], "binary.txt");
    assert_eq!(v["files"][1]["name"], "string.txt");
    h.close().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reads_never_adopt() {
    let h = Harness::new(Fixture::basic()).await;
    let ls = h.json(&["ls", "Shared"]).await;
    assert_eq!(ls["files"][0]["name"], "guest.txt");
    let out = h.run(&["search", "guest"]).await;
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).contains("guest.txt"));
    let owner_key_posts = h.count("POST", "/api/v1/files/");
    assert_eq!(owner_key_posts, 0);
    assert_eq!(
        h.state
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter(|(m, _, _)| m != "GET")
            .count(),
        0
    );
    h.close().await;
}

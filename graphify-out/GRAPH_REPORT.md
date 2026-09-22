# Graph Report - cli-0479  (2026-09-22)

## Corpus Check
- 57 files · ~83,156 words
- Verdict: corpus is large enough that graph structure adds value.

## Summary
- 719 nodes · 1982 edges · 16 communities detected
- Extraction: 69% EXTRACTED · 31% INFERRED · 0% AMBIGUOUS · INFERRED: 617 edges (avg confidence: 0.8)
- Token cost: 0 input · 0 output

## Community Hubs (Navigation)
- [[_COMMUNITY_Community 0|Community 0]]
- [[_COMMUNITY_Community 1|Community 1]]
- [[_COMMUNITY_Community 2|Community 2]]
- [[_COMMUNITY_Community 3|Community 3]]
- [[_COMMUNITY_Community 4|Community 4]]
- [[_COMMUNITY_Community 5|Community 5]]
- [[_COMMUNITY_Community 6|Community 6]]
- [[_COMMUNITY_Community 7|Community 7]]
- [[_COMMUNITY_Community 8|Community 8]]
- [[_COMMUNITY_Community 9|Community 9]]
- [[_COMMUNITY_Community 10|Community 10]]
- [[_COMMUNITY_Community 11|Community 11]]
- [[_COMMUNITY_Community 12|Community 12]]
- [[_COMMUNITY_Community 13|Community 13]]
- [[_COMMUNITY_Community 14|Community 14]]
- [[_COMMUNITY_Community 15|Community 15]]

## God Nodes (most connected - your core abstractions)
1. `ApiClient` - 77 edges
2. `parse_response()` - 62 edges
3. `is_json()` - 41 edges
4. `is_quiet()` - 38 edges
5. `run()` - 36 edges
6. `main()` - 28 edges
7. `load_master_key()` - 28 edges
8. `load_master_key()` - 27 edges
9. `run()` - 21 edges
10. `BeebeebFs` - 21 edges

## Surprising Connections (you probably didn't know these)
- `expected_ciphertext_for()` --calls--> `run()`  [INFERRED]
  src/upload.rs → src/commands/sync.rs
- `generate_from_file()` --calls--> `upload_file_to()`  [INFERRED]
  src/thumbnail.rs → src/commands/sync.rs
- `generate_from_file()` --calls--> `push_single_file()`  [INFERRED]
  src/thumbnail.rs → src/commands/push.rs
- `generate_large_from_file()` --calls--> `upload_file_to()`  [INFERRED]
  src/thumbnail.rs → src/commands/sync.rs
- `generate_large_from_file()` --calls--> `push_single_file()`  [INFERRED]
  src/thumbnail.rs → src/commands/push.rs

## Communities

### Community 0 - "Community 0"
Cohesion: 0.04
Nodes (76): build_show_payload(), build_show_payload_assembles_all_sections(), build_show_payload_degrades_gracefully_per_section(), map_email_change_error(), normalize_update_email(), opaque_email_change(), print_email_change_success(), render_progress_bar() (+68 more)

### Community 1 - "Community 1"
Cohesion: 0.07
Nodes (19): ApiClient, backoff(), build_client(), every_request_carries_client_and_version_headers(), format_request_error(), ids(), is_transient_transport_error(), last_page_null_cursor_terminates_the_walk() (+11 more)

### Community 2 - "Community 2"
Cohesion: 0.07
Nodes (82): format_number(), purchase_addon(), Match, Node, print_results(), run(), walk_mem(), walk_mem_anchored_subtree_only() (+74 more)

### Community 3 - "Community 3"
Cohesion: 0.05
Nodes (45): AccountCmd, AddonsAction, BillingAction, Cli, Commands, live(), peak(), RequestCmd (+37 more)

### Community 4 - "Community 4"
Cohesion: 0.07
Nodes (35): render_otpauth(), renders_a_typical_totp_uri(), renders_empty_for_garbage_that_cannot_encode(), from_setup_response_parses_the_live_server_shape(), from_setup_response_rejects_a_missing_secret(), from_setup_response_rejects_a_non_string_backup_code(), from_setup_response_rejects_an_empty_backup_codes_list(), from_setup_response_rejects_an_empty_qr_uri() (+27 more)

### Community 5 - "Community 5"
Cohesion: 0.09
Nodes (43): print_created(), run(), run_recursive(), split_parent_and_leaf(), CachedDir, check_lock(), child_href(), DavState (+35 more)

### Community 6 - "Community 6"
Cohesion: 0.08
Nodes (33): decrypt_listing(), DecryptedFile, LsOpts, print_header(), print_json(), print_recursive(), print_row(), run() (+25 more)

### Community 7 - "Community 7"
Cohesion: 0.11
Nodes (37): decrypt_name(), collect_zip_entries(), looks_like_id_prefix(), pull_folder(), pull_folder_inner(), pull_single_file(), resolve_as_path(), resolve_request_key() (+29 more)

### Community 8 - "Community 8"
Cohesion: 0.11
Nodes (13): BeebeebFs, CachedDir, InodeEntry, PendingCreate, unmount(), AtomicFile, buffered_fallback(), DownloadStats (+5 more)

### Community 9 - "Community 9"
Cohesion: 0.09
Nodes (19): addons(), capitalise(), format_date_human(), format_price(), portal(), price_from_catalog(), price_from_subscription(), print_addons() (+11 more)

### Community 10 - "Community 10"
Cohesion: 0.14
Nodes (23): b64std(), b64url(), build_link(), create(), decode_any_b64(), generate_request_keypair(), keypair_wrap_unwrap_roundtrip_matches_create_then_list(), link_assembly_roundtrips_through_parse() (+15 more)

### Community 11 - "Community 11"
Cohesion: 0.16
Nodes (22): stop_all_sessions(), stop_session_by_name(), daemon_dir(), is_daemon_running(), kill_daemon(), list_daemon_slugs(), log_dir(), pid_path() (+14 more)

### Community 12 - "Community 12"
Cohesion: 0.14
Nodes (11): check_and_update(), cooldown_elapsed(), ct_eq_ignore_case(), current_target(), DistArtifact, DistChecksums, DistManifest, GitHubAsset (+3 more)

### Community 13 - "Community 13"
Cohesion: 0.22
Nodes (10): event_loop(), run(), format_size_binary(), render(), render_hints(), render_sessions(), is_ctrl_c(), poll_key() (+2 more)

### Community 14 - "Community 14"
Cohesion: 0.25
Nodes (6): FileEventStatus, SessionInfo, SyncFileEvent, SyncStatus, TuiState, TuiView

### Community 15 - "Community 15"
Cohesion: 0.52
Nodes (6): decrypt_payload_matches_webcrypto(), ecdh_shared_secret_matches_webcrypto(), full_flow_ecdh_to_plaintext_via_core(), hex32(), hex_decode(), hkdf_aes_key_matches_webcrypto()

## Knowledge Gaps
- **57 isolated node(s):** `ThumbnailResult`, `OutputMode`, `DeviceInfo`, `DownloadStats`, `PendingDb` (+52 more)
  These have ≤1 connection - possible missing edges or undocumented components.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `ApiClient` connect `Community 1` to `Community 0`, `Community 2`?**
  _High betweenness centrality (0.103) - this node is a cross-community bridge._
- **Why does `run()` connect `Community 2` to `Community 0`, `Community 3`, `Community 6`, `Community 7`, `Community 11`?**
  _High betweenness centrality (0.079) - this node is a cross-community bridge._
- **Why does `is_quiet()` connect `Community 0` to `Community 1`, `Community 2`, `Community 6`, `Community 7`, `Community 10`?**
  _High betweenness centrality (0.079) - this node is a cross-community bridge._
- **Are the 40 inferred relationships involving `is_json()` (e.g. with `create()` and `list()`) actually correct?**
  _`is_json()` has 40 INFERRED edges - model-reasoned connections that need verification._
- **Are the 37 inferred relationships involving `is_quiet()` (e.g. with `parse_response()` and `create()`) actually correct?**
  _`is_quiet()` has 37 INFERRED edges - model-reasoned connections that need verification._
- **Are the 15 inferred relationships involving `run()` (e.g. with `.from_config()` and `uninstall_launchagent()`) actually correct?**
  _`run()` has 15 INFERRED edges - model-reasoned connections that need verification._
- **What connects `ThumbnailResult`, `OutputMode`, `DeviceInfo` to the rest of the system?**
  _57 weakly-connected nodes found - possible documentation gaps or missing edges._
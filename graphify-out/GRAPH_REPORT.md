# Graph Report - cli-0478  (2026-09-22)

## Corpus Check
- 57 files · ~80,989 words
- Verdict: corpus is large enough that graph structure adds value.

## Summary
- 689 nodes · 1906 edges · 17 communities detected
- Extraction: 68% EXTRACTED · 32% INFERRED · 0% AMBIGUOUS · INFERRED: 610 edges (avg confidence: 0.8)
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
- [[_COMMUNITY_Community 16|Community 16]]

## God Nodes (most connected - your core abstractions)
1. `ApiClient` - 75 edges
2. `parse_response()` - 60 edges
3. `is_json()` - 39 edges
4. `run()` - 36 edges
5. `is_quiet()` - 35 edges
6. `main()` - 29 edges
7. `load_master_key()` - 28 edges
8. `load_master_key()` - 27 edges
9. `run()` - 21 edges
10. `BeebeebFs` - 21 edges

## Surprising Connections (you probably didn't know these)
- `set_api_url_override()` --calls--> `main()`  [INFERRED]
  src/config.rs → src/main.rs
- `expected_ciphertext_for()` --calls--> `run()`  [INFERRED]
  src/upload.rs → src/commands/sync.rs
- `generate_from_file()` --calls--> `upload_file_to()`  [INFERRED]
  src/thumbnail.rs → src/commands/sync.rs
- `generate_large_from_file()` --calls--> `upload_file_to()`  [INFERRED]
  src/thumbnail.rs → src/commands/sync.rs
- `is_rich()` --calls--> `run()`  [INFERRED]
  src/ui.rs → src/commands/rm.rs

## Communities

### Community 0 - "Community 0"
Cohesion: 0.07
Nodes (16): ApiClient, backoff(), build_client(), every_request_carries_client_and_version_headers(), format_request_error(), ids(), is_transient_transport_error(), last_page_null_cursor_terminates_the_walk() (+8 more)

### Community 1 - "Community 1"
Cohesion: 0.07
Nodes (80): capitalise(), format_number(), purchase_addon(), Match, Node, walk_mem(), walk_mem_anchored_subtree_only(), walk_mem_dfs_order_paths_and_walked() (+72 more)

### Community 2 - "Community 2"
Cohesion: 0.06
Nodes (66): decrypt_listing(), DecryptedFile, LsOpts, print_header(), print_json(), print_recursive(), print_row(), run() (+58 more)

### Community 3 - "Community 3"
Cohesion: 0.08
Nodes (48): print_created(), run(), run_recursive(), split_parent_and_leaf(), confirm(), count(), permanent_delete_flow(), run() (+40 more)

### Community 4 - "Community 4"
Cohesion: 0.06
Nodes (38): AccountCmd, AddonsAction, BillingAction, Cli, Commands, live(), peak(), RequestCmd (+30 more)

### Community 5 - "Community 5"
Cohesion: 0.06
Nodes (29): addons(), format_date_human(), format_price(), price_from_catalog(), price_from_subscription(), print_addons(), show(), SortField (+21 more)

### Community 6 - "Community 6"
Cohesion: 0.07
Nodes (34): portal(), run(), browser_login(), print_browser_block(), print_headless_block(), run(), spawn_countdown(), run() (+26 more)

### Community 7 - "Community 7"
Cohesion: 0.11
Nodes (13): BeebeebFs, CachedDir, InodeEntry, PendingCreate, unmount(), AtomicFile, buffered_fallback(), DownloadStats (+5 more)

### Community 8 - "Community 8"
Cohesion: 0.1
Nodes (25): b64(), check_quota(), collect_entries(), ConflictResolution, ConflictStrategy, dir_total_size(), find_conflict(), prompt_conflict() (+17 more)

### Community 9 - "Community 9"
Cohesion: 0.14
Nodes (21): b64std(), b64url(), build_link(), decode_any_b64(), generate_request_keypair(), keypair_wrap_unwrap_roundtrip_matches_create_then_list(), link_assembly_roundtrips_through_parse(), parse_expiry_secs() (+13 more)

### Community 10 - "Community 10"
Cohesion: 0.16
Nodes (24): spawn_sync_daemon(), stop_all_sessions(), stop_session_by_name(), daemon_dir(), install_launchagent(), is_daemon_running(), kill_daemon(), list_daemon_slugs() (+16 more)

### Community 11 - "Community 11"
Cohesion: 0.15
Nodes (13): build_show_payload(), build_show_payload_assembles_all_sections(), build_show_payload_degrades_gracefully_per_section(), map_email_change_error(), normalize_update_email(), opaque_email_change(), print_email_change_success(), render_progress_bar() (+5 more)

### Community 12 - "Community 12"
Cohesion: 0.14
Nodes (11): check_and_update(), cooldown_elapsed(), ct_eq_ignore_case(), current_target(), DistArtifact, DistChecksums, DistManifest, GitHubAsset (+3 more)

### Community 13 - "Community 13"
Cohesion: 0.27
Nodes (15): decrypt_file_chunks(), decrypt_name(), decrypt_name_plaintext_passthrough(), decrypt_name_with_key(), decrypt_names(), decrypt_names_batch_matches_single(), json_blob_legacy_format_detected_and_decrypted(), json_blob_with_binary_uuid_key() (+7 more)

### Community 14 - "Community 14"
Cohesion: 0.22
Nodes (10): event_loop(), run(), format_size_binary(), render(), render_hints(), render_sessions(), is_ctrl_c(), poll_key() (+2 more)

### Community 15 - "Community 15"
Cohesion: 0.25
Nodes (6): FileEventStatus, SessionInfo, SyncFileEvent, SyncStatus, TuiState, TuiView

### Community 16 - "Community 16"
Cohesion: 0.52
Nodes (6): decrypt_payload_matches_webcrypto(), ecdh_shared_secret_matches_webcrypto(), full_flow_ecdh_to_plaintext_via_core(), hex32(), hex_decode(), hkdf_aes_key_matches_webcrypto()

## Knowledge Gaps
- **57 isolated node(s):** `ThumbnailResult`, `OutputMode`, `DeviceInfo`, `DownloadStats`, `PendingDb` (+52 more)
  These have ≤1 connection - possible missing edges or undocumented components.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `ApiClient` connect `Community 0` to `Community 6`?**
  _High betweenness centrality (0.106) - this node is a cross-community bridge._
- **Why does `run()` connect `Community 1` to `Community 2`, `Community 4`, `Community 6`, `Community 8`, `Community 10`?**
  _High betweenness centrality (0.081) - this node is a cross-community bridge._
- **Why does `main()` connect `Community 5` to `Community 1`, `Community 2`, `Community 3`, `Community 4`, `Community 6`, `Community 7`, `Community 9`, `Community 11`, `Community 12`, `Community 14`?**
  _High betweenness centrality (0.073) - this node is a cross-community bridge._
- **Are the 38 inferred relationships involving `is_json()` (e.g. with `create()` and `list()`) actually correct?**
  _`is_json()` has 38 INFERRED edges - model-reasoned connections that need verification._
- **Are the 15 inferred relationships involving `run()` (e.g. with `.from_config()` and `uninstall_launchagent()`) actually correct?**
  _`run()` has 15 INFERRED edges - model-reasoned connections that need verification._
- **Are the 34 inferred relationships involving `is_quiet()` (e.g. with `parse_response()` and `create()`) actually correct?**
  _`is_quiet()` has 34 INFERRED edges - model-reasoned connections that need verification._
- **What connects `ThumbnailResult`, `OutputMode`, `DeviceInfo` to the rest of the system?**
  _57 weakly-connected nodes found - possible documentation gaps or missing edges._
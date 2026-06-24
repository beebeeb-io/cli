# Graph Report - cli  (2026-06-25)

## Corpus Check
- 58 files · ~78,867 words
- Verdict: corpus is large enough that graph structure adds value.

## Summary
- 659 nodes · 1846 edges · 16 communities detected
- Extraction: 67% EXTRACTED · 33% INFERRED · 0% AMBIGUOUS · INFERRED: 600 edges (avg confidence: 0.8)
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
1. `ApiClient` - 72 edges
2. `parse_response()` - 60 edges
3. `is_json()` - 37 edges
4. `run()` - 36 edges
5. `is_quiet()` - 34 edges
6. `load_master_key()` - 29 edges
7. `load_master_key()` - 28 edges
8. `main()` - 24 edges
9. `decrypt_name()` - 23 edges
10. `run()` - 21 edges

## Surprising Connections (you probably didn't know these)
- `set_api_url_override()` --calls--> `main()`  [INFERRED]
  src/config.rs → src/main.rs
- `expected_ciphertext_for()` --calls--> `run()`  [INFERRED]
  src/upload.rs → src/commands/sync.rs
- `mark_uploaded()` --calls--> `push_single_file()`  [INFERRED]
  src/loopback.rs → src/commands/push.rs
- `generate_from_file()` --calls--> `push_single_file()`  [INFERRED]
  src/thumbnail.rs → src/commands/push.rs
- `generate_large_from_file()` --calls--> `push_single_file()`  [INFERRED]
  src/thumbnail.rs → src/commands/push.rs

## Communities

### Community 0 - "Community 0"
Cohesion: 0.06
Nodes (75): decrypt_listing(), DecryptedFile, LsOpts, print_header(), print_json(), print_recursive(), print_row(), run() (+67 more)

### Community 1 - "Community 1"
Cohesion: 0.09
Nodes (8): ApiClient, backoff(), format_request_error(), is_transient_transport_error(), pace_if_needed(), parse_response(), update_rate_state(), UploadInit

### Community 2 - "Community 2"
Cohesion: 0.05
Nodes (45): upload_file_to(), AccountCmd, AddonsAction, BillingAction, Cli, Commands, live(), peak() (+37 more)

### Community 3 - "Community 3"
Cohesion: 0.1
Nodes (55): Match, Node, walk_mem(), walk_mem_anchored_subtree_only(), walk_mem_dfs_order_paths_and_walked(), walk_mem_stops_at_limit(), classify_new_local_is_not_a_delete(), classify_prior_synced_modified_is_readd() (+47 more)

### Community 4 - "Community 4"
Cohesion: 0.09
Nodes (44): print_created(), run(), run_recursive(), split_parent_and_leaf(), CachedDir, check_lock(), child_href(), DavState (+36 more)

### Community 5 - "Community 5"
Cohesion: 0.09
Nodes (39): render_otpauth(), renders_a_typical_totp_uri(), renders_empty_for_garbage_that_cannot_encode(), build_plan_label(), capitalise(), format_number(), run(), spawn_sync_daemon() (+31 more)

### Community 6 - "Community 6"
Cohesion: 0.07
Nodes (34): run(), browser_login(), print_browser_block(), print_headless_block(), run(), spawn_countdown(), run(), run() (+26 more)

### Community 7 - "Community 7"
Cohesion: 0.11
Nodes (14): BeebeebFs, CachedDir, InodeEntry, PendingCreate, run(), unmount(), AtomicFile, buffered_fallback() (+6 more)

### Community 8 - "Community 8"
Cohesion: 0.09
Nodes (24): addons(), capitalise(), format_date_human(), format_number(), portal(), price_for(), print_addons(), purchase_addon() (+16 more)

### Community 9 - "Community 9"
Cohesion: 0.12
Nodes (30): collect_all_files(), decrypt_chunks_with_binary_key(), detect_key_derivation(), encrypt_chunks_with_string_key(), encrypt_name_with_string_key(), KeyDerivation, repair_file(), repair_folder() (+22 more)

### Community 10 - "Community 10"
Cohesion: 0.15
Nodes (22): b64std(), b64url(), build_link(), create(), decode_any_b64(), generate_request_keypair(), keypair_wrap_unwrap_roundtrip_matches_create_then_list(), link_assembly_roundtrips_through_parse() (+14 more)

### Community 11 - "Community 11"
Cohesion: 0.13
Nodes (18): check_and_update(), cooldown_elapsed(), ct_eq_ignore_case(), current_target(), DistArtifact, DistChecksums, DistManifest, extract_binary_from_tarball() (+10 more)

### Community 12 - "Community 12"
Cohesion: 0.15
Nodes (13): build_show_payload(), build_show_payload_assembles_all_sections(), build_show_payload_degrades_gracefully_per_section(), map_email_change_error(), normalize_update_email(), opaque_email_change(), print_email_change_success(), render_progress_bar() (+5 more)

### Community 13 - "Community 13"
Cohesion: 0.24
Nodes (12): download_to_mirror(), extract_data(), handle_event(), load_master_key(), next_backoff(), run(), sanitize_filename(), unlink_in_mirror() (+4 more)

### Community 14 - "Community 14"
Cohesion: 0.25
Nodes (6): FileEventStatus, SessionInfo, SyncFileEvent, SyncStatus, TuiState, TuiView

### Community 15 - "Community 15"
Cohesion: 0.52
Nodes (6): decrypt_payload_matches_webcrypto(), ecdh_shared_secret_matches_webcrypto(), full_flow_ecdh_to_plaintext_via_core(), hex32(), hex_decode(), hkdf_aes_key_matches_webcrypto()

## Knowledge Gaps
- **56 isolated node(s):** `ThumbnailResult`, `OutputMode`, `DeviceInfo`, `DownloadStats`, `PendingDb` (+51 more)
  These have ≤1 connection - possible missing edges or undocumented components.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `ApiClient` connect `Community 1` to `Community 0`, `Community 2`?**
  _High betweenness centrality (0.106) - this node is a cross-community bridge._
- **Why does `run()` connect `Community 3` to `Community 0`, `Community 2`, `Community 5`, `Community 6`?**
  _High betweenness centrality (0.086) - this node is a cross-community bridge._
- **Why does `is_quiet()` connect `Community 0` to `Community 1`, `Community 3`, `Community 5`, `Community 6`, `Community 9`, `Community 10`, `Community 12`?**
  _High betweenness centrality (0.067) - this node is a cross-community bridge._
- **Are the 36 inferred relationships involving `is_json()` (e.g. with `create()` and `list()`) actually correct?**
  _`is_json()` has 36 INFERRED edges - model-reasoned connections that need verification._
- **Are the 15 inferred relationships involving `run()` (e.g. with `.from_config()` and `uninstall_launchagent()`) actually correct?**
  _`run()` has 15 INFERRED edges - model-reasoned connections that need verification._
- **Are the 33 inferred relationships involving `is_quiet()` (e.g. with `parse_response()` and `create()`) actually correct?**
  _`is_quiet()` has 33 INFERRED edges - model-reasoned connections that need verification._
- **What connects `ThumbnailResult`, `OutputMode`, `DeviceInfo` to the rest of the system?**
  _56 weakly-connected nodes found - possible documentation gaps or missing edges._
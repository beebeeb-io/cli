# Graph Report - cli  (2026-06-25)

## Corpus Check
- 58 files · ~79,816 words
- Verdict: corpus is large enough that graph structure adds value.

## Summary
- 665 nodes · 1862 edges · 16 communities detected
- Extraction: 68% EXTRACTED · 32% INFERRED · 0% AMBIGUOUS · INFERRED: 601 edges (avg confidence: 0.8)
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
1. `ApiClient` - 73 edges
2. `parse_response()` - 58 edges
3. `is_json()` - 37 edges
4. `run()` - 36 edges
5. `is_quiet()` - 34 edges
6. `load_master_key()` - 29 edges
7. `load_master_key()` - 28 edges
8. `main()` - 24 edges
9. `decrypt_name()` - 23 edges
10. `run()` - 21 edges

## Surprising Connections (you probably didn't know these)
- `expected_ciphertext_for()` --calls--> `run()`  [INFERRED]
  src/upload.rs → src/commands/sync.rs
- `mark_uploaded()` --calls--> `push_single_file()`  [INFERRED]
  src/loopback.rs → src/commands/push.rs
- `generate_from_file()` --calls--> `maybe_upload_thumbnail()`  [INFERRED]
  src/thumbnail.rs → src/upload.rs
- `generate_from_file()` --calls--> `upload_file_to()`  [INFERRED]
  src/thumbnail.rs → src/commands/sync.rs
- `generate_from_file()` --calls--> `push_single_file()`  [INFERRED]
  src/thumbnail.rs → src/commands/push.rs

## Communities

### Community 0 - "Community 0"
Cohesion: 0.06
Nodes (71): decrypt_listing(), DecryptedFile, print_header(), print_json(), print_recursive(), print_row(), run(), sort_listing() (+63 more)

### Community 1 - "Community 1"
Cohesion: 0.08
Nodes (13): ApiClient, backoff(), format_request_error(), ids(), is_transient_transport_error(), last_page_null_cursor_terminates_the_walk(), list_files_walks_every_page_past_the_200_cap(), list_trashed_walks_every_page() (+5 more)

### Community 2 - "Community 2"
Cohesion: 0.07
Nodes (73): LsOpts, classify_new_local_is_not_a_delete(), classify_prior_synced_modified_is_readd(), classify_prior_synced_unchanged_is_remote_delete(), classify_remote_absent(), compute_file_hash(), create_folder(), entry_for() (+65 more)

### Community 3 - "Community 3"
Cohesion: 0.05
Nodes (55): b64(), check_quota(), collect_entries(), ConflictResolution, ConflictStrategy, dir_total_size(), find_conflict(), prompt_conflict() (+47 more)

### Community 4 - "Community 4"
Cohesion: 0.07
Nodes (51): render_otpauth(), renders_a_typical_totp_uri(), renders_empty_for_garbage_that_cannot_encode(), build_plan_label(), capitalise(), format_number(), run(), Match (+43 more)

### Community 5 - "Community 5"
Cohesion: 0.09
Nodes (44): print_created(), run(), run_recursive(), split_parent_and_leaf(), CachedDir, check_lock(), child_href(), DavState (+36 more)

### Community 6 - "Community 6"
Cohesion: 0.1
Nodes (15): BeebeebFs, CachedDir, InodeEntry, PendingCreate, run(), unmount(), download_to(), AtomicFile (+7 more)

### Community 7 - "Community 7"
Cohesion: 0.1
Nodes (24): portal(), run(), browser_login(), print_browser_block(), print_headless_block(), run(), spawn_countdown(), run() (+16 more)

### Community 8 - "Community 8"
Cohesion: 0.15
Nodes (21): b64std(), b64url(), build_link(), create(), decode_any_b64(), generate_request_keypair(), keypair_wrap_unwrap_roundtrip_matches_create_then_list(), link_assembly_roundtrips_through_parse() (+13 more)

### Community 9 - "Community 9"
Cohesion: 0.13
Nodes (18): check_and_update(), cooldown_elapsed(), ct_eq_ignore_case(), current_target(), DistArtifact, DistChecksums, DistManifest, extract_binary_from_tarball() (+10 more)

### Community 10 - "Community 10"
Cohesion: 0.22
Nodes (19): decrypt_file_chunks(), decrypt_json_chunks(), decrypt_name(), decrypt_name_plaintext_passthrough(), decrypt_name_with_key(), decrypt_names(), decrypt_names_batch_matches_single(), decrypt_raw_chunks() (+11 more)

### Community 11 - "Community 11"
Cohesion: 0.15
Nodes (13): build_show_payload(), build_show_payload_assembles_all_sections(), build_show_payload_degrades_gracefully_per_section(), map_email_change_error(), normalize_update_email(), opaque_email_change(), print_email_change_success(), render_progress_bar() (+5 more)

### Community 12 - "Community 12"
Cohesion: 0.17
Nodes (13): run(), chrono_now(), ctrlc_channel(), relevant_paths(), run(), sync_batch(), trash_server_file(), event_loop() (+5 more)

### Community 13 - "Community 13"
Cohesion: 0.23
Nodes (8): addons(), capitalise(), format_date_human(), format_number(), price_for(), print_addons(), purchase_addon(), show()

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

- **Why does `ApiClient` connect `Community 1` to `Community 0`?**
  _High betweenness centrality (0.107) - this node is a cross-community bridge._
- **Why does `run()` connect `Community 2` to `Community 0`, `Community 3`, `Community 4`, `Community 7`?**
  _High betweenness centrality (0.085) - this node is a cross-community bridge._
- **Why does `is_quiet()` connect `Community 0` to `Community 1`, `Community 2`, `Community 3`, `Community 4`, `Community 8`, `Community 11`?**
  _High betweenness centrality (0.065) - this node is a cross-community bridge._
- **Are the 36 inferred relationships involving `is_json()` (e.g. with `create()` and `list()`) actually correct?**
  _`is_json()` has 36 INFERRED edges - model-reasoned connections that need verification._
- **Are the 15 inferred relationships involving `run()` (e.g. with `.from_config()` and `uninstall_launchagent()`) actually correct?**
  _`run()` has 15 INFERRED edges - model-reasoned connections that need verification._
- **Are the 33 inferred relationships involving `is_quiet()` (e.g. with `parse_response()` and `create()`) actually correct?**
  _`is_quiet()` has 33 INFERRED edges - model-reasoned connections that need verification._
- **What connects `ThumbnailResult`, `OutputMode`, `DeviceInfo` to the rest of the system?**
  _56 weakly-connected nodes found - possible documentation gaps or missing edges._
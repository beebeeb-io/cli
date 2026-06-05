# Graph Report - cli  (2026-06-05)

## Corpus Check
- 53 files · ~70,309 words
- Verdict: corpus is large enough that graph structure adds value.

## Summary
- 601 nodes · 1663 edges · 15 communities detected
- Extraction: 66% EXTRACTED · 34% INFERRED · 0% AMBIGUOUS · INFERRED: 561 edges (avg confidence: 0.8)
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

## God Nodes (most connected - your core abstractions)
1. `ApiClient` - 64 edges
2. `parse_response()` - 52 edges
3. `is_json()` - 35 edges
4. `run()` - 34 edges
5. `is_quiet()` - 32 edges
6. `load_master_key()` - 28 edges
7. `main()` - 24 edges
8. `decrypt_name()` - 24 edges
9. `load_master_key()` - 23 edges
10. `BeebeebFs` - 21 edges

## Surprising Connections (you probably didn't know these)
- `expected_ciphertext_for()` --calls--> `run()`  [INFERRED]
  src/upload.rs → src/commands/sync.rs
- `main()` --calls--> `set_api_url_override()`  [INFERRED]
  src/main.rs → src/config.rs
- `percent_decode()` --calls--> `resolve_path()`  [INFERRED]
  src/path.rs → src/commands/webdav.rs
- `resolve_path()` --calls--> `decrypt_name()`  [INFERRED]
  src/path.rs → src/commands/mount.rs
- `list_children_names()` --calls--> `decrypt_name()`  [INFERRED]
  src/path.rs → src/commands/mount.rs

## Communities

### Community 0 - "Community 0"
Cohesion: 0.07
Nodes (61): decrypt_listing(), DecryptedFile, print_header(), print_json(), print_recursive(), print_row(), run(), sort_listing() (+53 more)

### Community 1 - "Community 1"
Cohesion: 0.06
Nodes (55): render_otpauth(), renders_a_typical_totp_uri(), renders_empty_for_garbage_that_cannot_encode(), build_plan_label(), capitalise(), format_number(), run(), capitalise() (+47 more)

### Community 2 - "Community 2"
Cohesion: 0.1
Nodes (7): ApiClient, backoff(), format_request_error(), is_transient_transport_error(), pace_if_needed(), parse_response(), update_rate_state()

### Community 3 - "Community 3"
Cohesion: 0.05
Nodes (47): run(), browser_login(), print_browser_block(), print_headless_block(), run(), spawn_countdown(), run(), LsOpts (+39 more)

### Community 4 - "Community 4"
Cohesion: 0.08
Nodes (55): b64(), compute_file_hash(), create_folder(), do_download(), do_upload(), download_to(), FileEntry, format_size() (+47 more)

### Community 5 - "Community 5"
Cohesion: 0.06
Nodes (37): AccountCmd, AccountExportCmd, BillingAction, Cli, Commands, live(), peak(), RequestCmd (+29 more)

### Community 6 - "Community 6"
Cohesion: 0.11
Nodes (38): acquire_confirm_token(), confirm(), count(), permanent_delete_flow(), run(), Target, CachedDir, check_lock() (+30 more)

### Community 7 - "Community 7"
Cohesion: 0.11
Nodes (23): b64(), check_quota(), collect_entries(), ConflictResolution, ConflictStrategy, dir_total_size(), find_conflict(), prompt_conflict() (+15 more)

### Community 8 - "Community 8"
Cohesion: 0.15
Nodes (22): b64std(), b64url(), build_link(), create(), decode_any_b64(), generate_request_keypair(), keypair_wrap_unwrap_roundtrip_matches_create_then_list(), link_assembly_roundtrips_through_parse() (+14 more)

### Community 9 - "Community 9"
Cohesion: 0.12
Nodes (20): build_show_payload(), build_show_payload_assembles_all_sections(), build_show_payload_degrades_gracefully_per_section(), delete(), export_download(), export_start(), export_status(), render_progress_bar() (+12 more)

### Community 10 - "Community 10"
Cohesion: 0.18
Nodes (5): BeebeebFs, CachedDir, InodeEntry, PendingCreate, unmount()

### Community 11 - "Community 11"
Cohesion: 0.29
Nodes (14): decrypt_file_chunks(), decrypt_name(), decrypt_name_plaintext_passthrough(), decrypt_name_with_key(), json_blob_legacy_format_detected_and_decrypted(), json_blob_with_binary_uuid_key(), mk(), raw_binary_uuid_legacy_dual_key_fallback() (+6 more)

### Community 12 - "Community 12"
Cohesion: 0.23
Nodes (12): print_created(), run(), run_recursive(), split_parent_and_leaf(), CacheEntry, find_child_by_name(), hex_val(), list_children_names() (+4 more)

### Community 13 - "Community 13"
Cohesion: 0.31
Nodes (8): AtomicFile, buffered_fallback(), DownloadStats, drain_rest(), fill_to(), header_u64(), stream_download_decrypt(), stream_raw()

### Community 14 - "Community 14"
Cohesion: 0.25
Nodes (6): FileEventStatus, SessionInfo, SyncFileEvent, SyncStatus, TuiState, TuiView

## Knowledge Gaps
- **54 isolated node(s):** `CacheEntry`, `ResolvedPath`, `DownloadStats`, `UploadSpec`, `UploadOutcome` (+49 more)
  These have ≤1 connection - possible missing edges or undocumented components.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `ApiClient` connect `Community 2` to `Community 3`, `Community 5`?**
  _High betweenness centrality (0.103) - this node is a cross-community bridge._
- **Why does `run()` connect `Community 4` to `Community 0`, `Community 1`, `Community 3`, `Community 5`, `Community 7`?**
  _High betweenness centrality (0.093) - this node is a cross-community bridge._
- **Why does `main()` connect `Community 9` to `Community 0`, `Community 1`, `Community 3`, `Community 5`, `Community 8`, `Community 10`?**
  _High betweenness centrality (0.072) - this node is a cross-community bridge._
- **Are the 34 inferred relationships involving `is_json()` (e.g. with `run()` and `move_bulk()`) actually correct?**
  _`is_json()` has 34 INFERRED edges - model-reasoned connections that need verification._
- **Are the 15 inferred relationships involving `run()` (e.g. with `.from_config()` and `uninstall_launchagent()`) actually correct?**
  _`run()` has 15 INFERRED edges - model-reasoned connections that need verification._
- **Are the 31 inferred relationships involving `is_quiet()` (e.g. with `parse_response()` and `run()`) actually correct?**
  _`is_quiet()` has 31 INFERRED edges - model-reasoned connections that need verification._
- **What connects `CacheEntry`, `ResolvedPath`, `DownloadStats` to the rest of the system?**
  _54 weakly-connected nodes found - possible documentation gaps or missing edges._
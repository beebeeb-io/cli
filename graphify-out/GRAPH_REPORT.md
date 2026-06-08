# Graph Report - cli  (2026-06-08)

## Corpus Check
- 53 files · ~71,613 words
- Verdict: corpus is large enough that graph structure adds value.

## Summary
- 602 nodes · 1676 edges · 14 communities detected
- Extraction: 66% EXTRACTED · 34% INFERRED · 0% AMBIGUOUS · INFERRED: 576 edges (avg confidence: 0.8)
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

## God Nodes (most connected - your core abstractions)
1. `ApiClient` - 63 edges
2. `parse_response()` - 51 edges
3. `is_json()` - 35 edges
4. `run()` - 34 edges
5. `is_quiet()` - 32 edges
6. `load_master_key()` - 29 edges
7. `load_master_key()` - 28 edges
8. `main()` - 25 edges
9. `decrypt_name()` - 24 edges
10. `run()` - 21 edges

## Surprising Connections (you probably didn't know these)
- `expected_ciphertext_for()` --calls--> `run()`  [INFERRED]
  src/upload.rs → src/commands/sync.rs
- `mark_uploaded()` --calls--> `push_single_file()`  [INFERRED]
  src/loopback.rs → src/commands/push.rs
- `is_rich()` --calls--> `run()`  [INFERRED]
  src/ui.rs → src/commands/rm.rs
- `is_rich()` --calls--> `run()`  [INFERRED]
  src/ui.rs → src/commands/pull.rs
- `is_rich()` --calls--> `pull_single_file()`  [INFERRED]
  src/ui.rs → src/commands/pull.rs

## Communities

### Community 0 - "Community 0"
Cohesion: 0.07
Nodes (66): acquire_confirm_token(), decrypt_listing(), DecryptedFile, print_header(), print_json(), print_recursive(), print_row(), run() (+58 more)

### Community 1 - "Community 1"
Cohesion: 0.1
Nodes (8): ApiClient, backoff(), format_request_error(), is_transient_transport_error(), pace_if_needed(), parse_response(), update_rate_state(), UploadInit

### Community 2 - "Community 2"
Cohesion: 0.07
Nodes (49): LsOpts, render_otpauth(), renders_a_typical_totp_uri(), renders_empty_for_garbage_that_cannot_encode(), build_plan_label(), capitalise(), format_number(), run() (+41 more)

### Community 3 - "Community 3"
Cohesion: 0.08
Nodes (53): b64(), compute_file_hash(), create_folder(), do_upload(), FileEntry, format_size(), is_ignored_path(), LocalFile (+45 more)

### Community 4 - "Community 4"
Cohesion: 0.06
Nodes (37): AccountCmd, AccountExportCmd, BillingAction, Cli, Commands, live(), peak(), RequestCmd (+29 more)

### Community 5 - "Community 5"
Cohesion: 0.07
Nodes (37): run(), browser_login(), print_browser_block(), print_headless_block(), run(), spawn_countdown(), run(), run() (+29 more)

### Community 6 - "Community 6"
Cohesion: 0.09
Nodes (44): print_created(), run(), run_recursive(), split_parent_and_leaf(), CachedDir, check_lock(), child_href(), DavState (+36 more)

### Community 7 - "Community 7"
Cohesion: 0.08
Nodes (24): build_show_payload(), build_show_payload_assembles_all_sections(), build_show_payload_degrades_gracefully_per_section(), delete(), export_download(), export_start(), export_status(), render_progress_bar() (+16 more)

### Community 8 - "Community 8"
Cohesion: 0.13
Nodes (25): download_to(), decrypt_file_chunks(), decrypt_json_chunks(), decrypt_name(), decrypt_name_plaintext_passthrough(), decrypt_raw_chunks(), json_blob_legacy_format_detected_and_decrypted(), json_blob_with_binary_uuid_key() (+17 more)

### Community 9 - "Community 9"
Cohesion: 0.11
Nodes (23): b64(), check_quota(), collect_entries(), ConflictResolution, ConflictStrategy, dir_total_size(), find_conflict(), prompt_conflict() (+15 more)

### Community 10 - "Community 10"
Cohesion: 0.15
Nodes (22): b64std(), b64url(), build_link(), create(), decode_any_b64(), generate_request_keypair(), keypair_wrap_unwrap_roundtrip_matches_create_then_list(), link_assembly_roundtrips_through_parse() (+14 more)

### Community 11 - "Community 11"
Cohesion: 0.18
Nodes (5): BeebeebFs, CachedDir, InodeEntry, PendingCreate, unmount()

### Community 12 - "Community 12"
Cohesion: 0.24
Nodes (12): download_to_mirror(), extract_data(), handle_event(), load_master_key(), next_backoff(), run(), sanitize_filename(), unlink_in_mirror() (+4 more)

### Community 13 - "Community 13"
Cohesion: 0.25
Nodes (6): FileEventStatus, SessionInfo, SyncFileEvent, SyncStatus, TuiState, TuiView

## Knowledge Gaps
- **54 isolated node(s):** `ThumbnailResult`, `OutputMode`, `DeviceInfo`, `DownloadStats`, `PendingDb` (+49 more)
  These have ≤1 connection - possible missing edges or undocumented components.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `ApiClient` connect `Community 1` to `Community 4`, `Community 5`?**
  _High betweenness centrality (0.104) - this node is a cross-community bridge._
- **Why does `run()` connect `Community 3` to `Community 0`, `Community 2`, `Community 4`, `Community 5`, `Community 9`?**
  _High betweenness centrality (0.093) - this node is a cross-community bridge._
- **Why does `main()` connect `Community 7` to `Community 0`, `Community 2`, `Community 4`, `Community 5`, `Community 6`, `Community 10`, `Community 11`?**
  _High betweenness centrality (0.073) - this node is a cross-community bridge._
- **Are the 34 inferred relationships involving `is_json()` (e.g. with `create()` and `list()`) actually correct?**
  _`is_json()` has 34 INFERRED edges - model-reasoned connections that need verification._
- **Are the 15 inferred relationships involving `run()` (e.g. with `.from_config()` and `uninstall_launchagent()`) actually correct?**
  _`run()` has 15 INFERRED edges - model-reasoned connections that need verification._
- **Are the 31 inferred relationships involving `is_quiet()` (e.g. with `parse_response()` and `create()`) actually correct?**
  _`is_quiet()` has 31 INFERRED edges - model-reasoned connections that need verification._
- **What connects `ThumbnailResult`, `OutputMode`, `DeviceInfo` to the rest of the system?**
  _54 weakly-connected nodes found - possible documentation gaps or missing edges._
# Graph Report - cli  (2026-06-05)

## Corpus Check
- 52 files · ~68,080 words
- Verdict: corpus is large enough that graph structure adds value.

## Summary
- 578 nodes · 1582 edges · 16 communities detected
- Extraction: 67% EXTRACTED · 33% INFERRED · 0% AMBIGUOUS · INFERRED: 526 edges (avg confidence: 0.8)
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
1. `ApiClient` - 61 edges
2. `parse_response()` - 49 edges
3. `run()` - 33 edges
4. `is_json()` - 31 edges
5. `is_quiet()` - 27 edges
6. `load_master_key()` - 27 edges
7. `load_master_key()` - 23 edges
8. `main()` - 22 edges
9. `decrypt_name()` - 22 edges
10. `BeebeebFs` - 21 edges

## Surprising Connections (you probably didn't know these)
- `expected_ciphertext_for()` --calls--> `run()`  [INFERRED]
  src/upload.rs → src/commands/sync.rs
- `main()` --calls--> `set_api_url_override()`  [INFERRED]
  src/main.rs → src/config.rs
- `resolve_path()` --calls--> `decrypt_name()`  [INFERRED]
  src/path.rs → src/commands/mount.rs
- `list_children_names()` --calls--> `decrypt_name()`  [INFERRED]
  src/path.rs → src/commands/mount.rs
- `find_child_by_name()` --calls--> `decrypt_name()`  [INFERRED]
  src/path.rs → src/commands/mount.rs

## Communities

### Community 0 - "Community 0"
Cohesion: 0.06
Nodes (59): capitalise(), format_date_human(), format_number(), price_for(), show(), render_otpauth(), renders_a_typical_totp_uri(), renders_empty_for_garbage_that_cannot_encode() (+51 more)

### Community 1 - "Community 1"
Cohesion: 0.07
Nodes (64): run(), decrypt_name(), move_bulk(), move_single(), noop(), run(), split_dst(), collect_zip_entries() (+56 more)

### Community 2 - "Community 2"
Cohesion: 0.1
Nodes (7): ApiClient, backoff(), format_request_error(), is_transient_transport_error(), pace_if_needed(), parse_response(), update_rate_state()

### Community 3 - "Community 3"
Cohesion: 0.08
Nodes (57): b64(), compute_file_hash(), create_folder(), do_download(), do_upload(), download_to(), FileEntry, format_size() (+49 more)

### Community 4 - "Community 4"
Cohesion: 0.06
Nodes (37): AccountCmd, AccountExportCmd, BillingAction, Cli, Commands, live(), peak(), RequestCmd (+29 more)

### Community 5 - "Community 5"
Cohesion: 0.09
Nodes (44): print_created(), run(), run_recursive(), split_parent_and_leaf(), CachedDir, check_lock(), child_href(), DavState (+36 more)

### Community 6 - "Community 6"
Cohesion: 0.1
Nodes (24): run(), browser_login(), print_browser_block(), print_headless_block(), run(), spawn_countdown(), run(), run() (+16 more)

### Community 7 - "Community 7"
Cohesion: 0.15
Nodes (6): BeebeebFs, CachedDir, InodeEntry, PendingCreate, run(), unmount()

### Community 8 - "Community 8"
Cohesion: 0.13
Nodes (18): check_and_update(), cooldown_elapsed(), ct_eq_ignore_case(), current_target(), DistArtifact, DistChecksums, DistManifest, extract_binary_from_tarball() (+10 more)

### Community 9 - "Community 9"
Cohesion: 0.12
Nodes (16): build_show_payload(), build_show_payload_assembles_all_sections(), build_show_payload_degrades_gracefully_per_section(), delete(), export_download(), export_start(), export_status(), render_progress_bar() (+8 more)

### Community 10 - "Community 10"
Cohesion: 0.17
Nodes (13): run(), chrono_now(), ctrlc_channel(), relevant_paths(), run(), sync_batch(), trash_server_file(), event_loop() (+5 more)

### Community 11 - "Community 11"
Cohesion: 0.29
Nodes (14): decrypt_file_chunks(), decrypt_name(), decrypt_name_plaintext_passthrough(), decrypt_name_with_key(), json_blob_legacy_format_detected_and_decrypted(), json_blob_with_binary_uuid_key(), mk(), raw_binary_uuid_legacy_dual_key_fallback() (+6 more)

### Community 12 - "Community 12"
Cohesion: 0.31
Nodes (8): AtomicFile, buffered_fallback(), DownloadStats, drain_rest(), fill_to(), header_u64(), stream_download_decrypt(), stream_raw()

### Community 13 - "Community 13"
Cohesion: 0.24
Nodes (12): download_to_mirror(), extract_data(), handle_event(), load_master_key(), next_backoff(), run(), sanitize_filename(), unlink_in_mirror() (+4 more)

### Community 14 - "Community 14"
Cohesion: 0.25
Nodes (7): generate_from_file(), generate_large_from_file(), generates_thumbnail_from_synthetic_image(), thumbnail_output_is_webp(), thumbnail_respects_max_bytes(), ThumbnailResult, maybe_upload_thumbnail()

### Community 15 - "Community 15"
Cohesion: 0.25
Nodes (6): FileEventStatus, SessionInfo, SyncFileEvent, SyncStatus, TuiState, TuiView

## Knowledge Gaps
- **51 isolated node(s):** `CacheEntry`, `ResolvedPath`, `DownloadStats`, `UploadSpec`, `UploadOutcome` (+46 more)
  These have ≤1 connection - possible missing edges or undocumented components.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `ApiClient` connect `Community 2` to `Community 1`, `Community 4`?**
  _High betweenness centrality (0.103) - this node is a cross-community bridge._
- **Why does `run()` connect `Community 3` to `Community 0`, `Community 1`, `Community 4`, `Community 6`, `Community 7`?**
  _High betweenness centrality (0.091) - this node is a cross-community bridge._
- **Why does `main()` connect `Community 9` to `Community 0`, `Community 1`, `Community 4`, `Community 6`, `Community 7`, `Community 8`, `Community 10`?**
  _High betweenness centrality (0.069) - this node is a cross-community bridge._
- **Are the 14 inferred relationships involving `run()` (e.g. with `.from_config()` and `uninstall_launchagent()`) actually correct?**
  _`run()` has 14 INFERRED edges - model-reasoned connections that need verification._
- **Are the 30 inferred relationships involving `is_json()` (e.g. with `run()` and `move_bulk()`) actually correct?**
  _`is_json()` has 30 INFERRED edges - model-reasoned connections that need verification._
- **Are the 26 inferred relationships involving `is_quiet()` (e.g. with `parse_response()` and `run()`) actually correct?**
  _`is_quiet()` has 26 INFERRED edges - model-reasoned connections that need verification._
- **What connects `CacheEntry`, `ResolvedPath`, `DownloadStats` to the rest of the system?**
  _51 weakly-connected nodes found - possible documentation gaps or missing edges._
# Graph Report - cli  (2026-05-29)

## Corpus Check
- 45 files · ~58,275 words
- Verdict: corpus is large enough that graph structure adds value.

## Summary
- 489 nodes · 1189 edges · 18 communities detected
- Extraction: 74% EXTRACTED · 26% INFERRED · 0% AMBIGUOUS · INFERRED: 306 edges (avg confidence: 0.8)
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
- [[_COMMUNITY_Community 17|Community 17]]

## God Nodes (most connected - your core abstractions)
1. `ApiClient` - 52 edges
2. `parse_response()` - 41 edges
3. `run()` - 31 edges
4. `BeebeebFs` - 21 edges
5. `is_json()` - 20 edges
6. `load_master_key()` - 19 edges
7. `is_quiet()` - 17 edges
8. `main()` - 17 edges
9. `decrypt_name()` - 17 edges
10. `load_config()` - 16 edges

## Surprising Connections (you probably didn't know these)
- `set_api_url_override()` --calls--> `main()`  [INFERRED]
  src/config.rs → src/main.rs
- `expected_ciphertext_for()` --calls--> `run()`  [INFERRED]
  src/upload.rs → src/commands/sync.rs
- `mark_uploaded()` --calls--> `push_single_file()`  [INFERRED]
  src/loopback.rs → src/commands/push.rs
- `generate_from_file()` --calls--> `upload_file_to()`  [INFERRED]
  src/thumbnail.rs → src/commands/sync.rs
- `generate_from_file()` --calls--> `push_single_file()`  [INFERRED]
  src/thumbnail.rs → src/commands/push.rs

## Communities

### Community 0 - "Community 0"
Cohesion: 0.07
Nodes (51): run(), decrypt_name(), collect_zip_entries(), looks_like_id_prefix(), pull_folder(), pull_single_file(), resolve_as_path(), run() (+43 more)

### Community 1 - "Community 1"
Cohesion: 0.12
Nodes (4): ApiClient, pace_if_needed(), parse_response(), update_rate_state()

### Community 2 - "Community 2"
Cohesion: 0.11
Nodes (34): b64(), compute_file_hash(), create_folder(), do_download(), do_upload(), download_to(), FileEntry, format_size() (+26 more)

### Community 3 - "Community 3"
Cohesion: 0.1
Nodes (26): run(), browser_login(), print_browser_block(), print_headless_block(), run(), spawn_countdown(), run(), run() (+18 more)

### Community 4 - "Community 4"
Cohesion: 0.14
Nodes (32): CachedDir, check_lock(), child_href(), DavState, decode_file_entry(), decrypt_name(), delete_response(), get_from_cache() (+24 more)

### Community 5 - "Community 5"
Cohesion: 0.15
Nodes (26): spawn_sync_daemon(), stop_all_sessions(), stop_session_by_name(), daemon_dir(), install_launchagent(), install_systemd_unit(), is_daemon_running(), kill_daemon() (+18 more)

### Community 6 - "Community 6"
Cohesion: 0.14
Nodes (22): render_otpauth(), renders_a_typical_totp_uri(), renders_empty_for_garbage_that_cannot_encode(), build_plan_label(), capitalise(), format_number(), run(), capitalise() (+14 more)

### Community 7 - "Community 7"
Cohesion: 0.09
Nodes (17): generate_from_file(), generate_large_from_file(), generates_thumbnail_from_synthetic_image(), thumbnail_output_is_webp(), thumbnail_respects_max_bytes(), ThumbnailResult, BarProgress, ChunkProgress (+9 more)

### Community 8 - "Community 8"
Cohesion: 0.11
Nodes (23): build_show_payload(), build_show_payload_assembles_all_sections(), build_show_payload_degrades_gracefully_per_section(), delete(), export_download(), export_start(), export_status(), render_progress_bar() (+15 more)

### Community 9 - "Community 9"
Cohesion: 0.13
Nodes (18): check_and_update(), cooldown_elapsed(), ct_eq_ignore_case(), current_target(), DistArtifact, DistChecksums, DistManifest, extract_binary_from_tarball() (+10 more)

### Community 10 - "Community 10"
Cohesion: 0.18
Nodes (5): BeebeebFs, CachedDir, InodeEntry, PendingCreate, unmount()

### Community 11 - "Community 11"
Cohesion: 0.23
Nodes (16): decrypt_file_chunks(), decrypt_json_chunks(), decrypt_name(), decrypt_name_plaintext_passthrough(), decrypt_raw_chunks(), json_blob_legacy_format_detected_and_decrypted(), json_blob_with_binary_uuid_key(), mk() (+8 more)

### Community 12 - "Community 12"
Cohesion: 0.21
Nodes (10): EnvSnapshot, explicit_override_always_wins(), explicit_override_zero_is_not_a_yes(), is_headless(), is_headless_with(), snap(), ssh_tty_alone_counts_as_ssh(), ssh_with_x_forwarding_is_not_headless() (+2 more)

### Community 13 - "Community 13"
Cohesion: 0.31
Nodes (8): AtomicFile, buffered_fallback(), DownloadStats, drain_rest(), fill_to(), header_u64(), stream_download_decrypt(), stream_raw()

### Community 14 - "Community 14"
Cohesion: 0.24
Nodes (12): download_to_mirror(), extract_data(), handle_event(), load_master_key(), next_backoff(), run(), sanitize_filename(), unlink_in_mirror() (+4 more)

### Community 15 - "Community 15"
Cohesion: 0.22
Nodes (10): event_loop(), run(), format_size_binary(), render(), render_hints(), render_sessions(), is_ctrl_c(), poll_key() (+2 more)

### Community 16 - "Community 16"
Cohesion: 0.33
Nodes (7): CacheEntry, hex_val(), list_children_names(), list_files_cached(), percent_decode(), resolve_path(), ResolvedPath

### Community 17 - "Community 17"
Cohesion: 0.22
Nodes (6): FileEventStatus, SessionInfo, SyncFileEvent, SyncStatus, TuiState, TuiView

## Knowledge Gaps
- **45 isolated node(s):** `ThumbnailResult`, `OutputMode`, `DeviceInfo`, `DownloadStats`, `UploadSpec` (+40 more)
  These have ≤1 connection - possible missing edges or undocumented components.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `ApiClient` connect `Community 1` to `Community 3`?**
  _High betweenness centrality (0.114) - this node is a cross-community bridge._
- **Why does `run()` connect `Community 2` to `Community 0`, `Community 3`, `Community 5`, `Community 6`, `Community 7`?**
  _High betweenness centrality (0.111) - this node is a cross-community bridge._
- **Why does `main()` connect `Community 8` to `Community 0`, `Community 9`, `Community 10`, `Community 3`?**
  _High betweenness centrality (0.057) - this node is a cross-community bridge._
- **Are the 13 inferred relationships involving `run()` (e.g. with `.from_config()` and `uninstall_launchagent()`) actually correct?**
  _`run()` has 13 INFERRED edges - model-reasoned connections that need verification._
- **Are the 19 inferred relationships involving `is_json()` (e.g. with `run()` and `run()`) actually correct?**
  _`is_json()` has 19 INFERRED edges - model-reasoned connections that need verification._
- **What connects `ThumbnailResult`, `OutputMode`, `DeviceInfo` to the rest of the system?**
  _45 weakly-connected nodes found - possible documentation gaps or missing edges._
- **Should `Community 0` be split into smaller, more focused modules?**
  _Cohesion score 0.07 - nodes in this community are weakly interconnected._
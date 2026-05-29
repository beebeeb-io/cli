# Graph Report - fr-cli  (2026-05-29)

## Corpus Check
- 44 files · ~58,339 words
- Verdict: corpus is large enough that graph structure adds value.

## Summary
- 482 nodes · 1213 edges · 18 communities detected
- Extraction: 73% EXTRACTED · 27% INFERRED · 0% AMBIGUOUS · INFERRED: 328 edges (avg confidence: 0.8)
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
1. `ApiClient` - 56 edges
2. `parse_response()` - 46 edges
3. `run()` - 29 edges
4. `is_json()` - 23 edges
5. `load_master_key()` - 22 edges
6. `BeebeebFs` - 21 edges
7. `is_quiet()` - 20 edges
8. `main()` - 20 edges
9. `decrypt_name()` - 19 edges
10. `run()` - 17 edges

## Surprising Connections (you probably didn't know these)
- `set_api_url_override()` --calls--> `main()`  [INFERRED]
  src/config.rs → src/main.rs
- `mark_uploaded()` --calls--> `push_single_file()`  [INFERRED]
  src/loopback.rs → src/commands/push.rs
- `generate_from_file()` --calls--> `upload_file_to()`  [INFERRED]
  src/thumbnail.rs → src/commands/sync.rs
- `generate_from_file()` --calls--> `push_single_file()`  [INFERRED]
  src/thumbnail.rs → src/commands/push.rs
- `generate_large_from_file()` --calls--> `upload_file_to()`  [INFERRED]
  src/thumbnail.rs → src/commands/sync.rs

## Communities

### Community 0 - "Community 0"
Cohesion: 0.08
Nodes (60): b64(), compute_file_hash(), create_folder(), do_download(), do_upload(), download_to(), FileEntry, format_size() (+52 more)

### Community 1 - "Community 1"
Cohesion: 0.11
Nodes (4): ApiClient, pace_if_needed(), parse_response(), update_rate_state()

### Community 2 - "Community 2"
Cohesion: 0.07
Nodes (37): run(), build_plan_label(), capitalise(), format_number(), run(), collect_all_files(), decrypt_chunks_with_binary_key(), detect_key_derivation() (+29 more)

### Community 3 - "Community 3"
Cohesion: 0.11
Nodes (30): b64std(), b64url(), build_link(), create(), decode_any_b64(), generate_request_keypair(), keypair_wrap_unwrap_roundtrip_matches_create_then_list(), link_assembly_roundtrips_through_parse() (+22 more)

### Community 4 - "Community 4"
Cohesion: 0.1
Nodes (26): run(), browser_login(), print_browser_block(), print_headless_block(), run(), spawn_countdown(), run(), run() (+18 more)

### Community 5 - "Community 5"
Cohesion: 0.14
Nodes (32): CachedDir, check_lock(), child_href(), DavState, decode_file_entry(), decrypt_name(), delete_response(), get_from_cache() (+24 more)

### Community 6 - "Community 6"
Cohesion: 0.15
Nodes (27): decrypt_name(), collect_zip_entries(), looks_like_id_prefix(), pull_folder(), pull_folder_inner(), pull_single_file(), resolve_as_path(), resolve_request_key() (+19 more)

### Community 7 - "Community 7"
Cohesion: 0.1
Nodes (24): build_show_payload(), build_show_payload_assembles_all_sections(), build_show_payload_degrades_gracefully_per_section(), delete(), export_download(), export_start(), export_status(), render_progress_bar() (+16 more)

### Community 8 - "Community 8"
Cohesion: 0.15
Nodes (5): BeebeebFs, CachedDir, InodeEntry, PendingCreate, unmount()

### Community 9 - "Community 9"
Cohesion: 0.13
Nodes (18): check_and_update(), cooldown_elapsed(), ct_eq_ignore_case(), current_target(), DistArtifact, DistChecksums, DistManifest, extract_binary_from_tarball() (+10 more)

### Community 10 - "Community 10"
Cohesion: 0.21
Nodes (10): EnvSnapshot, explicit_override_always_wins(), explicit_override_zero_is_not_a_yes(), is_headless(), is_headless_with(), snap(), ssh_tty_alone_counts_as_ssh(), ssh_with_x_forwarding_is_not_headless() (+2 more)

### Community 11 - "Community 11"
Cohesion: 0.24
Nodes (12): download_to_mirror(), extract_data(), handle_event(), load_master_key(), next_backoff(), run(), sanitize_filename(), unlink_in_mirror() (+4 more)

### Community 12 - "Community 12"
Cohesion: 0.22
Nodes (10): event_loop(), run(), format_size_binary(), render(), render_hints(), render_sessions(), is_ctrl_c(), poll_key() (+2 more)

### Community 13 - "Community 13"
Cohesion: 0.27
Nodes (6): generate_from_file(), generate_large_from_file(), generates_thumbnail_from_synthetic_image(), thumbnail_output_is_webp(), thumbnail_respects_max_bytes(), ThumbnailResult

### Community 14 - "Community 14"
Cohesion: 0.33
Nodes (7): CacheEntry, hex_val(), list_children_names(), list_files_cached(), percent_decode(), resolve_path(), ResolvedPath

### Community 15 - "Community 15"
Cohesion: 0.22
Nodes (6): FileEventStatus, SessionInfo, SyncFileEvent, SyncStatus, TuiState, TuiView

### Community 16 - "Community 16"
Cohesion: 0.57
Nodes (6): format_size_binary(), render(), render_file_log(), render_hints(), render_separator(), render_status_bar()

### Community 17 - "Community 17"
Cohesion: 0.83
Nodes (3): render_otpauth(), renders_a_typical_totp_uri(), renders_empty_for_garbage_that_cannot_encode()

## Knowledge Gaps
- **42 isolated node(s):** `ThumbnailResult`, `OutputMode`, `DeviceInfo`, `GitHubRelease`, `GitHubAsset` (+37 more)
  These have ≤1 connection - possible missing edges or undocumented components.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `ApiClient` connect `Community 1` to `Community 4`?**
  _High betweenness centrality (0.118) - this node is a cross-community bridge._
- **Why does `run()` connect `Community 0` to `Community 8`, `Community 2`, `Community 4`, `Community 6`?**
  _High betweenness centrality (0.074) - this node is a cross-community bridge._
- **Why does `main()` connect `Community 7` to `Community 2`, `Community 3`, `Community 4`, `Community 8`, `Community 9`?**
  _High betweenness centrality (0.068) - this node is a cross-community bridge._
- **Are the 11 inferred relationships involving `run()` (e.g. with `.from_config()` and `uninstall_launchagent()`) actually correct?**
  _`run()` has 11 INFERRED edges - model-reasoned connections that need verification._
- **Are the 22 inferred relationships involving `is_json()` (e.g. with `create()` and `list()`) actually correct?**
  _`is_json()` has 22 INFERRED edges - model-reasoned connections that need verification._
- **Are the 17 inferred relationships involving `load_master_key()` (e.g. with `create()` and `list()`) actually correct?**
  _`load_master_key()` has 17 INFERRED edges - model-reasoned connections that need verification._
- **What connects `ThumbnailResult`, `OutputMode`, `DeviceInfo` to the rest of the system?**
  _42 weakly-connected nodes found - possible documentation gaps or missing edges._
# Graph Report - cli  (2026-06-05)

## Corpus Check
- 48 files · ~65,926 words
- Verdict: corpus is large enough that graph structure adds value.

## Summary
- 562 nodes · 1522 edges · 14 communities detected
- Extraction: 68% EXTRACTED · 32% INFERRED · 0% AMBIGUOUS · INFERRED: 489 edges (avg confidence: 0.8)
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
1. `ApiClient` - 59 edges
2. `parse_response()` - 47 edges
3. `run()` - 33 edges
4. `is_json()` - 25 edges
5. `load_master_key()` - 23 edges
6. `load_master_key()` - 23 edges
7. `main()` - 21 edges
8. `is_quiet()` - 21 edges
9. `BeebeebFs` - 21 edges
10. `run()` - 21 edges

## Surprising Connections (you probably didn't know these)
- `expected_ciphertext_for()` --calls--> `run()`  [INFERRED]
  src/upload.rs → src/commands/sync.rs
- `is_ctrl_c()` --calls--> `event_loop()`  [INFERRED]
  src/tui/events.rs → src/tui/app.rs
- `resolve_path()` --calls--> `decrypt_name()`  [INFERRED]
  src/path.rs → src/commands/mount.rs
- `list_children_names()` --calls--> `decrypt_name()`  [INFERRED]
  src/path.rs → src/commands/mount.rs
- `find_child_by_name()` --calls--> `decrypt_name()`  [INFERRED]
  src/path.rs → src/commands/mount.rs

## Communities

### Community 0 - "Community 0"
Cohesion: 0.07
Nodes (59): run(), decrypt_name(), collect_zip_entries(), looks_like_id_prefix(), pull_folder(), pull_folder_inner(), pull_single_file(), resolve_as_path() (+51 more)

### Community 1 - "Community 1"
Cohesion: 0.11
Nodes (7): ApiClient, backoff(), format_request_error(), is_transient_transport_error(), pace_if_needed(), parse_response(), update_rate_state()

### Community 2 - "Community 2"
Cohesion: 0.08
Nodes (56): b64(), compute_file_hash(), create_folder(), do_download(), do_upload(), download_to(), FileEntry, format_size() (+48 more)

### Community 3 - "Community 3"
Cohesion: 0.06
Nodes (36): AccountCmd, AccountExportCmd, BillingAction, Cli, Commands, live(), peak(), RequestCmd (+28 more)

### Community 4 - "Community 4"
Cohesion: 0.07
Nodes (35): run(), browser_login(), print_browser_block(), print_headless_block(), run(), spawn_countdown(), run(), run() (+27 more)

### Community 5 - "Community 5"
Cohesion: 0.09
Nodes (44): print_created(), run(), run_recursive(), split_parent_and_leaf(), CachedDir, check_lock(), child_href(), DavState (+36 more)

### Community 6 - "Community 6"
Cohesion: 0.11
Nodes (38): render_otpauth(), renders_a_typical_totp_uri(), renders_empty_for_garbage_that_cannot_encode(), build_plan_label(), capitalise(), format_number(), run(), b64std() (+30 more)

### Community 7 - "Community 7"
Cohesion: 0.11
Nodes (13): BeebeebFs, CachedDir, InodeEntry, PendingCreate, unmount(), AtomicFile, buffered_fallback(), DownloadStats (+5 more)

### Community 8 - "Community 8"
Cohesion: 0.13
Nodes (26): download_to_mirror(), extract_data(), handle_event(), load_master_key(), next_backoff(), run(), sanitize_filename(), unlink_in_mirror() (+18 more)

### Community 9 - "Community 9"
Cohesion: 0.13
Nodes (18): check_and_update(), cooldown_elapsed(), ct_eq_ignore_case(), current_target(), DistArtifact, DistChecksums, DistManifest, extract_binary_from_tarball() (+10 more)

### Community 10 - "Community 10"
Cohesion: 0.14
Nodes (19): build_show_payload(), build_show_payload_assembles_all_sections(), build_show_payload_degrades_gracefully_per_section(), delete(), export_download(), export_start(), export_status(), render_progress_bar() (+11 more)

### Community 11 - "Community 11"
Cohesion: 0.15
Nodes (13): b64url(), build_link(), decode_any_b64(), link_assembly_roundtrips_through_parse(), parse_expiry_secs(), parse_request_link(), parse_size_bytes(), ParsedLink (+5 more)

### Community 12 - "Community 12"
Cohesion: 0.14
Nodes (12): upload_file_to(), generate_from_file(), generate_large_from_file(), generates_thumbnail_from_synthetic_image(), thumbnail_output_is_webp(), thumbnail_respects_max_bytes(), ThumbnailResult, maybe_upload_thumbnail() (+4 more)

### Community 13 - "Community 13"
Cohesion: 0.25
Nodes (6): FileEventStatus, SessionInfo, SyncFileEvent, SyncStatus, TuiState, TuiView

## Knowledge Gaps
- **50 isolated node(s):** `CacheEntry`, `ResolvedPath`, `DownloadStats`, `UploadSpec`, `UploadOutcome` (+45 more)
  These have ≤1 connection - possible missing edges or undocumented components.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `ApiClient` connect `Community 1` to `Community 3`, `Community 4`?**
  _High betweenness centrality (0.103) - this node is a cross-community bridge._
- **Why does `run()` connect `Community 2` to `Community 0`, `Community 3`, `Community 4`, `Community 6`, `Community 12`?**
  _High betweenness centrality (0.092) - this node is a cross-community bridge._
- **Why does `main()` connect `Community 10` to `Community 0`, `Community 3`, `Community 4`, `Community 6`, `Community 7`, `Community 9`, `Community 11`?**
  _High betweenness centrality (0.067) - this node is a cross-community bridge._
- **Are the 14 inferred relationships involving `run()` (e.g. with `.from_config()` and `uninstall_launchagent()`) actually correct?**
  _`run()` has 14 INFERRED edges - model-reasoned connections that need verification._
- **Are the 24 inferred relationships involving `is_json()` (e.g. with `run()` and `create()`) actually correct?**
  _`is_json()` has 24 INFERRED edges - model-reasoned connections that need verification._
- **Are the 18 inferred relationships involving `load_master_key()` (e.g. with `load_config()` and `.new()`) actually correct?**
  _`load_master_key()` has 18 INFERRED edges - model-reasoned connections that need verification._
- **What connects `CacheEntry`, `ResolvedPath`, `DownloadStats` to the rest of the system?**
  _50 weakly-connected nodes found - possible documentation gaps or missing edges._
# Graph Report - cli  (2026-05-26)

## Corpus Check
- 37 files · ~50,230 words
- Verdict: corpus is large enough that graph structure adds value.

## Summary
- 404 nodes · 994 edges · 14 communities detected
- Extraction: 74% EXTRACTED · 26% INFERRED · 0% AMBIGUOUS · INFERRED: 262 edges (avg confidence: 0.8)
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
1. `ApiClient` - 50 edges
2. `parse_response()` - 40 edges
3. `run()` - 29 edges
4. `BeebeebFs` - 21 edges
5. `is_json()` - 20 edges
6. `load_master_key()` - 19 edges
7. `is_quiet()` - 17 edges
8. `main()` - 17 edges
9. `decrypt_name()` - 17 edges
10. `load_config()` - 16 edges

## Surprising Connections (you probably didn't know these)
- `mark_uploaded()` --calls--> `push_single_file()`  [INFERRED]
  src/loopback.rs → src/commands/push.rs
- `upload_file_to()` --calls--> `generate_from_file()`  [INFERRED]
  src/commands/sync.rs → src/thumbnail.rs
- `push_single_file()` --calls--> `generate_from_file()`  [INFERRED]
  src/commands/push.rs → src/thumbnail.rs
- `is_json()` --calls--> `run()`  [INFERRED]
  src/ui.rs → src/commands/quota.rs
- `is_json()` --calls--> `show()`  [INFERRED]
  src/ui.rs → src/commands/billing.rs

## Communities

### Community 0 - "Community 0"
Cohesion: 0.08
Nodes (44): run(), decrypt_name(), collect_zip_entries(), looks_like_id_prefix(), pull_folder(), pull_single_file(), resolve_as_path(), run() (+36 more)

### Community 1 - "Community 1"
Cohesion: 0.13
Nodes (4): ApiClient, pace_if_needed(), parse_response(), update_rate_state()

### Community 2 - "Community 2"
Cohesion: 0.11
Nodes (25): run(), browser_login(), print_browser_block(), print_headless_block(), run(), spawn_countdown(), run(), run() (+17 more)

### Community 3 - "Community 3"
Cohesion: 0.14
Nodes (32): CachedDir, check_lock(), child_href(), DavState, decode_file_entry(), decrypt_name(), delete_response(), get_from_cache() (+24 more)

### Community 4 - "Community 4"
Cohesion: 0.12
Nodes (21): render_otpauth(), renders_a_typical_totp_uri(), renders_empty_for_garbage_that_cannot_encode(), build_plan_label(), capitalise(), format_number(), run(), capitalise() (+13 more)

### Community 5 - "Community 5"
Cohesion: 0.1
Nodes (24): build_show_payload(), build_show_payload_assembles_all_sections(), build_show_payload_degrades_gracefully_per_section(), delete(), export_download(), export_start(), export_status(), render_progress_bar() (+16 more)

### Community 6 - "Community 6"
Cohesion: 0.15
Nodes (26): spawn_sync_daemon(), stop_all_sessions(), stop_session_by_name(), daemon_dir(), install_launchagent(), install_systemd_unit(), is_daemon_running(), kill_daemon() (+18 more)

### Community 7 - "Community 7"
Cohesion: 0.14
Nodes (24): b64(), compute_file_hash(), create_folder(), do_upload(), FileEntry, format_size(), is_ignored_path(), load_master_key() (+16 more)

### Community 8 - "Community 8"
Cohesion: 0.13
Nodes (18): check_and_update(), cooldown_elapsed(), ct_eq_ignore_case(), current_target(), DistArtifact, DistChecksums, DistManifest, extract_binary_from_tarball() (+10 more)

### Community 9 - "Community 9"
Cohesion: 0.16
Nodes (5): BeebeebFs, CachedDir, InodeEntry, PendingCreate, unmount()

### Community 10 - "Community 10"
Cohesion: 0.18
Nodes (17): collect_all_files(), decrypt_chunks_with_binary_key(), detect_key_derivation(), encrypt_chunks_with_string_key(), encrypt_name_with_string_key(), KeyDerivation, repair_file(), repair_folder() (+9 more)

### Community 11 - "Community 11"
Cohesion: 0.21
Nodes (10): EnvSnapshot, explicit_override_always_wins(), explicit_override_zero_is_not_a_yes(), is_headless(), is_headless_with(), snap(), ssh_tty_alone_counts_as_ssh(), ssh_with_x_forwarding_is_not_headless() (+2 more)

### Community 12 - "Community 12"
Cohesion: 0.24
Nodes (11): extract_data(), handle_event(), load_master_key(), next_backoff(), run(), sanitize_filename(), unlink_in_mirror(), wait_or_cancel() (+3 more)

### Community 13 - "Community 13"
Cohesion: 0.33
Nodes (7): CacheEntry, hex_val(), list_children_names(), list_files_cached(), percent_decode(), resolve_path(), ResolvedPath

## Knowledge Gaps
- **32 isolated node(s):** `ThumbnailResult`, `OutputMode`, `DeviceInfo`, `GitHubRelease`, `GitHubAsset` (+27 more)
  These have ≤1 connection - possible missing edges or undocumented components.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `ApiClient` connect `Community 1` to `Community 2`?**
  _High betweenness centrality (0.131) - this node is a cross-community bridge._
- **Why does `run()` connect `Community 7` to `Community 0`, `Community 2`, `Community 4`, `Community 6`, `Community 9`?**
  _High betweenness centrality (0.107) - this node is a cross-community bridge._
- **Why does `main()` connect `Community 5` to `Community 8`, `Community 0`, `Community 2`, `Community 9`?**
  _High betweenness centrality (0.071) - this node is a cross-community bridge._
- **Are the 10 inferred relationships involving `run()` (e.g. with `.from_config()` and `uninstall_launchagent()`) actually correct?**
  _`run()` has 10 INFERRED edges - model-reasoned connections that need verification._
- **Are the 19 inferred relationships involving `is_json()` (e.g. with `run()` and `run()`) actually correct?**
  _`is_json()` has 19 INFERRED edges - model-reasoned connections that need verification._
- **What connects `ThumbnailResult`, `OutputMode`, `DeviceInfo` to the rest of the system?**
  _32 weakly-connected nodes found - possible documentation gaps or missing edges._
- **Should `Community 0` be split into smaller, more focused modules?**
  _Cohesion score 0.08 - nodes in this community are weakly interconnected._
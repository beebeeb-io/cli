# Graph Report - cli  (2026-05-26)

## Corpus Check
- 34 files · ~46,419 words
- Verdict: corpus is large enough that graph structure adds value.

## Summary
- 361 nodes · 872 edges · 14 communities detected
- Extraction: 74% EXTRACTED · 26% INFERRED · 0% AMBIGUOUS · INFERRED: 231 edges (avg confidence: 0.8)
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
1. `ApiClient` - 44 edges
2. `parse_response()` - 34 edges
3. `run()` - 25 edges
4. `BeebeebFs` - 21 edges
5. `is_json()` - 20 edges
6. `load_master_key()` - 19 edges
7. `main()` - 17 edges
8. `decrypt_name()` - 17 edges
9. `is_quiet()` - 16 edges
10. `load_config()` - 16 edges

## Surprising Connections (you probably didn't know these)
- `set_api_url_override()` --calls--> `main()`  [INFERRED]
  src/config.rs → src/main.rs
- `mark_uploaded()` --calls--> `push_single_file()`  [INFERRED]
  src/loopback.rs → src/commands/push.rs
- `is_json()` --calls--> `show()`  [INFERRED]
  src/ui.rs → src/commands/billing.rs
- `is_json()` --calls--> `run()`  [INFERRED]
  src/ui.rs → src/commands/pull.rs
- `is_json()` --calls--> `run_zip()`  [INFERRED]
  src/ui.rs → src/commands/pull.rs

## Communities

### Community 0 - "Community 0"
Cohesion: 0.13
Nodes (4): ApiClient, pace_if_needed(), parse_response(), update_rate_state()

### Community 1 - "Community 1"
Cohesion: 0.1
Nodes (25): run(), run(), build_plan_label(), capitalise(), format_number(), run(), collect_all_files(), decrypt_chunks_with_binary_key() (+17 more)

### Community 2 - "Community 2"
Cohesion: 0.14
Nodes (32): CachedDir, check_lock(), child_href(), DavState, decode_file_entry(), decrypt_name(), delete_response(), get_from_cache() (+24 more)

### Community 3 - "Community 3"
Cohesion: 0.12
Nodes (24): browser_login(), print_browser_block(), print_headless_block(), run(), spawn_countdown(), run(), run(), draw_picker() (+16 more)

### Community 4 - "Community 4"
Cohesion: 0.12
Nodes (23): render_otpauth(), renders_a_typical_totp_uri(), renders_empty_for_garbage_that_cannot_encode(), capitalise(), run(), run_json(), build_plan_label(), capitalise() (+15 more)

### Community 5 - "Community 5"
Cohesion: 0.12
Nodes (26): b64(), compute_file_hash(), create_folder(), do_upload(), download_to(), FileEntry, format_size(), install_launchagent() (+18 more)

### Community 6 - "Community 6"
Cohesion: 0.17
Nodes (24): decrypt_name(), collect_zip_entries(), looks_like_id_prefix(), pull_folder(), pull_single_file(), resolve_as_path(), run(), run_zip() (+16 more)

### Community 7 - "Community 7"
Cohesion: 0.16
Nodes (5): BeebeebFs, CachedDir, InodeEntry, PendingCreate, unmount()

### Community 8 - "Community 8"
Cohesion: 0.13
Nodes (17): check_and_update(), cooldown_elapsed(), ct_eq_ignore_case(), current_target(), DistArtifact, DistChecksums, DistManifest, extract_binary_from_tarball() (+9 more)

### Community 9 - "Community 9"
Cohesion: 0.15
Nodes (16): chrono_now(), ctrlc_channel(), relevant_paths(), run(), sync_batch(), trash_server_file(), EnvSnapshot, explicit_override_always_wins() (+8 more)

### Community 10 - "Community 10"
Cohesion: 0.16
Nodes (18): build_show_payload(), build_show_payload_assembles_all_sections(), build_show_payload_degrades_gracefully_per_section(), delete(), export_download(), export_start(), export_status(), render_progress_bar() (+10 more)

### Community 11 - "Community 11"
Cohesion: 0.24
Nodes (12): download_to_mirror(), extract_data(), handle_event(), load_master_key(), next_backoff(), run(), sanitize_filename(), unlink_in_mirror() (+4 more)

### Community 12 - "Community 12"
Cohesion: 0.33
Nodes (7): CacheEntry, hex_val(), list_children_names(), list_files_cached(), percent_decode(), resolve_path(), ResolvedPath

### Community 13 - "Community 13"
Cohesion: 0.36
Nodes (5): capitalise(), format_date_human(), format_number(), price_for(), show()

## Knowledge Gaps
- **30 isolated node(s):** `OutputMode`, `GitHubRelease`, `GitHubAsset`, `DistManifest`, `DistArtifact` (+25 more)
  These have ≤1 connection - possible missing edges or undocumented components.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `ApiClient` connect `Community 0` to `Community 3`?**
  _High betweenness centrality (0.135) - this node is a cross-community bridge._
- **Why does `run()` connect `Community 5` to `Community 1`, `Community 3`, `Community 4`, `Community 7`?**
  _High betweenness centrality (0.085) - this node is a cross-community bridge._
- **Why does `main()` connect `Community 10` to `Community 1`, `Community 3`, `Community 4`, `Community 7`, `Community 8`, `Community 13`?**
  _High betweenness centrality (0.080) - this node is a cross-community bridge._
- **Are the 8 inferred relationships involving `run()` (e.g. with `.from_config()` and `.read()`) actually correct?**
  _`run()` has 8 INFERRED edges - model-reasoned connections that need verification._
- **Are the 19 inferred relationships involving `is_json()` (e.g. with `run()` and `run()`) actually correct?**
  _`is_json()` has 19 INFERRED edges - model-reasoned connections that need verification._
- **What connects `OutputMode`, `GitHubRelease`, `GitHubAsset` to the rest of the system?**
  _30 weakly-connected nodes found - possible documentation gaps or missing edges._
- **Should `Community 0` be split into smaller, more focused modules?**
  _Cohesion score 0.13 - nodes in this community are weakly interconnected._
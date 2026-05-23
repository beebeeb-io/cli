# Graph Report - cli  (2026-05-23)

## Corpus Check
- 28 files · ~40,435 words
- Verdict: corpus is large enough that graph structure adds value.

## Summary
- 302 nodes · 753 edges · 11 communities detected
- Extraction: 73% EXTRACTED · 27% INFERRED · 0% AMBIGUOUS · INFERRED: 207 edges (avg confidence: 0.8)
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

## God Nodes (most connected - your core abstractions)
1. `ApiClient` - 35 edges
2. `parse_response()` - 25 edges
3. `run()` - 24 edges
4. `BeebeebFs` - 21 edges
5. `is_json()` - 18 edges
6. `load_master_key()` - 18 edges
7. `decrypt_name()` - 17 edges
8. `load_config()` - 16 edges
9. `is_quiet()` - 15 edges
10. `handle_webdav()` - 15 edges

## Surprising Connections (you probably didn't know these)
- `mark_uploaded()` --calls--> `push_single_file()`  [INFERRED]
  loopback.rs → commands/push.rs
- `is_json()` --calls--> `run()`  [INFERRED]
  ui.rs → commands/ls.rs
- `is_json()` --calls--> `run()`  [INFERRED]
  ui.rs → commands/quota.rs
- `is_json()` --calls--> `run()`  [INFERRED]
  ui.rs → commands/config.rs
- `is_json()` --calls--> `run()`  [INFERRED]
  ui.rs → commands/sync.rs

## Communities

### Community 0 - "Community 0"
Cohesion: 0.11
Nodes (39): decrypt_name(), collect_zip_entries(), looks_like_id_prefix(), pull_folder(), pull_single_file(), resolve_as_path(), run(), run_zip() (+31 more)

### Community 1 - "Community 1"
Cohesion: 0.17
Nodes (2): ApiClient, parse_response()

### Community 2 - "Community 2"
Cohesion: 0.11
Nodes (30): build_plan_label(), capitalise(), format_number(), run(), capitalise(), run(), run_json(), download_to() (+22 more)

### Community 3 - "Community 3"
Cohesion: 0.11
Nodes (25): run(), browser_login(), print_browser_block(), print_headless_block(), run(), spawn_countdown(), run(), run() (+17 more)

### Community 4 - "Community 4"
Cohesion: 0.14
Nodes (32): CachedDir, check_lock(), child_href(), DavState, decode_file_entry(), decrypt_name(), delete_response(), get_from_cache() (+24 more)

### Community 5 - "Community 5"
Cohesion: 0.1
Nodes (20): run(), draw_picker(), parse_hours(), pick_share_interactively(), revoke(), run(), run_picker(), ShareEntry (+12 more)

### Community 6 - "Community 6"
Cohesion: 0.16
Nodes (22): b64(), compute_file_hash(), create_folder(), do_upload(), FileEntry, format_size(), is_ignored_path(), load_master_key() (+14 more)

### Community 7 - "Community 7"
Cohesion: 0.16
Nodes (5): BeebeebFs, CachedDir, InodeEntry, PendingCreate, unmount()

### Community 8 - "Community 8"
Cohesion: 0.21
Nodes (10): EnvSnapshot, explicit_override_always_wins(), explicit_override_zero_is_not_a_yes(), is_headless(), is_headless_with(), snap(), ssh_tty_alone_counts_as_ssh(), ssh_with_x_forwarding_is_not_headless() (+2 more)

### Community 9 - "Community 9"
Cohesion: 0.24
Nodes (12): download_to_mirror(), extract_data(), handle_event(), load_master_key(), next_backoff(), run(), sanitize_filename(), unlink_in_mirror() (+4 more)

### Community 10 - "Community 10"
Cohesion: 0.33
Nodes (7): CacheEntry, hex_val(), list_children_names(), list_files_cached(), percent_decode(), resolve_path(), ResolvedPath

## Knowledge Gaps
- **24 isolated node(s):** `OutputMode`, `GitHubRelease`, `GitHubAsset`, `CacheEntry`, `ResolvedPath` (+19 more)
  These have ≤1 connection - possible missing edges or undocumented components.
- **Thin community `Community 1`** (37 nodes): `api.rs`, `ApiClient`, `.check_conflict()`, `.create_folder()`, `.create_share()`, `.create_stream_token()`, `.delete_share()`, `.download_file()`, `.find_file_by_id_prefix()`, `.get_file()`, `.get_file_count()`, `.get_json()`, `.get_me()`, `.get_my_region()`, `.get_region()`, `.get_sessions()`, `.get_subscription()`, `.get_usage()`, `.list_files()`, `.list_ops_since()`, `.list_shares()`, `.logout()`, `.move_file()`, `.opaque_login_finish()`, `.opaque_login_start()`, `.open_sync_stream()`, `.ping_health()`, `.require_auth()`, `.signup()`, `.speedtest_download()`, `.speedtest_upload()`, `.stream_url()`, `.trash_file()`, `.upload_encrypted()`, `.url()`, `format_request_error()`, `parse_response()`
  Too small to be a meaningful cluster - may be noise or needs more connections extracted.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `ApiClient` connect `Community 1` to `Community 3`?**
  _High betweenness centrality (0.159) - this node is a cross-community bridge._
- **Why does `run()` connect `Community 6` to `Community 0`, `Community 2`, `Community 3`, `Community 5`, `Community 7`?**
  _High betweenness centrality (0.101) - this node is a cross-community bridge._
- **Why does `decrypt_name()` connect `Community 0` to `Community 4`, `Community 5`, `Community 6`, `Community 7`, `Community 9`, `Community 10`?**
  _High betweenness centrality (0.072) - this node is a cross-community bridge._
- **Are the 8 inferred relationships involving `run()` (e.g. with `.from_config()` and `.read()`) actually correct?**
  _`run()` has 8 INFERRED edges - model-reasoned connections that need verification._
- **Are the 17 inferred relationships involving `is_json()` (e.g. with `run()` and `run()`) actually correct?**
  _`is_json()` has 17 INFERRED edges - model-reasoned connections that need verification._
- **What connects `OutputMode`, `GitHubRelease`, `GitHubAsset` to the rest of the system?**
  _24 weakly-connected nodes found - possible documentation gaps or missing edges._
- **Should `Community 0` be split into smaller, more focused modules?**
  _Cohesion score 0.11 - nodes in this community are weakly interconnected._
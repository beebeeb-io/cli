# Graph Report - cli  (2026-05-23)

## Corpus Check
- 29 files · ~41,417 words
- Verdict: corpus is large enough that graph structure adds value.

## Summary
- 313 nodes · 773 edges · 11 communities detected
- Extraction: 73% EXTRACTED · 27% INFERRED · 0% AMBIGUOUS · INFERRED: 212 edges (avg confidence: 0.8)
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
1. `ApiClient` - 37 edges
2. `parse_response()` - 25 edges
3. `run()` - 24 edges
4. `BeebeebFs` - 21 edges
5. `is_json()` - 19 edges
6. `load_master_key()` - 18 edges
7. `decrypt_name()` - 17 edges
8. `load_config()` - 16 edges
9. `is_quiet()` - 15 edges
10. `handle_webdav()` - 15 edges

## Surprising Connections (you probably didn't know these)
- `mark_uploaded()` --calls--> `push_single_file()`  [INFERRED]
  loopback.rs → commands/push.rs
- `show()` --calls--> `is_json()`  [INFERRED]
  commands/billing.rs → ui.rs
- `is_json()` --calls--> `run()`  [INFERRED]
  ui.rs → commands/pull.rs
- `is_json()` --calls--> `run_zip()`  [INFERRED]
  ui.rs → commands/pull.rs
- `is_json()` --calls--> `run()`  [INFERRED]
  ui.rs → commands/sync.rs

## Communities

### Community 0 - "Community 0"
Cohesion: 0.08
Nodes (34): run(), run(), build_plan_label(), capitalise(), format_number(), run(), collect_all_files(), decrypt_chunks_with_binary_key() (+26 more)

### Community 1 - "Community 1"
Cohesion: 0.11
Nodes (34): decrypt_name(), collect_zip_entries(), looks_like_id_prefix(), pull_folder(), pull_single_file(), resolve_as_path(), run(), run_zip() (+26 more)

### Community 2 - "Community 2"
Cohesion: 0.16
Nodes (2): ApiClient, parse_response()

### Community 3 - "Community 3"
Cohesion: 0.14
Nodes (32): CachedDir, check_lock(), child_href(), DavState, decode_file_entry(), decrypt_name(), delete_response(), get_from_cache() (+24 more)

### Community 4 - "Community 4"
Cohesion: 0.12
Nodes (24): browser_login(), print_browser_block(), print_headless_block(), run(), spawn_countdown(), run(), run(), run() (+16 more)

### Community 5 - "Community 5"
Cohesion: 0.15
Nodes (23): capitalise(), run(), run_json(), decrypt_json_chunks(), decrypt_name(), decrypt_raw_chunks(), try_decrypt_all_chunks(), unwrap_metadata_json() (+15 more)

### Community 6 - "Community 6"
Cohesion: 0.15
Nodes (23): b64(), compute_file_hash(), create_folder(), do_upload(), FileEntry, format_size(), install_launchagent(), is_ignored_path() (+15 more)

### Community 7 - "Community 7"
Cohesion: 0.16
Nodes (5): BeebeebFs, CachedDir, InodeEntry, PendingCreate, unmount()

### Community 8 - "Community 8"
Cohesion: 0.17
Nodes (12): capitalise(), format_date_human(), format_number(), price_for(), show(), set_api_url_override(), BillingAction, Cli (+4 more)

### Community 9 - "Community 9"
Cohesion: 0.21
Nodes (10): EnvSnapshot, explicit_override_always_wins(), explicit_override_zero_is_not_a_yes(), is_headless(), is_headless_with(), snap(), ssh_tty_alone_counts_as_ssh(), ssh_with_x_forwarding_is_not_headless() (+2 more)

### Community 10 - "Community 10"
Cohesion: 0.24
Nodes (11): extract_data(), handle_event(), load_master_key(), next_backoff(), run(), sanitize_filename(), unlink_in_mirror(), wait_or_cancel() (+3 more)

## Knowledge Gaps
- **25 isolated node(s):** `OutputMode`, `GitHubRelease`, `GitHubAsset`, `CacheEntry`, `ResolvedPath` (+20 more)
  These have ≤1 connection - possible missing edges or undocumented components.
- **Thin community `Community 2`** (39 nodes): `api.rs`, `ApiClient`, `.check_conflict()`, `.create_folder()`, `.create_share()`, `.create_stream_token()`, `.delete_share()`, `.download_file()`, `.find_file_by_id_prefix()`, `.get_billing_subscription()`, `.get_billing_usage()`, `.get_file()`, `.get_file_count()`, `.get_json()`, `.get_me()`, `.get_my_region()`, `.get_region()`, `.get_sessions()`, `.get_subscription()`, `.get_usage()`, `.list_files()`, `.list_ops_since()`, `.list_shares()`, `.logout()`, `.move_file()`, `.opaque_login_finish()`, `.opaque_login_start()`, `.open_sync_stream()`, `.ping_health()`, `.require_auth()`, `.signup()`, `.speedtest_download()`, `.speedtest_upload()`, `.stream_url()`, `.trash_file()`, `.upload_encrypted()`, `.url()`, `format_request_error()`, `parse_response()`
  Too small to be a meaningful cluster - may be noise or needs more connections extracted.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `ApiClient` connect `Community 2` to `Community 4`?**
  _High betweenness centrality (0.166) - this node is a cross-community bridge._
- **Why does `run()` connect `Community 6` to `Community 0`, `Community 1`, `Community 4`, `Community 5`, `Community 7`?**
  _High betweenness centrality (0.098) - this node is a cross-community bridge._
- **Why does `decrypt_name()` connect `Community 1` to `Community 0`, `Community 3`, `Community 6`, `Community 7`, `Community 10`?**
  _High betweenness centrality (0.069) - this node is a cross-community bridge._
- **Are the 8 inferred relationships involving `run()` (e.g. with `.from_config()` and `.read()`) actually correct?**
  _`run()` has 8 INFERRED edges - model-reasoned connections that need verification._
- **Are the 18 inferred relationships involving `is_json()` (e.g. with `run()` and `run()`) actually correct?**
  _`is_json()` has 18 INFERRED edges - model-reasoned connections that need verification._
- **What connects `OutputMode`, `GitHubRelease`, `GitHubAsset` to the rest of the system?**
  _25 weakly-connected nodes found - possible documentation gaps or missing edges._
- **Should `Community 0` be split into smaller, more focused modules?**
  _Cohesion score 0.08 - nodes in this community are weakly interconnected._
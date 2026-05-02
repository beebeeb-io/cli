# Graph Report - cli  (2026-05-02)

## Corpus Check
- 15 files · ~9,882 words
- Verdict: corpus is large enough that graph structure adds value.

## Summary
- 103 nodes · 208 edges · 7 communities detected
- Extraction: 87% EXTRACTED · 13% INFERRED · 0% AMBIGUOUS · INFERRED: 27 edges (avg confidence: 0.8)
- Token cost: 0 input · 0 output

## Community Hubs (Navigation)
- [[_COMMUNITY_Community 0|Community 0]]
- [[_COMMUNITY_Community 1|Community 1]]
- [[_COMMUNITY_Community 2|Community 2]]
- [[_COMMUNITY_Community 3|Community 3]]
- [[_COMMUNITY_Community 4|Community 4]]
- [[_COMMUNITY_Community 5|Community 5]]
- [[_COMMUNITY_Community 6|Community 6]]

## God Nodes (most connected - your core abstractions)
1. `ApiClient` - 22 edges
2. `parse_response()` - 18 edges
3. `run()` - 12 edges
4. `load_config()` - 11 edges
5. `load_master_key()` - 6 edges
6. `run()` - 6 edges
7. `main()` - 5 edges
8. `run()` - 5 edges
9. `load_master_key()` - 5 edges
10. `push_single_file()` - 5 edges

## Surprising Connections (you probably didn't know these)
- `load_config()` --calls--> `run()`  [INFERRED]
  config.rs → commands/config.rs
- `load_config()` --calls--> `load_master_key()`  [INFERRED]
  config.rs → commands/pull.rs
- `load_master_key()` --calls--> `load_config()`  [INFERRED]
  commands/sync.rs → config.rs
- `load_config()` --calls--> `load_master_key()`  [INFERRED]
  config.rs → commands/push.rs
- `load_config()` --calls--> `run()`  [INFERRED]
  config.rs → commands/status.rs

## Communities

### Community 0 - "Community 0"
Cohesion: 0.28
Nodes (2): ApiClient, parse_response()

### Community 1 - "Community 1"
Cohesion: 0.17
Nodes (19): b64(), create_folder(), do_download(), do_upload(), download_to(), FileEntry, format_size(), load_master_key() (+11 more)

### Community 2 - "Community 2"
Cohesion: 0.15
Nodes (11): run(), list(), parse_hours(), revoke(), run(), get_session_expiry(), run(), run() (+3 more)

### Community 3 - "Community 3"
Cohesion: 0.19
Nodes (12): run(), b64(), legacy_login(), LoginResult, opaque_login(), run(), run(), clear_config() (+4 more)

### Community 4 - "Community 4"
Cohesion: 0.44
Nodes (7): b64(), collect_entries(), format_size(), load_master_key(), push_directory(), push_single_file(), run()

### Community 5 - "Community 5"
Cohesion: 0.62
Nodes (6): b64(), format_size(), load_master_key(), pull_folder(), pull_single_file(), run()

### Community 6 - "Community 6"
Cohesion: 0.6
Nodes (5): chrono_now(), ctrlc_channel(), relevant_paths(), run(), sync_batch()

## Knowledge Gaps
- **7 isolated node(s):** `Cli`, `Commands`, `SyncState`, `FileEntry`, `LocalFile` (+2 more)
  These have ≤1 connection - possible missing edges or undocumented components.
- **Thin community `Community 0`** (23 nodes): `api.rs`, `ApiClient`, `.create_folder()`, `.create_share()`, `.delete_share()`, `.download_file()`, `.get_file()`, `.get_me()`, `.get_region()`, `.get_sessions()`, `.get_usage()`, `.list_files()`, `.list_shares()`, `.login()`, `.logout()`, `.opaque_login_finish()`, `.opaque_login_start()`, `.require_auth()`, `.signup()`, `.trash_file()`, `.upload_encrypted()`, `.url()`, `parse_response()`
  Too small to be a meaningful cluster - may be noise or needs more connections extracted.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `ApiClient` connect `Community 0` to `Community 2`?**
  _High betweenness centrality (0.350) - this node is a cross-community bridge._
- **Why does `run()` connect `Community 1` to `Community 2`, `Community 3`?**
  _High betweenness centrality (0.263) - this node is a cross-community bridge._
- **Why does `load_config()` connect `Community 3` to `Community 1`, `Community 2`, `Community 4`, `Community 5`?**
  _High betweenness centrality (0.208) - this node is a cross-community bridge._
- **Are the 2 inferred relationships involving `run()` (e.g. with `.from_config()` and `.default()`) actually correct?**
  _`run()` has 2 INFERRED edges - model-reasoned connections that need verification._
- **Are the 8 inferred relationships involving `load_config()` (e.g. with `.from_config()` and `run()`) actually correct?**
  _`load_config()` has 8 INFERRED edges - model-reasoned connections that need verification._
- **What connects `Cli`, `Commands`, `SyncState` to the rest of the system?**
  _7 weakly-connected nodes found - possible documentation gaps or missing edges._
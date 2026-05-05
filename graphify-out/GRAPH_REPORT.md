# Graph Report - cli  (2026-05-05)

## Corpus Check
- 17 files · ~19,435 words
- Verdict: corpus is large enough that graph structure adds value.

## Summary
- 175 nodes · 402 edges · 9 communities detected
- Extraction: 83% EXTRACTED · 17% INFERRED · 0% AMBIGUOUS · INFERRED: 68 edges (avg confidence: 0.8)
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

## God Nodes (most connected - your core abstractions)
1. `ApiClient` - 23 edges
2. `BeebeebFs` - 21 edges
3. `parse_response()` - 19 edges
4. `run()` - 15 edges
5. `load_config()` - 14 edges
6. `handle_webdav()` - 13 edges
7. `propfind_response()` - 10 edges
8. `resolve_path()` - 9 edges
9. `put_response()` - 8 edges
10. `run()` - 8 edges

## Surprising Connections (you probably didn't know these)
- `load_config()` --calls--> `run()`  [INFERRED]
  config.rs → commands/config.rs
- `load_config()` --calls--> `load_master_key()`  [INFERRED]
  config.rs → commands/pull.rs
- `load_config()` --calls--> `load_master_key()`  [INFERRED]
  config.rs → commands/sync.rs
- `load_config()` --calls--> `load_master_key()`  [INFERRED]
  config.rs → commands/push.rs
- `run()` --calls--> `load_config()`  [INFERRED]
  commands/webdav.rs → config.rs

## Communities

### Community 0 - "Community 0"
Cohesion: 0.14
Nodes (33): CachedDir, check_lock(), child_href(), DavState, decode_file_entry(), decrypt_chunks(), decrypt_name(), delete_response() (+25 more)

### Community 1 - "Community 1"
Cohesion: 0.13
Nodes (21): run(), b64(), browser_login(), BrowserState, CallbackPayload, handle_callback(), legacy_login(), LoginResult (+13 more)

### Community 2 - "Community 2"
Cohesion: 0.15
Nodes (6): BeebeebFs, CachedDir, decrypt_chunks(), decrypt_name(), InodeEntry, PendingCreate

### Community 3 - "Community 3"
Cohesion: 0.27
Nodes (2): ApiClient, parse_response()

### Community 4 - "Community 4"
Cohesion: 0.16
Nodes (19): b64(), create_folder(), do_download(), do_upload(), download_to(), FileEntry, format_size(), load_master_key() (+11 more)

### Community 5 - "Community 5"
Cohesion: 0.15
Nodes (10): run(), unmount(), list(), parse_hours(), revoke(), run(), run(), Cli (+2 more)

### Community 6 - "Community 6"
Cohesion: 0.44
Nodes (7): b64(), collect_entries(), format_size(), load_master_key(), push_directory(), push_single_file(), run()

### Community 7 - "Community 7"
Cohesion: 0.54
Nodes (6): b64(), format_size(), load_master_key(), pull_folder(), pull_single_file(), run()

### Community 8 - "Community 8"
Cohesion: 0.6
Nodes (5): chrono_now(), ctrlc_channel(), relevant_paths(), run(), sync_batch()

## Knowledge Gaps
- **15 isolated node(s):** `Cli`, `Commands`, `SyncState`, `FileEntry`, `LocalFile` (+10 more)
  These have ≤1 connection - possible missing edges or undocumented components.
- **Thin community `Community 3`** (24 nodes): `api.rs`, `ApiClient`, `.create_folder()`, `.create_share()`, `.delete_share()`, `.download_file()`, `.get_file()`, `.get_me()`, `.get_region()`, `.get_sessions()`, `.get_usage()`, `.list_files()`, `.list_shares()`, `.login()`, `.logout()`, `.move_file()`, `.opaque_login_finish()`, `.opaque_login_start()`, `.require_auth()`, `.signup()`, `.trash_file()`, `.upload_encrypted()`, `.url()`, `parse_response()`
  Too small to be a meaningful cluster - may be noise or needs more connections extracted.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `ApiClient` connect `Community 3` to `Community 1`?**
  _High betweenness centrality (0.165) - this node is a cross-community bridge._
- **Why does `BeebeebFs` connect `Community 2` to `Community 1`, `Community 4`, `Community 7`?**
  _High betweenness centrality (0.130) - this node is a cross-community bridge._
- **Why does `run()` connect `Community 4` to `Community 1`, `Community 7`?**
  _High betweenness centrality (0.115) - this node is a cross-community bridge._
- **Are the 5 inferred relationships involving `run()` (e.g. with `.from_config()` and `.read()`) actually correct?**
  _`run()` has 5 INFERRED edges - model-reasoned connections that need verification._
- **Are the 11 inferred relationships involving `load_config()` (e.g. with `.from_config()` and `run()`) actually correct?**
  _`load_config()` has 11 INFERRED edges - model-reasoned connections that need verification._
- **What connects `Cli`, `Commands`, `SyncState` to the rest of the system?**
  _15 weakly-connected nodes found - possible documentation gaps or missing edges._
- **Should `Community 0` be split into smaller, more focused modules?**
  _Cohesion score 0.14 - nodes in this community are weakly interconnected._
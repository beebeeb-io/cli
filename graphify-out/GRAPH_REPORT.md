# Graph Report - cli  (2026-05-07)

## Corpus Check
- 21 files · ~24,154 words
- Verdict: corpus is large enough that graph structure adds value.

## Summary
- 210 nodes · 482 edges · 8 communities detected
- Extraction: 82% EXTRACTED · 18% INFERRED · 0% AMBIGUOUS · INFERRED: 85 edges (avg confidence: 0.8)
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

## God Nodes (most connected - your core abstractions)
1. `ApiClient` - 30 edges
2. `parse_response()` - 24 edges
3. `BeebeebFs` - 21 edges
4. `load_config()` - 17 edges
5. `run()` - 15 edges
6. `handle_webdav()` - 13 edges
7. `push_single_file()` - 11 edges
8. `propfind_response()` - 10 edges
9. `resolve_path()` - 9 edges
10. `put_response()` - 8 edges

## Surprising Connections (you probably didn't know these)
- `push_single_file()` --calls--> `mark_uploaded()`  [INFERRED]
  commands/push.rs → loopback.rs
- `load_master_key()` --calls--> `load_config()`  [INFERRED]
  commands/watch_remote.rs → config.rs
- `load_config()` --calls--> `run()`  [INFERRED]
  config.rs → commands/config.rs
- `load_config()` --calls--> `load_master_key()`  [INFERRED]
  config.rs → commands/pull.rs
- `load_config()` --calls--> `load_master_key()`  [INFERRED]
  config.rs → commands/sync.rs

## Communities

### Community 0 - "Community 0"
Cohesion: 0.07
Nodes (35): run(), b64(), browser_login(), BrowserState, CallbackPayload, handle_callback(), legacy_login(), LoginResult (+27 more)

### Community 1 - "Community 1"
Cohesion: 0.14
Nodes (33): CachedDir, check_lock(), child_href(), DavState, decode_file_entry(), decrypt_chunks(), decrypt_name(), delete_response() (+25 more)

### Community 2 - "Community 2"
Cohesion: 0.21
Nodes (2): ApiClient, parse_response()

### Community 3 - "Community 3"
Cohesion: 0.15
Nodes (6): BeebeebFs, CachedDir, decrypt_chunks(), decrypt_name(), InodeEntry, PendingCreate

### Community 4 - "Community 4"
Cohesion: 0.16
Nodes (19): b64(), create_folder(), do_download(), do_upload(), download_to(), FileEntry, format_size(), load_master_key() (+11 more)

### Community 5 - "Community 5"
Cohesion: 0.18
Nodes (18): b64(), collect_entries(), ConflictResolution, ConflictStrategy, find_conflict(), format_size(), load_master_key(), prompt_conflict() (+10 more)

### Community 6 - "Community 6"
Cohesion: 0.23
Nodes (13): decrypt_file(), download_to_mirror(), extract_data(), handle_event(), load_master_key(), next_backoff(), run(), sanitize_filename() (+5 more)

### Community 7 - "Community 7"
Cohesion: 0.54
Nodes (6): b64(), format_size(), load_master_key(), pull_folder(), pull_single_file(), run()

## Knowledge Gaps
- **17 isolated node(s):** `Cli`, `Commands`, `SyncState`, `FileEntry`, `LocalFile` (+12 more)
  These have ≤1 connection - possible missing edges or undocumented components.
- **Thin community `Community 2`** (31 nodes): `api.rs`, `ApiClient`, `.check_conflict()`, `.create_folder()`, `.create_share()`, `.create_stream_token()`, `.delete_share()`, `.download_file()`, `.get_file()`, `.get_file_count()`, `.get_me()`, `.get_region()`, `.get_sessions()`, `.get_subscription()`, `.get_usage()`, `.list_files()`, `.list_ops_since()`, `.list_shares()`, `.login()`, `.logout()`, `.move_file()`, `.opaque_login_finish()`, `.opaque_login_start()`, `.open_sync_stream()`, `.require_auth()`, `.signup()`, `.stream_url()`, `.trash_file()`, `.upload_encrypted()`, `.url()`, `parse_response()`
  Too small to be a meaningful cluster - may be noise or needs more connections extracted.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `ApiClient` connect `Community 2` to `Community 0`?**
  _High betweenness centrality (0.183) - this node is a cross-community bridge._
- **Why does `BeebeebFs` connect `Community 3` to `Community 0`, `Community 4`, `Community 7`?**
  _High betweenness centrality (0.111) - this node is a cross-community bridge._
- **Why does `load_config()` connect `Community 0` to `Community 4`, `Community 5`, `Community 6`, `Community 7`?**
  _High betweenness centrality (0.099) - this node is a cross-community bridge._
- **Are the 14 inferred relationships involving `load_config()` (e.g. with `.from_config()` and `load_master_key()`) actually correct?**
  _`load_config()` has 14 INFERRED edges - model-reasoned connections that need verification._
- **Are the 5 inferred relationships involving `run()` (e.g. with `.from_config()` and `.read()`) actually correct?**
  _`run()` has 5 INFERRED edges - model-reasoned connections that need verification._
- **What connects `Cli`, `Commands`, `SyncState` to the rest of the system?**
  _17 weakly-connected nodes found - possible documentation gaps or missing edges._
- **Should `Community 0` be split into smaller, more focused modules?**
  _Cohesion score 0.07 - nodes in this community are weakly interconnected._
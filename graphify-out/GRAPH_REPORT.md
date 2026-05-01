# Graph Report - cli  (2026-05-01)

## Corpus Check
- 14 files · ~7,472 words
- Verdict: corpus is large enough that graph structure adds value.

## Summary
- 81 nodes · 164 edges · 9 communities detected
- Extraction: 86% EXTRACTED · 14% INFERRED · 0% AMBIGUOUS · INFERRED: 23 edges (avg confidence: 0.8)
- Token cost: 0 input · 0 output

## Community Hubs (Navigation)
- [[_COMMUNITY_Community 0|Community 0]]
- [[_COMMUNITY_Community 1|Community 1]]
- [[_COMMUNITY_Community 2|Community 2]]
- [[_COMMUNITY_Community 3|Community 3]]
- [[_COMMUNITY_Community 4|Community 4]]
- [[_COMMUNITY_Community 6|Community 6]]
- [[_COMMUNITY_Community 7|Community 7]]
- [[_COMMUNITY_Community 8|Community 8]]
- [[_COMMUNITY_Community 9|Community 9]]

## God Nodes (most connected - your core abstractions)
1. `ApiClient` - 21 edges
2. `parse_response()` - 17 edges
3. `load_config()` - 10 edges
4. `run()` - 6 edges
5. `load_master_key()` - 6 edges
6. `load_master_key()` - 5 edges
7. `push_single_file()` - 5 edges
8. `push_directory()` - 5 edges
9. `run()` - 5 edges
10. `run()` - 5 edges

## Surprising Connections (you probably didn't know these)
- `load_master_key()` --calls--> `load_config()`  [INFERRED]
  commands/push.rs → config.rs
- `run()` --calls--> `load_config()`  [INFERRED]
  commands/login.rs → config.rs
- `run()` --calls--> `load_config()`  [INFERRED]
  commands/config.rs → config.rs
- `load_master_key()` --calls--> `load_config()`  [INFERRED]
  commands/pull.rs → config.rs
- `run()` --calls--> `load_config()`  [INFERRED]
  commands/status.rs → config.rs

## Communities

### Community 0 - "Community 0"
Cohesion: 0.18
Nodes (9): run(), list(), parse_hours(), revoke(), run(), run(), Cli, Commands (+1 more)

### Community 1 - "Community 1"
Cohesion: 0.27
Nodes (7): run(), run(), clear_config(), Config, config_path(), load_config(), save_config()

### Community 2 - "Community 2"
Cohesion: 0.38
Nodes (1): ApiClient

### Community 3 - "Community 3"
Cohesion: 0.44
Nodes (7): b64(), collect_entries(), format_size(), load_master_key(), push_directory(), push_single_file(), run()

### Community 4 - "Community 4"
Cohesion: 0.62
Nodes (6): b64(), format_size(), load_master_key(), pull_folder(), pull_single_file(), run()

### Community 6 - "Community 6"
Cohesion: 0.33
Nodes (1): parse_response()

### Community 7 - "Community 7"
Cohesion: 0.6
Nodes (5): b64(), legacy_login(), LoginResult, opaque_login(), run()

### Community 8 - "Community 8"
Cohesion: 0.6
Nodes (5): chrono_now(), ctrlc_channel(), relevant_paths(), run(), sync_batch()

### Community 9 - "Community 9"
Cohesion: 0.67
Nodes (2): get_session_expiry(), run()

## Knowledge Gaps
- **3 isolated node(s):** `Cli`, `Commands`, `LoginResult`
  These have ≤1 connection - possible missing edges or undocumented components.
- **Thin community `Community 2`** (10 nodes): `ApiClient`, `.create_share()`, `.download_file()`, `.get_file()`, `.get_me()`, `.get_region()`, `.get_usage()`, `.opaque_login_start()`, `.signup()`, `.url()`
  Too small to be a meaningful cluster - may be noise or needs more connections extracted.
- **Thin community `Community 6`** (6 nodes): `api.rs`, `.list_files()`, `.list_shares()`, `.login()`, `.opaque_login_finish()`, `parse_response()`
  Too small to be a meaningful cluster - may be noise or needs more connections extracted.
- **Thin community `Community 9`** (4 nodes): `format_bytes()`, `get_session_expiry()`, `status.rs`, `run()`
  Too small to be a meaningful cluster - may be noise or needs more connections extracted.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `ApiClient` connect `Community 2` to `Community 0`, `Community 5`, `Community 6`?**
  _High betweenness centrality (0.404) - this node is a cross-community bridge._
- **Why does `load_config()` connect `Community 1` to `Community 0`, `Community 3`, `Community 4`, `Community 7`, `Community 9`?**
  _High betweenness centrality (0.235) - this node is a cross-community bridge._
- **Why does `run()` connect `Community 7` to `Community 0`, `Community 1`?**
  _High betweenness centrality (0.135) - this node is a cross-community bridge._
- **Are the 7 inferred relationships involving `load_config()` (e.g. with `.from_config()` and `load_master_key()`) actually correct?**
  _`load_config()` has 7 INFERRED edges - model-reasoned connections that need verification._
- **Are the 3 inferred relationships involving `run()` (e.g. with `.from_config()` and `load_config()`) actually correct?**
  _`run()` has 3 INFERRED edges - model-reasoned connections that need verification._
- **What connects `Cli`, `Commands`, `LoginResult` to the rest of the system?**
  _3 weakly-connected nodes found - possible documentation gaps or missing edges._
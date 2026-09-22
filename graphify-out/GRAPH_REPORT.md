# Graph Report - cli-0477  (2026-09-22)

## Corpus Check
- 57 files · ~80,038 words
- Verdict: corpus is large enough that graph structure adds value.

## Summary
- 678 nodes · 1877 edges · 19 communities detected
- Extraction: 68% EXTRACTED · 32% INFERRED · 0% AMBIGUOUS · INFERRED: 605 edges (avg confidence: 0.8)
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
- [[_COMMUNITY_Community 18|Community 18]]

## God Nodes (most connected - your core abstractions)
1. `ApiClient` - 74 edges
2. `parse_response()` - 59 edges
3. `is_json()` - 38 edges
4. `run()` - 36 edges
5. `is_quiet()` - 34 edges
6. `main()` - 29 edges
7. `load_master_key()` - 28 edges
8. `load_master_key()` - 27 edges
9. `run()` - 21 edges
10. `BeebeebFs` - 21 edges

## Surprising Connections (you probably didn't know these)
- `main()` --calls--> `set_api_url_override()`  [INFERRED]
  src/main.rs → src/config.rs
- `expected_ciphertext_for()` --calls--> `run()`  [INFERRED]
  src/upload.rs → src/commands/sync.rs
- `is_rich()` --calls--> `run()`  [INFERRED]
  src/ui.rs → src/commands/rm.rs
- `is_rich()` --calls--> `run()`  [INFERRED]
  src/ui.rs → src/commands/pull.rs
- `is_rich()` --calls--> `pull_single_file()`  [INFERRED]
  src/ui.rs → src/commands/pull.rs

## Communities

### Community 0 - "Community 0"
Cohesion: 0.07
Nodes (16): ApiClient, backoff(), build_client(), every_request_carries_client_and_version_headers(), format_request_error(), ids(), is_transient_transport_error(), last_page_null_cursor_terminates_the_walk() (+8 more)

### Community 1 - "Community 1"
Cohesion: 0.06
Nodes (68): print_email_change_success(), run(), decrypt_listing(), DecryptedFile, LsOpts, print_header(), print_json(), print_recursive() (+60 more)

### Community 2 - "Community 2"
Cohesion: 0.08
Nodes (70): Match, Node, walk_mem(), walk_mem_anchored_subtree_only(), walk_mem_dfs_order_paths_and_walked(), walk_mem_stops_at_limit(), capitalise(), classify_new_local_is_not_a_delete() (+62 more)

### Community 3 - "Community 3"
Cohesion: 0.06
Nodes (29): addons(), capitalise(), format_date_human(), format_number(), format_price(), price_from_catalog(), price_from_subscription(), print_addons() (+21 more)

### Community 4 - "Community 4"
Cohesion: 0.08
Nodes (37): b64(), check_quota(), collect_entries(), ConflictResolution, ConflictStrategy, dir_total_size(), find_conflict(), prompt_conflict() (+29 more)

### Community 5 - "Community 5"
Cohesion: 0.09
Nodes (43): print_created(), run(), run_recursive(), split_parent_and_leaf(), CachedDir, check_lock(), child_href(), DavState (+35 more)

### Community 6 - "Community 6"
Cohesion: 0.07
Nodes (27): AccountCmd, AddonsAction, BillingAction, Cli, Commands, live(), peak(), RequestCmd (+19 more)

### Community 7 - "Community 7"
Cohesion: 0.11
Nodes (13): BeebeebFs, CachedDir, InodeEntry, PendingCreate, unmount(), AtomicFile, buffered_fallback(), DownloadStats (+5 more)

### Community 8 - "Community 8"
Cohesion: 0.15
Nodes (26): spawn_sync_daemon(), stop_all_sessions(), stop_session_by_name(), daemon_dir(), install_launchagent(), install_systemd_unit(), is_daemon_running(), kill_daemon() (+18 more)

### Community 9 - "Community 9"
Cohesion: 0.11
Nodes (21): portal(), browser_login(), print_browser_block(), print_headless_block(), run(), spawn_countdown(), run(), clear_config() (+13 more)

### Community 10 - "Community 10"
Cohesion: 0.14
Nodes (20): b64std(), b64url(), build_link(), decode_any_b64(), generate_request_keypair(), keypair_wrap_unwrap_roundtrip_matches_create_then_list(), link_assembly_roundtrips_through_parse(), parse_expiry_secs() (+12 more)

### Community 11 - "Community 11"
Cohesion: 0.13
Nodes (18): check_and_update(), cooldown_elapsed(), ct_eq_ignore_case(), current_target(), DistArtifact, DistChecksums, DistManifest, extract_binary_from_tarball() (+10 more)

### Community 12 - "Community 12"
Cohesion: 0.22
Nodes (19): decrypt_file_chunks(), decrypt_json_chunks(), decrypt_name(), decrypt_name_plaintext_passthrough(), decrypt_name_with_key(), decrypt_names(), decrypt_names_batch_matches_single(), decrypt_raw_chunks() (+11 more)

### Community 13 - "Community 13"
Cohesion: 0.15
Nodes (12): build_show_payload(), build_show_payload_assembles_all_sections(), build_show_payload_degrades_gracefully_per_section(), map_email_change_error(), normalize_update_email(), opaque_email_change(), render_progress_bar(), show() (+4 more)

### Community 14 - "Community 14"
Cohesion: 0.3
Nodes (11): collect_all_files(), decrypt_chunks_with_binary_key(), detect_key_derivation(), encrypt_chunks_with_string_key(), encrypt_name_with_string_key(), KeyDerivation, repair_file(), repair_folder() (+3 more)

### Community 15 - "Community 15"
Cohesion: 0.25
Nodes (6): FileEventStatus, SessionInfo, SyncFileEvent, SyncStatus, TuiState, TuiView

### Community 16 - "Community 16"
Cohesion: 0.52
Nodes (6): decrypt_payload_matches_webcrypto(), ecdh_shared_secret_matches_webcrypto(), full_flow_ecdh_to_plaintext_via_core(), hex32(), hex_decode(), hkdf_aes_key_matches_webcrypto()

### Community 17 - "Community 17"
Cohesion: 0.7
Nodes (4): build_plan_label(), capitalise(), format_number(), run()

### Community 18 - "Community 18"
Cohesion: 0.83
Nodes (3): render_otpauth(), renders_a_typical_totp_uri(), renders_empty_for_garbage_that_cannot_encode()

## Knowledge Gaps
- **57 isolated node(s):** `ThumbnailResult`, `OutputMode`, `DeviceInfo`, `DownloadStats`, `PendingDb` (+52 more)
  These have ≤1 connection - possible missing edges or undocumented components.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `ApiClient` connect `Community 0` to `Community 1`?**
  _High betweenness centrality (0.107) - this node is a cross-community bridge._
- **Why does `run()` connect `Community 2` to `Community 8`, `Community 1`, `Community 4`, `Community 6`?**
  _High betweenness centrality (0.083) - this node is a cross-community bridge._
- **Why does `main()` connect `Community 3` to `Community 1`, `Community 5`, `Community 6`, `Community 7`, `Community 9`, `Community 10`, `Community 11`, `Community 13`?**
  _High betweenness centrality (0.077) - this node is a cross-community bridge._
- **Are the 37 inferred relationships involving `is_json()` (e.g. with `create()` and `list()`) actually correct?**
  _`is_json()` has 37 INFERRED edges - model-reasoned connections that need verification._
- **Are the 15 inferred relationships involving `run()` (e.g. with `.from_config()` and `uninstall_launchagent()`) actually correct?**
  _`run()` has 15 INFERRED edges - model-reasoned connections that need verification._
- **Are the 33 inferred relationships involving `is_quiet()` (e.g. with `parse_response()` and `create()`) actually correct?**
  _`is_quiet()` has 33 INFERRED edges - model-reasoned connections that need verification._
- **What connects `ThumbnailResult`, `OutputMode`, `DeviceInfo` to the rest of the system?**
  _57 weakly-connected nodes found - possible documentation gaps or missing edges._
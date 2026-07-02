# Graph Report - cli-1129-check  (2026-07-02)

## Corpus Check
- 58 files · ~80,333 words
- Verdict: corpus is large enough that graph structure adds value.

## Summary
- 673 nodes · 1892 edges · 19 communities detected
- Extraction: 67% EXTRACTED · 33% INFERRED · 0% AMBIGUOUS · INFERRED: 619 edges (avg confidence: 0.8)
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
3. `is_json()` - 37 edges
4. `run()` - 36 edges
5. `is_quiet()` - 34 edges
6. `load_master_key()` - 29 edges
7. `load_master_key()` - 28 edges
8. `main()` - 24 edges
9. `decrypt_name()` - 23 edges
10. `BeebeebFs` - 21 edges

## Surprising Connections (you probably didn't know these)
- `expected_ciphertext_for()` --calls--> `run()`  [INFERRED]
  src/upload.rs → src/commands/sync.rs
- `percent_decode()` --calls--> `resolve_path()`  [INFERRED]
  src/path.rs → src/commands/webdav.rs
- `resolve_path()` --calls--> `decrypt_name()`  [INFERRED]
  src/path.rs → src/commands/mount.rs
- `list_children_names()` --calls--> `decrypt_name()`  [INFERRED]
  src/path.rs → src/commands/mount.rs
- `find_child_by_name()` --calls--> `decrypt_name()`  [INFERRED]
  src/path.rs → src/commands/mount.rs

## Communities

### Community 0 - "Community 0"
Cohesion: 0.06
Nodes (74): print_email_change_success(), purchase_addon(), decrypt_listing(), DecryptedFile, LsOpts, print_header(), print_json(), print_recursive() (+66 more)

### Community 1 - "Community 1"
Cohesion: 0.08
Nodes (13): ApiClient, backoff(), format_request_error(), ids(), is_transient_transport_error(), last_page_null_cursor_terminates_the_walk(), list_files_walks_every_page_past_the_200_cap(), list_trashed_walks_every_page() (+5 more)

### Community 2 - "Community 2"
Cohesion: 0.08
Nodes (70): build_plan_label(), capitalise(), format_number(), run(), Match, Node, run(), walk_mem() (+62 more)

### Community 3 - "Community 3"
Cohesion: 0.06
Nodes (37): AccountCmd, AddonsAction, BillingAction, Cli, Commands, live(), peak(), RequestCmd (+29 more)

### Community 4 - "Community 4"
Cohesion: 0.12
Nodes (37): confirm(), count(), permanent_delete_flow(), run(), Target, CachedDir, check_lock(), child_href() (+29 more)

### Community 5 - "Community 5"
Cohesion: 0.08
Nodes (29): print_created(), run(), run_recursive(), split_parent_and_leaf(), run(), chrono_now(), ctrlc_channel(), relevant_paths() (+21 more)

### Community 6 - "Community 6"
Cohesion: 0.11
Nodes (14): BeebeebFs, CachedDir, InodeEntry, PendingCreate, run(), unmount(), AtomicFile, buffered_fallback() (+6 more)

### Community 7 - "Community 7"
Cohesion: 0.1
Nodes (24): portal(), run(), browser_login(), print_browser_block(), print_headless_block(), run(), spawn_countdown(), run() (+16 more)

### Community 8 - "Community 8"
Cohesion: 0.11
Nodes (23): b64(), check_quota(), collect_entries(), ConflictResolution, ConflictStrategy, dir_total_size(), find_conflict(), prompt_conflict() (+15 more)

### Community 9 - "Community 9"
Cohesion: 0.15
Nodes (26): spawn_sync_daemon(), stop_all_sessions(), stop_session_by_name(), daemon_dir(), install_launchagent(), install_systemd_unit(), is_daemon_running(), kill_daemon() (+18 more)

### Community 10 - "Community 10"
Cohesion: 0.15
Nodes (22): b64std(), b64url(), build_link(), create(), decode_any_b64(), generate_request_keypair(), keypair_wrap_unwrap_roundtrip_matches_create_then_list(), link_assembly_roundtrips_through_parse() (+14 more)

### Community 11 - "Community 11"
Cohesion: 0.13
Nodes (18): check_and_update(), cooldown_elapsed(), ct_eq_ignore_case(), current_target(), DistArtifact, DistChecksums, DistManifest, extract_binary_from_tarball() (+10 more)

### Community 12 - "Community 12"
Cohesion: 0.15
Nodes (12): build_show_payload(), build_show_payload_assembles_all_sections(), build_show_payload_degrades_gracefully_per_section(), map_email_change_error(), normalize_update_email(), opaque_email_change(), render_progress_bar(), show() (+4 more)

### Community 13 - "Community 13"
Cohesion: 0.15
Nodes (9): addons(), capitalise(), format_date_human(), format_number(), format_price(), price_from_catalog(), price_from_subscription(), print_addons() (+1 more)

### Community 14 - "Community 14"
Cohesion: 0.24
Nodes (17): decrypt_file_chunks(), decrypt_json_chunks(), decrypt_name(), decrypt_name_plaintext_passthrough(), decrypt_names(), decrypt_names_batch_matches_single(), decrypt_raw_chunks(), json_blob_legacy_format_detected_and_decrypted() (+9 more)

### Community 15 - "Community 15"
Cohesion: 0.24
Nodes (12): download_to_mirror(), extract_data(), handle_event(), load_master_key(), next_backoff(), run(), sanitize_filename(), unlink_in_mirror() (+4 more)

### Community 16 - "Community 16"
Cohesion: 0.25
Nodes (6): FileEventStatus, SessionInfo, SyncFileEvent, SyncStatus, TuiState, TuiView

### Community 17 - "Community 17"
Cohesion: 0.52
Nodes (6): decrypt_payload_matches_webcrypto(), ecdh_shared_secret_matches_webcrypto(), full_flow_ecdh_to_plaintext_via_core(), hex32(), hex_decode(), hkdf_aes_key_matches_webcrypto()

### Community 18 - "Community 18"
Cohesion: 0.83
Nodes (3): render_otpauth(), renders_a_typical_totp_uri(), renders_empty_for_garbage_that_cannot_encode()

## Knowledge Gaps
- **56 isolated node(s):** `CacheEntry`, `ResolvedPath`, `DownloadStats`, `UploadSpec`, `UploadOutcome` (+51 more)
  These have ≤1 connection - possible missing edges or undocumented components.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `ApiClient` connect `Community 1` to `Community 0`?**
  _High betweenness centrality (0.107) - this node is a cross-community bridge._
- **Why does `run()` connect `Community 2` to `Community 0`, `Community 3`, `Community 7`, `Community 8`, `Community 9`?**
  _High betweenness centrality (0.082) - this node is a cross-community bridge._
- **Why does `is_quiet()` connect `Community 0` to `Community 1`, `Community 2`, `Community 4`, `Community 5`, `Community 8`, `Community 9`, `Community 10`?**
  _High betweenness centrality (0.063) - this node is a cross-community bridge._
- **Are the 36 inferred relationships involving `is_json()` (e.g. with `run()` and `move_bulk()`) actually correct?**
  _`is_json()` has 36 INFERRED edges - model-reasoned connections that need verification._
- **Are the 15 inferred relationships involving `run()` (e.g. with `.from_config()` and `uninstall_launchagent()`) actually correct?**
  _`run()` has 15 INFERRED edges - model-reasoned connections that need verification._
- **Are the 33 inferred relationships involving `is_quiet()` (e.g. with `parse_response()` and `run()`) actually correct?**
  _`is_quiet()` has 33 INFERRED edges - model-reasoned connections that need verification._
- **What connects `CacheEntry`, `ResolvedPath`, `DownloadStats` to the rest of the system?**
  _56 weakly-connected nodes found - possible documentation gaps or missing edges._
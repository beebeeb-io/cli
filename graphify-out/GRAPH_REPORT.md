# Graph Report - cli-0482  (2026-09-23)

## Corpus Check
- 59 files · ~89,779 words
- Verdict: corpus is large enough that graph structure adds value.

## Summary
- 820 nodes · 2203 edges · 24 communities detected
- Extraction: 71% EXTRACTED · 29% INFERRED · 0% AMBIGUOUS · INFERRED: 643 edges (avg confidence: 0.8)
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
- [[_COMMUNITY_Community 19|Community 19]]
- [[_COMMUNITY_Community 20|Community 20]]
- [[_COMMUNITY_Community 21|Community 21]]
- [[_COMMUNITY_Community 22|Community 22]]
- [[_COMMUNITY_Community 23|Community 23]]

## God Nodes (most connected - your core abstractions)
1. `ApiClient` - 79 edges
2. `parse_response()` - 64 edges
3. `is_json()` - 45 edges
4. `is_quiet()` - 42 edges
5. `run()` - 36 edges
6. `main()` - 29 edges
7. `load_master_key()` - 28 edges
8. `load_master_key()` - 27 edges
9. `run()` - 21 edges
10. `BeebeebFs` - 21 edges

## Surprising Connections (you probably didn't know these)
- `set_api_url_override()` --calls--> `main()`  [INFERRED]
  src/config.rs → src/main.rs
- `expected_ciphertext_for()` --calls--> `run()`  [INFERRED]
  src/upload.rs → src/commands/sync.rs
- `generate_from_file()` --calls--> `upload_file_to()`  [INFERRED]
  src/thumbnail.rs → src/commands/sync.rs
- `generate_large_from_file()` --calls--> `upload_file_to()`  [INFERRED]
  src/thumbnail.rs → src/commands/sync.rs
- `init()` --calls--> `main()`  [INFERRED]
  src/ui.rs → src/main.rs

## Communities

### Community 0 - "Community 0"
Cohesion: 0.06
Nodes (102): format_number(), Match, Node, print_results(), run(), walk_mem(), walk_mem_anchored_subtree_only(), walk_mem_dfs_order_paths_and_walked() (+94 more)

### Community 1 - "Community 1"
Cohesion: 0.07
Nodes (23): ApiClient, backoff(), build_client(), echo_method_id_and_headers(), every_request_carries_client_and_version_headers(), format_request_error(), ids(), is_transient_transport_error() (+15 more)

### Community 2 - "Community 2"
Cohesion: 0.06
Nodes (82): print_email_change_success(), portal(), purchase_addon(), run(), run(), decrypt_listing(), DecryptedFile, LsOpts (+74 more)

### Community 3 - "Community 3"
Cohesion: 0.06
Nodes (40): AccountCmd, AddonsAction, BillingAction, Cli, Commands, live(), PasskeyCmd, peak() (+32 more)

### Community 4 - "Community 4"
Cohesion: 0.07
Nodes (39): render_otpauth(), renders_a_typical_totp_uri(), renders_empty_for_garbage_that_cannot_encode(), classify_disable_error(), classify_disable_error_passes_through_unrecognized_errors(), classify_disable_error_reports_ambiguous_on_bare_unauthorized(), classify_disable_error_reports_not_enabled_when_never_set_up(), classify_disable_error_reports_not_enabled_when_row_disabled() (+31 more)

### Community 5 - "Community 5"
Cohesion: 0.07
Nodes (33): classify_list_error(), confirm_revoke_all(), extract_sessions_json(), guard_not_current(), guard_not_current_allows_a_non_current_session(), guard_not_current_refuses_the_current_session(), json_mode_extracts_the_raw_sessions_array_unmodified(), list() (+25 more)

### Community 6 - "Community 6"
Cohesion: 0.08
Nodes (30): b64(), check_quota(), collect_entries(), ConflictResolution, ConflictStrategy, dir_total_size(), find_conflict(), prompt_conflict() (+22 more)

### Community 7 - "Community 7"
Cohesion: 0.11
Nodes (13): BeebeebFs, CachedDir, InodeEntry, PendingCreate, unmount(), AtomicFile, buffered_fallback(), DownloadStats (+5 more)

### Community 8 - "Community 8"
Cohesion: 0.14
Nodes (32): CachedDir, check_lock(), child_href(), DavState, decode_file_entry(), decrypt_name(), delete_response(), get_from_cache() (+24 more)

### Community 9 - "Community 9"
Cohesion: 0.12
Nodes (19): classify_list_error(), extract_passkeys_json(), json_mode_extracts_the_raw_passkeys_array_unmodified(), list(), parse_passkeys(), parse_passkeys_defaults_a_missing_name_to_passkey(), parse_passkeys_defaults_an_empty_name_to_passkey(), parse_passkeys_reads_every_field_from_the_live_shape() (+11 more)

### Community 10 - "Community 10"
Cohesion: 0.12
Nodes (20): browser_login(), print_browser_block(), print_headless_block(), run(), spawn_countdown(), clear_config(), Config, config_path() (+12 more)

### Community 11 - "Community 11"
Cohesion: 0.15
Nodes (19): b64std(), b64url(), build_link(), decode_any_b64(), generate_request_keypair(), keypair_wrap_unwrap_roundtrip_matches_create_then_list(), link_assembly_roundtrips_through_parse(), parse_expiry_secs() (+11 more)

### Community 12 - "Community 12"
Cohesion: 0.15
Nodes (12): build_show_payload(), build_show_payload_assembles_all_sections(), build_show_payload_degrades_gracefully_per_section(), map_email_change_error(), normalize_update_email(), opaque_email_change(), render_progress_bar(), show() (+4 more)

### Community 13 - "Community 13"
Cohesion: 0.14
Nodes (11): check_and_update(), cooldown_elapsed(), ct_eq_ignore_case(), current_target(), DistArtifact, DistChecksums, DistManifest, GitHubAsset (+3 more)

### Community 14 - "Community 14"
Cohesion: 0.26
Nodes (16): decrypt_file_chunks(), decrypt_name(), decrypt_name_plaintext_passthrough(), decrypt_name_with_key(), decrypt_names(), decrypt_names_batch_matches_single(), json_blob_legacy_format_detected_and_decrypted(), json_blob_with_binary_uuid_key() (+8 more)

### Community 15 - "Community 15"
Cohesion: 0.16
Nodes (8): addons(), capitalise(), format_date_human(), format_price(), price_from_catalog(), price_from_subscription(), print_addons(), show()

### Community 16 - "Community 16"
Cohesion: 0.16
Nodes (8): print_custom_help(), box_line(), init(), OutputMode, strip_ansi(), table(), table_aligns_columns_by_widest_cell(), table_width_calculation_ignores_ansi_escapes()

### Community 17 - "Community 17"
Cohesion: 0.3
Nodes (11): collect_all_files(), decrypt_chunks_with_binary_key(), detect_key_derivation(), encrypt_chunks_with_string_key(), encrypt_name_with_string_key(), KeyDerivation, repair_file(), repair_folder() (+3 more)

### Community 18 - "Community 18"
Cohesion: 0.33
Nodes (7): CacheEntry, find_child_by_name(), hex_val(), list_files_cached(), percent_decode(), resolve_path(), ResolvedPath

### Community 19 - "Community 19"
Cohesion: 0.25
Nodes (6): FileEventStatus, SessionInfo, SyncFileEvent, SyncStatus, TuiState, TuiView

### Community 20 - "Community 20"
Cohesion: 0.52
Nodes (6): decrypt_payload_matches_webcrypto(), ecdh_shared_secret_matches_webcrypto(), full_flow_ecdh_to_plaintext_via_core(), hex32(), hex_decode(), hkdf_aes_key_matches_webcrypto()

### Community 21 - "Community 21"
Cohesion: 0.53
Nodes (5): current_platform(), device_path(), DeviceInfo, get_device_id(), load_or_create()

### Community 22 - "Community 22"
Cohesion: 0.4
Nodes (2): build_plan_label(), capitalise()

### Community 23 - "Community 23"
Cohesion: 0.7
Nodes (4): build_plan_label(), capitalise(), format_number(), run()

## Knowledge Gaps
- **60 isolated node(s):** `ThumbnailResult`, `OutputMode`, `DeviceInfo`, `DownloadStats`, `PendingDb` (+55 more)
  These have ≤1 connection - possible missing edges or undocumented components.
- **Thin community `Community 22`** (6 nodes): `build_plan_label()`, `capitalise()`, `upload_limit_for_plan()`, `upload_limit_for_plan_covers_every_plan()`, `upload_limit_for_starter_matches_basic()`, `whoami.rs`
  Too small to be a meaningful cluster - may be noise or needs more connections extracted.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `ApiClient` connect `Community 1` to `Community 2`?**
  _High betweenness centrality (0.094) - this node is a cross-community bridge._
- **Why does `is_quiet()` connect `Community 2` to `Community 0`, `Community 1`, `Community 5`, `Community 6`, `Community 9`, `Community 16`, `Community 17`, `Community 23`?**
  _High betweenness centrality (0.091) - this node is a cross-community bridge._
- **Why does `run()` connect `Community 0` to `Community 2`, `Community 3`, `Community 21`, `Community 6`?**
  _High betweenness centrality (0.071) - this node is a cross-community bridge._
- **Are the 44 inferred relationships involving `is_json()` (e.g. with `list()` and `create()`) actually correct?**
  _`is_json()` has 44 INFERRED edges - model-reasoned connections that need verification._
- **Are the 41 inferred relationships involving `is_quiet()` (e.g. with `parse_response()` and `list()`) actually correct?**
  _`is_quiet()` has 41 INFERRED edges - model-reasoned connections that need verification._
- **Are the 15 inferred relationships involving `run()` (e.g. with `.from_config()` and `uninstall_launchagent()`) actually correct?**
  _`run()` has 15 INFERRED edges - model-reasoned connections that need verification._
- **What connects `ThumbnailResult`, `OutputMode`, `DeviceInfo` to the rest of the system?**
  _60 weakly-connected nodes found - possible documentation gaps or missing edges._
# Graph Report - cli-0481  (2026-09-23)

## Corpus Check
- 58 files · ~87,927 words
- Verdict: corpus is large enough that graph structure adds value.

## Summary
- 784 nodes · 2133 edges · 21 communities detected
- Extraction: 70% EXTRACTED · 30% INFERRED · 0% AMBIGUOUS · INFERRED: 638 edges (avg confidence: 0.8)
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

## God Nodes (most connected - your core abstractions)
1. `ApiClient` - 79 edges
2. `parse_response()` - 64 edges
3. `is_json()` - 44 edges
4. `is_quiet()` - 41 edges
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
Nodes (100): format_number(), collect_all_files(), encrypt_chunks_with_string_key(), Match, Node, print_results(), run(), walk_mem() (+92 more)

### Community 1 - "Community 1"
Cohesion: 0.07
Nodes (23): ApiClient, backoff(), build_client(), echo_method_id_and_headers(), every_request_carries_client_and_version_headers(), format_request_error(), ids(), is_transient_transport_error() (+15 more)

### Community 2 - "Community 2"
Cohesion: 0.05
Nodes (84): print_email_change_success(), portal(), purchase_addon(), run(), run(), decrypt_listing(), DecryptedFile, LsOpts (+76 more)

### Community 3 - "Community 3"
Cohesion: 0.06
Nodes (42): render_otpauth(), renders_a_typical_totp_uri(), renders_empty_for_garbage_that_cannot_encode(), classify_disable_error(), classify_disable_error_passes_through_unrecognized_errors(), classify_disable_error_reports_ambiguous_on_bare_unauthorized(), classify_disable_error_reports_not_enabled_when_never_set_up(), classify_disable_error_reports_not_enabled_when_row_disabled() (+34 more)

### Community 4 - "Community 4"
Cohesion: 0.06
Nodes (39): AccountCmd, AddonsAction, BillingAction, Cli, Commands, live(), peak(), RequestCmd (+31 more)

### Community 5 - "Community 5"
Cohesion: 0.06
Nodes (33): b64(), check_quota(), collect_entries(), ConflictResolution, ConflictStrategy, dir_total_size(), find_conflict(), prompt_conflict() (+25 more)

### Community 6 - "Community 6"
Cohesion: 0.08
Nodes (28): confirm_revoke_all(), extract_sessions_json(), guard_not_current(), guard_not_current_allows_a_non_current_session(), guard_not_current_refuses_the_current_session(), json_mode_extracts_the_raw_sessions_array_unmodified(), marker_cell(), parse_sessions() (+20 more)

### Community 7 - "Community 7"
Cohesion: 0.07
Nodes (30): browser_login(), print_browser_block(), print_headless_block(), run(), spawn_countdown(), clear_config(), Config, config_path() (+22 more)

### Community 8 - "Community 8"
Cohesion: 0.12
Nodes (37): confirm(), count(), permanent_delete_flow(), run(), Target, CachedDir, check_lock(), child_href() (+29 more)

### Community 9 - "Community 9"
Cohesion: 0.11
Nodes (13): BeebeebFs, CachedDir, InodeEntry, PendingCreate, unmount(), AtomicFile, buffered_fallback(), DownloadStats (+5 more)

### Community 10 - "Community 10"
Cohesion: 0.15
Nodes (21): b64std(), b64url(), build_link(), create(), decode_any_b64(), generate_request_keypair(), keypair_wrap_unwrap_roundtrip_matches_create_then_list(), link_assembly_roundtrips_through_parse() (+13 more)

### Community 11 - "Community 11"
Cohesion: 0.15
Nodes (12): build_show_payload(), build_show_payload_assembles_all_sections(), build_show_payload_degrades_gracefully_per_section(), map_email_change_error(), normalize_update_email(), opaque_email_change(), render_progress_bar(), show() (+4 more)

### Community 12 - "Community 12"
Cohesion: 0.14
Nodes (11): check_and_update(), cooldown_elapsed(), ct_eq_ignore_case(), current_target(), DistArtifact, DistChecksums, DistManifest, GitHubAsset (+3 more)

### Community 13 - "Community 13"
Cohesion: 0.26
Nodes (16): decrypt_file_chunks(), decrypt_name(), decrypt_name_plaintext_passthrough(), decrypt_name_with_key(), decrypt_names(), decrypt_names_batch_matches_single(), json_blob_legacy_format_detected_and_decrypted(), json_blob_with_binary_uuid_key() (+8 more)

### Community 14 - "Community 14"
Cohesion: 0.16
Nodes (8): addons(), capitalise(), format_date_human(), format_price(), price_from_catalog(), price_from_subscription(), print_addons(), show()

### Community 15 - "Community 15"
Cohesion: 0.36
Nodes (6): CacheEntry, hex_val(), list_files_cached(), percent_decode(), resolve_path(), ResolvedPath

### Community 16 - "Community 16"
Cohesion: 0.25
Nodes (6): FileEventStatus, SessionInfo, SyncFileEvent, SyncStatus, TuiState, TuiView

### Community 17 - "Community 17"
Cohesion: 0.52
Nodes (6): decrypt_payload_matches_webcrypto(), ecdh_shared_secret_matches_webcrypto(), full_flow_ecdh_to_plaintext_via_core(), hex32(), hex_decode(), hkdf_aes_key_matches_webcrypto()

### Community 18 - "Community 18"
Cohesion: 0.53
Nodes (5): current_platform(), device_path(), DeviceInfo, get_device_id(), load_or_create()

### Community 19 - "Community 19"
Cohesion: 0.4
Nodes (2): build_plan_label(), capitalise()

### Community 20 - "Community 20"
Cohesion: 0.7
Nodes (4): build_plan_label(), capitalise(), format_number(), run()

## Knowledge Gaps
- **59 isolated node(s):** `ThumbnailResult`, `OutputMode`, `DeviceInfo`, `DownloadStats`, `PendingDb` (+54 more)
  These have ≤1 connection - possible missing edges or undocumented components.
- **Thin community `Community 19`** (6 nodes): `build_plan_label()`, `capitalise()`, `upload_limit_for_plan()`, `upload_limit_for_plan_covers_every_plan()`, `upload_limit_for_starter_matches_basic()`, `whoami.rs`
  Too small to be a meaningful cluster - may be noise or needs more connections extracted.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `ApiClient` connect `Community 1` to `Community 0`, `Community 2`?**
  _High betweenness centrality (0.098) - this node is a cross-community bridge._
- **Why does `is_quiet()` connect `Community 2` to `Community 0`, `Community 1`, `Community 3`, `Community 5`, `Community 8`, `Community 10`, `Community 20`?**
  _High betweenness centrality (0.085) - this node is a cross-community bridge._
- **Why does `run()` connect `Community 0` to `Community 2`, `Community 4`, `Community 5`, `Community 7`, `Community 18`?**
  _High betweenness centrality (0.073) - this node is a cross-community bridge._
- **Are the 43 inferred relationships involving `is_json()` (e.g. with `create()` and `list()`) actually correct?**
  _`is_json()` has 43 INFERRED edges - model-reasoned connections that need verification._
- **Are the 40 inferred relationships involving `is_quiet()` (e.g. with `parse_response()` and `create()`) actually correct?**
  _`is_quiet()` has 40 INFERRED edges - model-reasoned connections that need verification._
- **Are the 15 inferred relationships involving `run()` (e.g. with `.from_config()` and `uninstall_launchagent()`) actually correct?**
  _`run()` has 15 INFERRED edges - model-reasoned connections that need verification._
- **What connects `ThumbnailResult`, `OutputMode`, `DeviceInfo` to the rest of the system?**
  _59 weakly-connected nodes found - possible documentation gaps or missing edges._
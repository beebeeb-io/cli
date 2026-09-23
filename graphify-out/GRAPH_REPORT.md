# Graph Report - cli-0483  (2026-09-23)

## Corpus Check
- 59 files · ~91,320 words
- Verdict: corpus is large enough that graph structure adds value.

## Summary
- 844 nodes · 2249 edges · 20 communities detected
- Extraction: 71% EXTRACTED · 29% INFERRED · 0% AMBIGUOUS · INFERRED: 649 edges (avg confidence: 0.8)
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

## God Nodes (most connected - your core abstractions)
1. `ApiClient` - 79 edges
2. `parse_response()` - 64 edges
3. `is_json()` - 46 edges
4. `is_quiet()` - 43 edges
5. `run()` - 36 edges
6. `main()` - 30 edges
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
- `generate_from_file()` --calls--> `push_single_file()`  [INFERRED]
  src/thumbnail.rs → src/commands/push.rs
- `generate_large_from_file()` --calls--> `upload_file_to()`  [INFERRED]
  src/thumbnail.rs → src/commands/sync.rs

## Communities

### Community 0 - "Community 0"
Cohesion: 0.06
Nodes (100): format_number(), Match, Node, print_results(), run(), walk_mem(), walk_mem_anchored_subtree_only(), walk_mem_dfs_order_paths_and_walked() (+92 more)

### Community 1 - "Community 1"
Cohesion: 0.06
Nodes (27): diagnose_unauthorized(), diagnose_unauthorized_blames_the_code_when_the_session_is_alive(), diagnose_unauthorized_blames_the_session_when_it_is_dead(), spawn_me_mock(), ApiClient, backoff(), build_client(), echo_method_id_and_headers() (+19 more)

### Community 2 - "Community 2"
Cohesion: 0.04
Nodes (76): print_email_change_success(), addons(), capitalise(), format_date_human(), format_price(), portal(), price_from_catalog(), price_from_subscription() (+68 more)

### Community 3 - "Community 3"
Cohesion: 0.05
Nodes (47): AccountCmd, AddonsAction, BillingAction, Cli, Commands, live(), PasskeyCmd, peak() (+39 more)

### Community 4 - "Community 4"
Cohesion: 0.08
Nodes (48): print_created(), run(), run_recursive(), split_parent_and_leaf(), confirm(), count(), permanent_delete_flow(), run() (+40 more)

### Community 5 - "Community 5"
Cohesion: 0.07
Nodes (34): add_json_body_carries_the_url_and_a_note_never_a_token(), AddAction, classify_list_error(), enrollment_url_bracketed_ipv6_loopback_is_local_and_port_is_stripped(), enrollment_url_does_not_treat_a_127_0_0_1_labeled_domain_as_local(), enrollment_url_does_not_treat_a_localhost_labeled_domain_as_local(), enrollment_url_for_local_api_points_at_the_local_dev_web_app(), enrollment_url_for_prod_api_points_at_the_prod_web_app() (+26 more)

### Community 6 - "Community 6"
Cohesion: 0.07
Nodes (33): classify_list_error(), confirm_revoke_all(), extract_sessions_json(), guard_not_current(), guard_not_current_allows_a_non_current_session(), guard_not_current_refuses_the_current_session(), json_mode_extracts_the_raw_sessions_array_unmodified(), list() (+25 more)

### Community 7 - "Community 7"
Cohesion: 0.07
Nodes (35): render_otpauth(), renders_a_typical_totp_uri(), renders_empty_for_garbage_that_cannot_encode(), classify_disable_error(), classify_disable_error_passes_through_unrecognized_errors(), classify_disable_error_reports_ambiguous_on_bare_unauthorized(), classify_disable_error_reports_not_enabled_when_never_set_up(), classify_disable_error_reports_not_enabled_when_row_disabled() (+27 more)

### Community 8 - "Community 8"
Cohesion: 0.12
Nodes (35): decrypt_name(), collect_zip_entries(), looks_like_id_prefix(), pull_folder(), pull_folder_inner(), pull_single_file(), resolve_as_path(), resolve_request_key() (+27 more)

### Community 9 - "Community 9"
Cohesion: 0.11
Nodes (14): BeebeebFs, CachedDir, InodeEntry, PendingCreate, run(), unmount(), AtomicFile, buffered_fallback() (+6 more)

### Community 10 - "Community 10"
Cohesion: 0.15
Nodes (21): b64std(), b64url(), build_link(), create(), decode_any_b64(), generate_request_keypair(), keypair_wrap_unwrap_roundtrip_matches_create_then_list(), link_assembly_roundtrips_through_parse() (+13 more)

### Community 11 - "Community 11"
Cohesion: 0.11
Nodes (21): run(), browser_login(), print_browser_block(), print_headless_block(), run(), spawn_countdown(), run(), passkey_enrollment_url() (+13 more)

### Community 12 - "Community 12"
Cohesion: 0.22
Nodes (19): decrypt_file_chunks(), decrypt_json_chunks(), decrypt_name(), decrypt_name_plaintext_passthrough(), decrypt_name_with_key(), decrypt_names(), decrypt_names_batch_matches_single(), decrypt_raw_chunks() (+11 more)

### Community 13 - "Community 13"
Cohesion: 0.15
Nodes (12): build_show_payload(), build_show_payload_assembles_all_sections(), build_show_payload_degrades_gracefully_per_section(), map_email_change_error(), normalize_update_email(), opaque_email_change(), render_progress_bar(), show() (+4 more)

### Community 14 - "Community 14"
Cohesion: 0.14
Nodes (11): check_and_update(), cooldown_elapsed(), ct_eq_ignore_case(), current_target(), DistArtifact, DistChecksums, DistManifest, GitHubAsset (+3 more)

### Community 15 - "Community 15"
Cohesion: 0.21
Nodes (10): EnvSnapshot, explicit_override_always_wins(), explicit_override_zero_is_not_a_yes(), is_headless(), is_headless_with(), snap(), ssh_tty_alone_counts_as_ssh(), ssh_with_x_forwarding_is_not_headless() (+2 more)

### Community 16 - "Community 16"
Cohesion: 0.19
Nodes (6): box_line(), OutputMode, strip_ansi(), table(), table_aligns_columns_by_widest_cell(), table_width_calculation_ignores_ansi_escapes()

### Community 17 - "Community 17"
Cohesion: 0.25
Nodes (6): FileEventStatus, SessionInfo, SyncFileEvent, SyncStatus, TuiState, TuiView

### Community 18 - "Community 18"
Cohesion: 0.52
Nodes (6): decrypt_payload_matches_webcrypto(), ecdh_shared_secret_matches_webcrypto(), full_flow_ecdh_to_plaintext_via_core(), hex32(), hex_decode(), hkdf_aes_key_matches_webcrypto()

### Community 19 - "Community 19"
Cohesion: 0.53
Nodes (5): current_platform(), device_path(), DeviceInfo, get_device_id(), load_or_create()

## Knowledge Gaps
- **61 isolated node(s):** `ThumbnailResult`, `OutputMode`, `DeviceInfo`, `DownloadStats`, `PendingDb` (+56 more)
  These have ≤1 connection - possible missing edges or undocumented components.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `is_quiet()` connect `Community 2` to `Community 0`, `Community 1`, `Community 4`, `Community 6`, `Community 8`, `Community 10`, `Community 16`?**
  _High betweenness centrality (0.096) - this node is a cross-community bridge._
- **Why does `ApiClient` connect `Community 1` to `Community 0`, `Community 2`?**
  _High betweenness centrality (0.091) - this node is a cross-community bridge._
- **Why does `run()` connect `Community 0` to `Community 8`, `Community 3`, `Community 2`, `Community 19`?**
  _High betweenness centrality (0.068) - this node is a cross-community bridge._
- **Are the 45 inferred relationships involving `is_json()` (e.g. with `list()` and `add()`) actually correct?**
  _`is_json()` has 45 INFERRED edges - model-reasoned connections that need verification._
- **Are the 42 inferred relationships involving `is_quiet()` (e.g. with `parse_response()` and `list()`) actually correct?**
  _`is_quiet()` has 42 INFERRED edges - model-reasoned connections that need verification._
- **Are the 15 inferred relationships involving `run()` (e.g. with `.from_config()` and `uninstall_launchagent()`) actually correct?**
  _`run()` has 15 INFERRED edges - model-reasoned connections that need verification._
- **What connects `ThumbnailResult`, `OutputMode`, `DeviceInfo` to the rest of the system?**
  _61 weakly-connected nodes found - possible documentation gaps or missing edges._
# Graph Report - cli-flow1-2  (2026-09-25)

## Corpus Check
- 60 files · ~104,500 words
- Verdict: corpus is large enough that graph structure adds value.

## Summary
- 953 nodes · 2525 edges · 21 communities detected
- Extraction: 72% EXTRACTED · 28% INFERRED · 0% AMBIGUOUS · INFERRED: 719 edges (avg confidence: 0.8)
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
1. `ApiClient` - 82 edges
2. `parse_response()` - 59 edges
3. `is_json()` - 50 edges
4. `is_quiet()` - 47 edges
5. `run()` - 36 edges
6. `main()` - 33 edges
7. `load_master_key()` - 28 edges
8. `load_master_key()` - 27 edges
9. `run()` - 22 edges
10. `BeebeebFs` - 21 edges

## Surprising Connections (you probably didn't know these)
- `oneRun()` --calls--> `String`  [INFERRED]
  scripts/prod-bots/auth-bot.mjs → src/api.rs
- `set_api_url_override()` --calls--> `main()`  [INFERRED]
  src/config.rs → src/main.rs
- `expected_ciphertext_for()` --calls--> `run()`  [INFERRED]
  src/upload.rs → src/commands/sync.rs
- `mark_uploaded()` --calls--> `push_single_file()`  [INFERRED]
  src/loopback.rs → src/commands/push.rs
- `generate_from_file()` --calls--> `upload_file_to()`  [INFERRED]
  src/thumbnail.rs → src/commands/sync.rs

## Communities

### Community 0 - "Community 0"
Cohesion: 0.05
Nodes (88): run(), run(), decrypt_listing(), DecryptedFile, LsOpts, print_header(), print_json(), print_recursive() (+80 more)

### Community 1 - "Community 1"
Cohesion: 0.07
Nodes (76): classify_new_local_is_not_a_delete(), classify_prior_synced_modified_is_readd(), classify_prior_synced_unchanged_is_remote_delete(), classify_remote_absent(), compute_file_hash(), create_folder(), do_download(), do_upload() (+68 more)

### Community 2 - "Community 2"
Cohesion: 0.09
Nodes (4): ApiClient, pace_if_needed(), parse_response(), parse_response_typed()

### Community 3 - "Community 3"
Cohesion: 0.05
Nodes (48): add(), add_json_body_carries_the_url_and_a_note_never_a_token(), AddAction, classify_list_error(), confirm_remove(), enrollment_url_bracketed_ipv6_loopback_is_local_and_port_is_stripped(), enrollment_url_does_not_treat_a_127_0_0_1_labeled_domain_as_local(), enrollment_url_does_not_treat_a_localhost_labeled_domain_as_local() (+40 more)

### Community 4 - "Community 4"
Cohesion: 0.04
Nodes (51): AccountCmd, AddonsAction, BillingAction, build_help_text(), Cli, Commands, help_screen_lists_every_top_level_subcommand(), live() (+43 more)

### Community 5 - "Community 5"
Cohesion: 0.06
Nodes (47): render_otpauth(), renders_a_typical_totp_uri(), renders_empty_for_garbage_that_cannot_encode(), classify_disable_error(), classify_disable_error_passes_through_unrecognized_errors(), classify_disable_error_reports_ambiguous_on_bare_unauthorized(), classify_disable_error_reports_not_enabled_when_never_set_up(), classify_disable_error_reports_not_enabled_when_row_disabled() (+39 more)

### Community 6 - "Community 6"
Cohesion: 0.05
Nodes (45): addons(), aggregate_by_region(), aggregate_by_region_defaults_missing_storage_location_to_europe_falkenstein(), aggregate_by_region_empty_input_is_empty_output(), aggregate_by_region_splits_multiple_regions(), aggregate_by_region_sums_bytes_per_region_and_skips_folders(), capitalise(), format_date_human() (+37 more)

### Community 7 - "Community 7"
Cohesion: 0.07
Nodes (34): classify_list_error(), confirm_revoke_all(), extract_sessions_json(), guard_not_current(), guard_not_current_allows_a_non_current_session(), guard_not_current_refuses_the_current_session(), json_mode_extracts_the_raw_sessions_array_unmodified(), list() (+26 more)

### Community 8 - "Community 8"
Cohesion: 0.09
Nodes (43): Match, Node, run(), walk_mem(), walk_mem_anchored_subtree_only(), walk_mem_dfs_order_paths_and_walked(), walk_mem_stops_at_limit(), backoff() (+35 more)

### Community 9 - "Community 9"
Cohesion: 0.09
Nodes (42): b64std(), b64url(), build_link(), create(), decode_any_b64(), generate_request_keypair(), keypair_wrap_unwrap_roundtrip_matches_create_then_list(), link_assembly_roundtrips_through_parse() (+34 more)

### Community 10 - "Community 10"
Cohesion: 0.09
Nodes (43): print_created(), run(), run_recursive(), split_parent_and_leaf(), CachedDir, check_lock(), child_href(), DavState (+35 more)

### Community 11 - "Community 11"
Cohesion: 0.1
Nodes (28): oneRun(), String, Config, combined(), dist_builds_with_fuse(), help_does_not_list_mount_in_a_build_without_fuse(), mount_stub_points_at_webdav_not_a_nonexistent_fuse_build(), readme_mount_claim_matches_the_dist_feature_list() (+20 more)

### Community 12 - "Community 12"
Cohesion: 0.11
Nodes (13): BeebeebFs, CachedDir, InodeEntry, PendingCreate, unmount(), AtomicFile, buffered_fallback(), DownloadStats (+5 more)

### Community 13 - "Community 13"
Cohesion: 0.12
Nodes (20): portal(), browser_login(), print_browser_block(), print_headless_block(), run(), spawn_countdown(), clear_config(), config_path() (+12 more)

### Community 14 - "Community 14"
Cohesion: 0.12
Nodes (19): SortField, check_and_update(), cooldown_elapsed(), ct_eq_ignore_case(), current_target(), DistArtifact, DistChecksums, DistManifest (+11 more)

### Community 15 - "Community 15"
Cohesion: 0.14
Nodes (15): build_show_payload(), build_show_payload_assembles_all_sections(), build_show_payload_degrades_gracefully_per_section(), map_email_change_error(), normalize_update_email(), opaque_email_change(), print_email_change_success(), render_progress_bar() (+7 more)

### Community 16 - "Community 16"
Cohesion: 0.19
Nodes (6): box_line(), OutputMode, strip_ansi(), table(), table_aligns_columns_by_widest_cell(), table_width_calculation_ignores_ansi_escapes()

### Community 17 - "Community 17"
Cohesion: 0.27
Nodes (7): build_plan_label(), build_plan_label_breaks_down_extra_when_it_exactly_accounts_for_the_total(), build_plan_label_never_fabricates_a_bonus_for_an_unreconciled_remainder(), build_plan_label_shows_the_servers_total_bytes_directly_never_double_counted(), build_plan_label_with_no_extra_shows_just_plan_and_total(), capitalise(), format_number()

### Community 18 - "Community 18"
Cohesion: 0.42
Nodes (8): build_plan_label(), build_plan_label_breaks_down_extra_when_it_exactly_accounts_for_the_total(), build_plan_label_never_fabricates_a_bonus_for_an_unreconciled_remainder(), build_plan_label_shows_the_servers_total_bytes_directly_never_double_counted(), build_plan_label_with_no_extra_shows_just_plan_and_total(), capitalise(), format_number(), run()

### Community 19 - "Community 19"
Cohesion: 0.25
Nodes (6): FileEventStatus, SessionInfo, SyncFileEvent, SyncStatus, TuiState, TuiView

### Community 20 - "Community 20"
Cohesion: 0.52
Nodes (6): decrypt_payload_matches_webcrypto(), ecdh_shared_secret_matches_webcrypto(), full_flow_ecdh_to_plaintext_via_core(), hex32(), hex_decode(), hkdf_aes_key_matches_webcrypto()

## Knowledge Gaps
- **62 isolated node(s):** `ThumbnailResult`, `OutputMode`, `DeviceInfo`, `DownloadStats`, `PendingDb` (+57 more)
  These have ≤1 connection - possible missing edges or undocumented components.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `is_quiet()` connect `Community 0` to `Community 1`, `Community 2`, `Community 3`, `Community 5`, `Community 6`, `Community 7`, `Community 9`, `Community 15`, `Community 16`, `Community 18`?**
  _High betweenness centrality (0.091) - this node is a cross-community bridge._
- **Why does `ApiClient` connect `Community 2` to `Community 8`, `Community 0`?**
  _High betweenness centrality (0.083) - this node is a cross-community bridge._
- **Why does `is_json()` connect `Community 0` to `Community 1`, `Community 3`, `Community 5`, `Community 6`, `Community 7`, `Community 8`, `Community 9`, `Community 10`, `Community 15`, `Community 16`, `Community 18`?**
  _High betweenness centrality (0.059) - this node is a cross-community bridge._
- **Are the 49 inferred relationships involving `is_json()` (e.g. with `list()` and `add()`) actually correct?**
  _`is_json()` has 49 INFERRED edges - model-reasoned connections that need verification._
- **Are the 46 inferred relationships involving `is_quiet()` (e.g. with `parse_response_typed()` and `list()`) actually correct?**
  _`is_quiet()` has 46 INFERRED edges - model-reasoned connections that need verification._
- **Are the 15 inferred relationships involving `run()` (e.g. with `.from_config()` and `uninstall_launchagent()`) actually correct?**
  _`run()` has 15 INFERRED edges - model-reasoned connections that need verification._
- **What connects `ThumbnailResult`, `OutputMode`, `DeviceInfo` to the rest of the system?**
  _62 weakly-connected nodes found - possible documentation gaps or missing edges._
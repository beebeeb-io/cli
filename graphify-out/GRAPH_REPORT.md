# Graph Report - cli-flow6-4  (2026-09-25)

## Corpus Check
- 61 files · ~106,129 words
- Verdict: corpus is large enough that graph structure adds value.

## Summary
- 960 nodes · 2564 edges · 22 communities detected
- Extraction: 71% EXTRACTED · 29% INFERRED · 0% AMBIGUOUS · INFERRED: 737 edges (avg confidence: 0.8)
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

## God Nodes (most connected - your core abstractions)
1. `ApiClient` - 82 edges
2. `parse_response()` - 59 edges
3. `is_json()` - 50 edges
4. `is_quiet()` - 47 edges
5. `run()` - 37 edges
6. `main()` - 34 edges
7. `load_master_key()` - 28 edges
8. `load_master_key()` - 27 edges
9. `run()` - 22 edges
10. `BeebeebFs` - 21 edges

## Surprising Connections (you probably didn't know these)
- `browser_login()` --calls--> `text()`  [INFERRED]
  src/commands/login.rs → tests/non_interactive.rs
- `oneRun()` --calls--> `String`  [INFERRED]
  scripts/prod-bots/auth-bot.mjs → src/api.rs
- `set_api_url_override()` --calls--> `main()`  [INFERRED]
  src/config.rs → src/main.rs
- `expected_ciphertext_for()` --calls--> `run()`  [INFERRED]
  src/upload.rs → src/commands/sync.rs
- `main()` --calls--> `code()`  [INFERRED]
  src/main.rs → src/exit.rs

## Communities

### Community 0 - "Community 0"
Cohesion: 0.07
Nodes (86): classify_new_local_is_not_a_delete(), classify_prior_synced_modified_is_readd(), classify_prior_synced_unchanged_is_remote_delete(), classify_remote_absent(), compute_file_hash(), create_folder(), do_download(), do_upload() (+78 more)

### Community 1 - "Community 1"
Cohesion: 0.05
Nodes (76): build_show_payload(), build_show_payload_assembles_all_sections(), build_show_payload_degrades_gracefully_per_section(), map_email_change_error(), normalize_update_email(), opaque_email_change(), print_email_change_success(), render_progress_bar() (+68 more)

### Community 2 - "Community 2"
Cohesion: 0.09
Nodes (4): ApiClient, pace_if_needed(), parse_response(), parse_response_typed()

### Community 3 - "Community 3"
Cohesion: 0.04
Nodes (52): upload_file_to(), AccountCmd, AddonsAction, BillingAction, build_help_text(), Cli, Commands, help_screen_lists_every_top_level_subcommand() (+44 more)

### Community 4 - "Community 4"
Cohesion: 0.05
Nodes (44): add_json_body_carries_the_url_and_a_note_never_a_token(), AddAction, confirm_remove(), enrollment_url_bracketed_ipv6_loopback_is_local_and_port_is_stripped(), enrollment_url_does_not_treat_a_127_0_0_1_labeled_domain_as_local(), enrollment_url_does_not_treat_a_localhost_labeled_domain_as_local(), enrollment_url_for_local_api_points_at_the_local_dev_web_app(), enrollment_url_for_prod_api_points_at_the_prod_web_app() (+36 more)

### Community 5 - "Community 5"
Cohesion: 0.05
Nodes (38): addons(), aggregate_by_region(), aggregate_by_region_defaults_missing_storage_location_to_europe_falkenstein(), aggregate_by_region_empty_input_is_empty_output(), aggregate_by_region_splits_multiple_regions(), aggregate_by_region_sums_bytes_per_region_and_skips_folders(), capitalise(), format_date_human() (+30 more)

### Community 6 - "Community 6"
Cohesion: 0.06
Nodes (40): render_otpauth(), renders_a_typical_totp_uri(), renders_empty_for_garbage_that_cannot_encode(), classify_disable_error(), classify_disable_error_passes_through_unrecognized_errors(), classify_disable_error_reports_ambiguous_on_bare_unauthorized(), classify_disable_error_reports_not_enabled_when_never_set_up(), classify_disable_error_reports_not_enabled_when_row_disabled() (+32 more)

### Community 7 - "Community 7"
Cohesion: 0.08
Nodes (50): decrypt_name(), collect_zip_entries(), looks_like_id_prefix(), pull_folder(), pull_folder_inner(), pull_single_file(), resolve_as_path(), resolve_request_key() (+42 more)

### Community 8 - "Community 8"
Cohesion: 0.07
Nodes (47): acquire_confirm_token(), acquire_confirmed_password(), confirm_password_session_too_old_survives_a_message_that_does_not_mention_the_code(), ConfirmedPassword, map_confirm_error(), Match, Node, print_results() (+39 more)

### Community 9 - "Community 9"
Cohesion: 0.07
Nodes (30): confirm_revoke_all(), extract_sessions_json(), guard_not_current(), guard_not_current_allows_a_non_current_session(), guard_not_current_refuses_the_current_session(), json_mode_extracts_the_raw_sessions_array_unmodified(), marker_cell(), no_yes_and_not_rich_refuses_instead_of_silently_proceeding() (+22 more)

### Community 10 - "Community 10"
Cohesion: 0.09
Nodes (43): print_created(), run(), run_recursive(), split_parent_and_leaf(), CachedDir, check_lock(), child_href(), DavState (+35 more)

### Community 11 - "Community 11"
Cohesion: 0.08
Nodes (31): run(), browser_login(), print_browser_block(), print_headless_block(), run(), spawn_countdown(), run(), run() (+23 more)

### Community 12 - "Community 12"
Cohesion: 0.11
Nodes (14): BeebeebFs, CachedDir, InodeEntry, PendingCreate, run(), unmount(), AtomicFile, buffered_fallback() (+6 more)

### Community 13 - "Community 13"
Cohesion: 0.15
Nodes (21): b64std(), b64url(), build_link(), create(), decode_any_b64(), generate_request_keypair(), keypair_wrap_unwrap_roundtrip_matches_create_then_list(), link_assembly_roundtrips_through_parse() (+13 more)

### Community 14 - "Community 14"
Cohesion: 0.13
Nodes (18): check_and_update(), cooldown_elapsed(), ct_eq_ignore_case(), current_target(), DistArtifact, DistChecksums, DistManifest, extract_binary_from_tarball() (+10 more)

### Community 15 - "Community 15"
Cohesion: 0.22
Nodes (19): decrypt_file_chunks(), decrypt_json_chunks(), decrypt_name(), decrypt_name_plaintext_passthrough(), decrypt_name_with_key(), decrypt_names(), decrypt_names_batch_matches_single(), decrypt_raw_chunks() (+11 more)

### Community 16 - "Community 16"
Cohesion: 0.19
Nodes (6): box_line(), OutputMode, strip_ansi(), table(), table_aligns_columns_by_widest_cell(), table_width_calculation_ignores_ansi_escapes()

### Community 17 - "Community 17"
Cohesion: 0.21
Nodes (9): EnvSnapshot, explicit_override_always_wins(), explicit_override_zero_is_not_a_yes(), is_headless_with(), snap(), ssh_tty_alone_counts_as_ssh(), ssh_with_x_forwarding_is_not_headless(), ssh_without_display_is_headless() (+1 more)

### Community 18 - "Community 18"
Cohesion: 0.27
Nodes (7): build_plan_label(), build_plan_label_breaks_down_extra_when_it_exactly_accounts_for_the_total(), build_plan_label_never_fabricates_a_bonus_for_an_unreconciled_remainder(), build_plan_label_shows_the_servers_total_bytes_directly_never_double_counted(), build_plan_label_with_no_extra_shows_just_plan_and_total(), capitalise(), format_number()

### Community 19 - "Community 19"
Cohesion: 0.42
Nodes (8): build_plan_label(), build_plan_label_breaks_down_extra_when_it_exactly_accounts_for_the_total(), build_plan_label_never_fabricates_a_bonus_for_an_unreconciled_remainder(), build_plan_label_shows_the_servers_total_bytes_directly_never_double_counted(), build_plan_label_with_no_extra_shows_just_plan_and_total(), capitalise(), format_number(), run()

### Community 20 - "Community 20"
Cohesion: 0.25
Nodes (6): FileEventStatus, SessionInfo, SyncFileEvent, SyncStatus, TuiState, TuiView

### Community 21 - "Community 21"
Cohesion: 0.52
Nodes (6): decrypt_payload_matches_webcrypto(), ecdh_shared_secret_matches_webcrypto(), full_flow_ecdh_to_plaintext_via_core(), hex32(), hex_decode(), hkdf_aes_key_matches_webcrypto()

## Knowledge Gaps
- **63 isolated node(s):** `Calls`, `ThumbnailResult`, `OutputMode`, `DeviceInfo`, `DownloadStats` (+58 more)
  These have ≤1 connection - possible missing edges or undocumented components.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `is_quiet()` connect `Community 1` to `Community 0`, `Community 2`, `Community 7`, `Community 8`, `Community 13`, `Community 16`, `Community 19`?**
  _High betweenness centrality (0.091) - this node is a cross-community bridge._
- **Why does `ApiClient` connect `Community 2` to `Community 8`, `Community 1`?**
  _High betweenness centrality (0.083) - this node is a cross-community bridge._
- **Why does `is_json()` connect `Community 1` to `Community 0`, `Community 5`, `Community 7`, `Community 8`, `Community 10`, `Community 11`, `Community 13`, `Community 16`, `Community 19`?**
  _High betweenness centrality (0.058) - this node is a cross-community bridge._
- **Are the 49 inferred relationships involving `is_json()` (e.g. with `list()` and `add()`) actually correct?**
  _`is_json()` has 49 INFERRED edges - model-reasoned connections that need verification._
- **Are the 46 inferred relationships involving `is_quiet()` (e.g. with `parse_response_typed()` and `list()`) actually correct?**
  _`is_quiet()` has 46 INFERRED edges - model-reasoned connections that need verification._
- **Are the 16 inferred relationships involving `run()` (e.g. with `.from_config()` and `uninstall_launchagent()`) actually correct?**
  _`run()` has 16 INFERRED edges - model-reasoned connections that need verification._
- **What connects `Calls`, `ThumbnailResult`, `OutputMode` to the rest of the system?**
  _63 weakly-connected nodes found - possible documentation gaps or missing edges._
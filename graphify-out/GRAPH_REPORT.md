# Graph Report - cli-1552  (2026-09-25)

## Corpus Check
- 61 files · ~107,971 words
- Verdict: corpus is large enough that graph structure adds value.

## Summary
- 974 nodes · 2703 edges · 23 communities detected
- Extraction: 69% EXTRACTED · 31% INFERRED · 0% AMBIGUOUS · INFERRED: 833 edges (avg confidence: 0.8)
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

## God Nodes (most connected - your core abstractions)
1. `err()` - 94 edges
2. `ApiClient` - 86 edges
3. `parse_response()` - 59 edges
4. `is_json()` - 50 edges
5. `is_quiet()` - 47 edges
6. `run()` - 38 edges
7. `main()` - 34 edges
8. `load_master_key()` - 29 edges
9. `spawn()` - 28 edges
10. `load_master_key()` - 28 edges

## Surprising Connections (you probably didn't know these)
- `oneRun()` --calls--> `String`  [INFERRED]
  scripts/prod-bots/auth-bot.mjs → src/api.rs
- `expected_ciphertext_for()` --calls--> `run()`  [INFERRED]
  src/upload.rs → src/commands/sync.rs
- `mark_uploaded()` --calls--> `push_single_file()`  [INFERRED]
  src/loopback.rs → src/commands/push.rs
- `generate_from_file()` --calls--> `push_single_file()`  [INFERRED]
  src/thumbnail.rs → src/commands/push.rs
- `generate_large_from_file()` --calls--> `push_single_file()`  [INFERRED]
  src/thumbnail.rs → src/commands/push.rs

## Communities

### Community 0 - "Community 0"
Cohesion: 0.05
Nodes (80): build_show_payload(), build_show_payload_assembles_all_sections(), build_show_payload_degrades_gracefully_per_section(), map_email_change_error(), normalize_update_email(), opaque_email_change(), print_email_change_success(), render_progress_bar() (+72 more)

### Community 1 - "Community 1"
Cohesion: 0.08
Nodes (4): ApiClient, pace_if_needed(), parse_response(), parse_response_typed()

### Community 2 - "Community 2"
Cohesion: 0.07
Nodes (75): classify_new_local_is_not_a_delete(), classify_prior_synced_modified_is_readd(), classify_prior_synced_unchanged_is_remote_delete(), classify_remote_absent(), compute_file_hash(), create_folder(), do_download(), do_upload() (+67 more)

### Community 3 - "Community 3"
Cohesion: 0.06
Nodes (60): decrypt_listing(), DecryptedFile, LsOpts, print_header(), print_json(), print_recursive(), print_row(), run() (+52 more)

### Community 4 - "Community 4"
Cohesion: 0.04
Nodes (52): upload_file_to(), AccountCmd, AddonsAction, BillingAction, build_help_text(), Cli, Commands, help_screen_lists_every_top_level_subcommand() (+44 more)

### Community 5 - "Community 5"
Cohesion: 0.05
Nodes (43): add_json_body_carries_the_url_and_a_note_never_a_token(), AddAction, confirm_remove(), enrollment_url_bracketed_ipv6_loopback_is_local_and_port_is_stripped(), enrollment_url_does_not_treat_a_127_0_0_1_labeled_domain_as_local(), enrollment_url_does_not_treat_a_localhost_labeled_domain_as_local(), enrollment_url_for_local_api_points_at_the_local_dev_web_app(), enrollment_url_for_prod_api_points_at_the_prod_web_app() (+35 more)

### Community 6 - "Community 6"
Cohesion: 0.09
Nodes (56): confirm_password_session_too_old_survives_a_message_that_does_not_mention_the_code(), Match, Node, print_results(), run(), walk_mem(), walk_mem_anchored_subtree_only(), walk_mem_dfs_order_paths_and_walked() (+48 more)

### Community 7 - "Community 7"
Cohesion: 0.05
Nodes (37): addons(), aggregate_by_region(), aggregate_by_region_defaults_missing_storage_location_to_europe_falkenstein(), aggregate_by_region_empty_input_is_empty_output(), aggregate_by_region_splits_multiple_regions(), aggregate_by_region_sums_bytes_per_region_and_skips_folders(), capitalise(), format_date_human() (+29 more)

### Community 8 - "Community 8"
Cohesion: 0.06
Nodes (37): render_otpauth(), renders_a_typical_totp_uri(), renders_empty_for_garbage_that_cannot_encode(), classify_disable_error(), classify_disable_error_passes_through_unrecognized_errors(), classify_disable_error_reports_ambiguous_on_bare_unauthorized(), classify_disable_error_reports_not_enabled_when_never_set_up(), classify_disable_error_reports_not_enabled_when_row_disabled() (+29 more)

### Community 9 - "Community 9"
Cohesion: 0.07
Nodes (29): confirm_revoke_all(), extract_sessions_json(), guard_not_current_allows_a_non_current_session(), guard_not_current_refuses_the_current_session(), json_mode_extracts_the_raw_sessions_array_unmodified(), marker_cell(), no_yes_and_not_rich_refuses_instead_of_silently_proceeding(), parse_sessions() (+21 more)

### Community 10 - "Community 10"
Cohesion: 0.09
Nodes (41): acquire_confirm_token(), acquire_confirmed_password(), ConfirmedPassword, map_confirm_error(), confirm(), count(), permanent_delete_flow(), run() (+33 more)

### Community 11 - "Community 11"
Cohesion: 0.12
Nodes (12): BeebeebFs, CachedDir, InodeEntry, PendingCreate, AtomicFile, buffered_fallback(), DownloadStats, drain_rest() (+4 more)

### Community 12 - "Community 12"
Cohesion: 0.1
Nodes (28): collect_all_files(), decrypt_chunks_with_binary_key(), detect_key_derivation(), encrypt_chunks_with_string_key(), encrypt_name_with_string_key(), KeyDerivation, repair_file(), repair_folder() (+20 more)

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
Cohesion: 0.22
Nodes (11): finish_registration(), prompt_line(), prompt_password_confirmed(), run(), run_code_step(), show_recovery_phrase(), is_legacy_fallback_error(), is_ticket_invalid_error() (+3 more)

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

### Community 22 - "Community 22"
Cohesion: 0.83
Nodes (3): mark_uploaded(), registry(), was_recently_uploaded()

## Knowledge Gaps
- **62 isolated node(s):** `ThumbnailResult`, `OutputMode`, `DeviceInfo`, `DownloadStats`, `PendingDb` (+57 more)
  These have ≤1 connection - possible missing edges or undocumented components.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `err()` connect `Community 0` to `Community 1`, `Community 2`, `Community 3`, `Community 4`, `Community 5`, `Community 6`, `Community 7`, `Community 8`, `Community 9`, `Community 10`, `Community 11`, `Community 13`, `Community 14`, `Community 15`, `Community 16`?**
  _High betweenness centrality (0.204) - this node is a cross-community bridge._
- **Why does `is_quiet()` connect `Community 0` to `Community 1`, `Community 2`, `Community 3`, `Community 6`, `Community 10`, `Community 12`, `Community 13`, `Community 19`?**
  _High betweenness centrality (0.063) - this node is a cross-community bridge._
- **Why does `ApiClient` connect `Community 1` to `Community 0`, `Community 6`?**
  _High betweenness centrality (0.060) - this node is a cross-community bridge._
- **Are the 93 inferred relationships involving `err()` (e.g. with `stream_download_decrypt()` and `stream_raw()`) actually correct?**
  _`err()` has 93 INFERRED edges - model-reasoned connections that need verification._
- **Are the 49 inferred relationships involving `is_json()` (e.g. with `list()` and `add()`) actually correct?**
  _`is_json()` has 49 INFERRED edges - model-reasoned connections that need verification._
- **Are the 46 inferred relationships involving `is_quiet()` (e.g. with `parse_response_typed()` and `list()`) actually correct?**
  _`is_quiet()` has 46 INFERRED edges - model-reasoned connections that need verification._
- **What connects `ThumbnailResult`, `OutputMode`, `DeviceInfo` to the rest of the system?**
  _62 weakly-connected nodes found - possible documentation gaps or missing edges._
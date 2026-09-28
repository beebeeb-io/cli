# Graph Report - cli-1037  (2026-09-28)

## Corpus Check
- 70 files · ~123,325 words
- Verdict: corpus is large enough that graph structure adds value.

## Summary
- 1164 nodes · 3164 edges · 29 communities detected
- Extraction: 71% EXTRACTED · 29% INFERRED · 0% AMBIGUOUS · INFERRED: 922 edges (avg confidence: 0.8)
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
- [[_COMMUNITY_Community 24|Community 24]]
- [[_COMMUNITY_Community 25|Community 25]]
- [[_COMMUNITY_Community 26|Community 26]]
- [[_COMMUNITY_Community 27|Community 27]]
- [[_COMMUNITY_Community 28|Community 28]]

## God Nodes (most connected - your core abstractions)
1. `ApiClient` - 85 edges
2. `parse_response()` - 55 edges
3. `is_json()` - 51 edges
4. `is_quiet()` - 49 edges
5. `run()` - 38 edges
6. `main()` - 36 edges
7. `spawn()` - 29 edges
8. `load_master_key()` - 29 edges
9. `load_master_key()` - 28 edges
10. `run()` - 23 edges

## Surprising Connections (you probably didn't know these)
- `start_mock()` --calls--> `spawn()`  [INFERRED]
  tests/zero_byte_files.rs → src/upload.rs
- `spawn_mock()` --calls--> `spawn()`  [INFERRED]
  tests/pull_overwrite.rs → src/upload.rs
- `start_mock()` --calls--> `spawn()`  [INFERRED]
  tests/non_interactive.rs → src/upload.rs
- `spawn_mock()` --calls--> `spawn()`  [INFERRED]
  tests/share_resolves_like_pull.rs → src/upload.rs
- `spawn_unauthorized_mock()` --calls--> `spawn()`  [INFERRED]
  tests/auth_errors.rs → src/upload.rs

## Communities

### Community 0 - "Community 0"
Cohesion: 0.04
Nodes (118): build_show_payload(), build_show_payload_assembles_all_sections(), build_show_payload_degrades_gracefully_per_section(), map_email_change_error(), normalize_update_email(), opaque_email_change(), print_email_change_success(), render_progress_bar() (+110 more)

### Community 1 - "Community 1"
Cohesion: 0.04
Nodes (120): capitalise(), format_number(), write_private_temp_pdf_refuses_to_follow_an_existing_symlink_at_the_target_path(), dir_total_size(), prompt_conflict(), render_otpauth(), renders_a_typical_totp_uri(), renders_empty_for_garbage_that_cannot_encode() (+112 more)

### Community 2 - "Community 2"
Cohesion: 0.06
Nodes (68): live(), peak(), reset_peak(), clear(), db_path(), key(), load(), PendingDb (+60 more)

### Community 3 - "Community 3"
Cohesion: 0.08
Nodes (4): ApiClient, pace_if_needed(), parse_response(), parse_response_typed()

### Community 4 - "Community 4"
Cohesion: 0.05
Nodes (58): run(), browser_login(), print_browser_block(), print_headless_block(), run(), spawn_countdown(), run(), collect_all_files() (+50 more)

### Community 5 - "Community 5"
Cohesion: 0.05
Nodes (46): add_json_body_carries_the_url_and_a_note_never_a_token(), AddAction, classify_list_error(), confirm_remove(), enrollment_url_bracketed_ipv6_loopback_is_local_and_port_is_stripped(), enrollment_url_does_not_treat_a_127_0_0_1_labeled_domain_as_local(), enrollment_url_does_not_treat_a_localhost_labeled_domain_as_local(), enrollment_url_for_local_api_points_at_the_local_dev_web_app() (+38 more)

### Community 6 - "Community 6"
Cohesion: 0.05
Nodes (34): addons(), aggregate_by_region(), aggregate_by_region_defaults_missing_storage_location_to_europe_falkenstein(), aggregate_by_region_empty_input_is_empty_output(), aggregate_by_region_splits_multiple_regions(), aggregate_by_region_sums_bytes_per_region_and_skips_folders(), format_date_human(), format_invoice_amount() (+26 more)

### Community 7 - "Community 7"
Cohesion: 0.07
Nodes (36): classify_disable_error(), classify_disable_error_passes_through_unrecognized_errors(), classify_disable_error_reports_ambiguous_on_bare_unauthorized(), classify_disable_error_reports_not_enabled_when_never_set_up(), classify_disable_error_reports_not_enabled_when_row_disabled(), classify_enable_error(), classify_enable_error_passes_through_unrecognized_errors(), classify_enable_error_reports_already_enabled() (+28 more)

### Community 8 - "Community 8"
Cohesion: 0.08
Nodes (47): acquire_confirm_token(), acquire_confirmed_password(), confirm_password_session_too_old_survives_a_message_that_does_not_mention_the_code(), ConfirmedPassword, map_confirm_error(), a_successful_init_never_fetches_the_subscription(), backoff(), build_client() (+39 more)

### Community 9 - "Community 9"
Cohesion: 0.08
Nodes (31): classify_list_error(), extract_sessions_json(), guard_not_current(), guard_not_current_allows_a_non_current_session(), guard_not_current_refuses_the_current_session(), json_mode_extracts_the_raw_sessions_array_unmodified(), list(), no_yes_and_not_rich_refuses_instead_of_silently_proceeding() (+23 more)

### Community 10 - "Community 10"
Cohesion: 0.09
Nodes (43): print_created(), run(), run_recursive(), split_parent_and_leaf(), CachedDir, check_lock(), child_href(), DavState (+35 more)

### Community 11 - "Community 11"
Cohesion: 0.14
Nodes (24): decrypt_file_chunks(), decrypt_name(), decrypt_name_plaintext_passthrough(), decrypt_name_with_key(), decrypt_names(), decrypt_names_batch_matches_single(), json_blob_legacy_format_detected_and_decrypted(), json_blob_with_binary_uuid_key() (+16 more)

### Community 12 - "Community 12"
Cohesion: 0.13
Nodes (8): BeebeebFs, CachedDir, InodeEntry, PendingCreate, run(), unmount(), generate_large_from_file(), maybe_upload_thumbnail()

### Community 13 - "Community 13"
Cohesion: 0.15
Nodes (19): b64std(), b64url(), build_link(), decode_any_b64(), generate_request_keypair(), keypair_wrap_unwrap_roundtrip_matches_create_then_list(), link_assembly_roundtrips_through_parse(), parse_expiry_secs() (+11 more)

### Community 14 - "Community 14"
Cohesion: 0.09
Nodes (5): AccountState, at(), lapsed_notice_names_the_deletion_date_and_points_at_billing(), notices_follow_the_web_app_base_they_are_given(), slug_and_json_date_round_trip_the_wire_values()

### Community 15 - "Community 15"
Cohesion: 0.09
Nodes (7): plan_signup(), signup_url(), signup_url_for_the_default_api_is_the_production_signup(), SignupAction, host_of(), resolve_web_app_base(), web_app_base_from_api()

### Community 16 - "Community 16"
Cohesion: 0.12
Nodes (18): AccountCmd, AddonsAction, all_help_text(), BillingAction, build_help_text(), Cli, Commands, help_footer_text() (+10 more)

### Community 17 - "Community 17"
Cohesion: 0.16
Nodes (16): arr32(), build_share_material(), draw_picker(), hex(), parse_hours(), recover_share_link(), run_picker(), share_link() (+8 more)

### Community 18 - "Community 18"
Cohesion: 0.21
Nodes (9): EnvSnapshot, explicit_override_always_wins(), explicit_override_zero_is_not_a_yes(), is_headless_with(), snap(), ssh_tty_alone_counts_as_ssh(), ssh_with_x_forwarding_is_not_headless(), ssh_without_display_is_headless() (+1 more)

### Community 19 - "Community 19"
Cohesion: 0.33
Nodes (11): bb(), pull_help_documents_force(), pull_into_empty_dir_writes_remote_content(), pull_refuses_to_overwrite_existing_file_without_force(), pull_refuses_to_overwrite_existing_output_flag_target(), pull_with_force_overwrites_existing_file(), remote_ciphertext(), scratch() (+3 more)

### Community 20 - "Community 20"
Cohesion: 0.31
Nodes (12): check(), clap_accepts(), code_spans(), expand(), extract(), flag_defined_anywhere(), Invocation, is_flag() (+4 more)

### Community 21 - "Community 21"
Cohesion: 0.58
Nodes (11): bb(), logged_out_whoami_exits_nonzero_with_a_login_hint(), revoked_session_ls_tells_the_user_to_run_bb_login(), revoked_session_quota_tells_the_user_to_run_bb_login(), revoked_session_status_fails_too(), revoked_session_whoami_fails_and_never_shows_placeholder_plan_data(), scratch_home(), spawn_unauthorized_mock() (+3 more)

### Community 22 - "Community 22"
Cohesion: 0.55
Nodes (10): bb(), empty_file_uploads_as_one_aead_chunk_when_the_server_accepts_it(), folder_push_carries_on_past_a_rejected_file_and_names_it(), folder_push_json_lists_the_failed_file(), make_folder2(), Mock, scratch_home(), start_mock() (+2 more)

### Community 23 - "Community 23"
Cohesion: 0.24
Nodes (5): bb(), Recorded, scratch_home(), share_accepts_path_and_short_id_and_resolves_like_pull(), spawn_mock()

### Community 24 - "Community 24"
Cohesion: 0.31
Nodes (6): build_plan_label(), build_plan_label_breaks_down_extra_when_it_exactly_accounts_for_the_total(), build_plan_label_never_fabricates_a_bonus_for_an_unreconciled_remainder(), build_plan_label_shows_the_servers_total_bytes_directly_never_double_counted(), build_plan_label_with_no_extra_shows_just_plan_and_total(), capitalise()

### Community 25 - "Community 25"
Cohesion: 0.25
Nodes (6): FileEventStatus, SessionInfo, SyncFileEvent, SyncStatus, TuiState, TuiView

### Community 26 - "Community 26"
Cohesion: 0.52
Nodes (6): decrypt_payload_matches_webcrypto(), ecdh_shared_secret_matches_webcrypto(), full_flow_ecdh_to_plaintext_via_core(), hex32(), hex_decode(), hkdf_aes_key_matches_webcrypto()

### Community 27 - "Community 27"
Cohesion: 0.52
Nodes (6): build_plan_label(), build_plan_label_breaks_down_extra_when_it_exactly_accounts_for_the_total(), build_plan_label_never_fabricates_a_bonus_for_an_unreconciled_remainder(), build_plan_label_shows_the_servers_total_bytes_directly_never_double_counted(), build_plan_label_with_no_extra_shows_just_plan_and_total(), capitalise()

### Community 28 - "Community 28"
Cohesion: 0.83
Nodes (3): mark_uploaded(), registry(), was_recently_uploaded()

## Knowledge Gaps
- **71 isolated node(s):** `Mock`, `Calls`, `Recorded`, `CacheEntry`, `ResolvedPath` (+66 more)
  These have ≤1 connection - possible missing edges or undocumented components.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `is_quiet()` connect `Community 0` to `Community 1`, `Community 2`, `Community 3`, `Community 4`, `Community 5`, `Community 9`?**
  _High betweenness centrality (0.112) - this node is a cross-community bridge._
- **Why does `ApiClient` connect `Community 3` to `Community 8`, `Community 0`, `Community 1`?**
  _High betweenness centrality (0.073) - this node is a cross-community bridge._
- **Why does `is_json()` connect `Community 0` to `Community 1`, `Community 4`, `Community 5`, `Community 9`, `Community 10`?**
  _High betweenness centrality (0.052) - this node is a cross-community bridge._
- **Are the 50 inferred relationships involving `is_json()` (e.g. with `run()` and `move_bulk()`) actually correct?**
  _`is_json()` has 50 INFERRED edges - model-reasoned connections that need verification._
- **Are the 48 inferred relationships involving `is_quiet()` (e.g. with `parse_response_typed()` and `stream_encrypt_upload()`) actually correct?**
  _`is_quiet()` has 48 INFERRED edges - model-reasoned connections that need verification._
- **Are the 17 inferred relationships involving `run()` (e.g. with `.from_config()` and `uninstall_launchagent()`) actually correct?**
  _`run()` has 17 INFERRED edges - model-reasoned connections that need verification._
- **What connects `Mock`, `Calls`, `Recorded` to the rest of the system?**
  _71 weakly-connected nodes found - possible documentation gaps or missing edges._
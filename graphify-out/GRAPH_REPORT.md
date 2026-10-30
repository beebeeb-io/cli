# Graph Report - oc-2285cl-cli  (2026-10-30)

## Corpus Check
- 115 files · ~248,181 words
- Verdict: corpus is large enough that graph structure adds value.

## Summary
- 2580 nodes · 8201 edges · 44 communities detected
- Extraction: 70% EXTRACTED · 30% INFERRED · 0% AMBIGUOUS · INFERRED: 2448 edges (avg confidence: 0.8)
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
- [[_COMMUNITY_Community 29|Community 29]]
- [[_COMMUNITY_Community 30|Community 30]]
- [[_COMMUNITY_Community 31|Community 31]]
- [[_COMMUNITY_Community 32|Community 32]]
- [[_COMMUNITY_Community 33|Community 33]]
- [[_COMMUNITY_Community 34|Community 34]]
- [[_COMMUNITY_Community 35|Community 35]]
- [[_COMMUNITY_Community 36|Community 36]]
- [[_COMMUNITY_Community 37|Community 37]]
- [[_COMMUNITY_Community 38|Community 38]]
- [[_COMMUNITY_Community 39|Community 39]]
- [[_COMMUNITY_Community 40|Community 40]]
- [[_COMMUNITY_Community 41|Community 41]]
- [[_COMMUNITY_Community 42|Community 42]]
- [[_COMMUNITY_Community 44|Community 44]]

## God Nodes (most connected - your core abstractions)
1. `ok()` - 266 edges
2. `err()` - 161 edges
3. `ApiClient` - 101 edges
4. `spawn()` - 74 edges
5. `run()` - 66 edges
6. `parse_response()` - 58 edges
7. `is_json()` - 57 edges
8. `is_quiet()` - 56 edges
9. `serve()` - 53 edges
10. `main()` - 46 edges

## Surprising Connections (you probably didn't know these)
- `spawn_storage()` --calls--> `spawn()`  [INFERRED]
  tests/direct_download.rs → src/commands/confirm.rs
- `setup_with()` --calls--> `spawn()`  [INFERRED]
  tests/direct_download.rs → src/commands/confirm.rs
- `setup_with()` --calls--> `get()`  [INFERRED]
  tests/direct_download.rs → src/commands/region.rs
- `setup_with()` --calls--> `serve()`  [INFERRED]
  tests/direct_download.rs → src/download.rs
- `sync_until_watching()` --calls--> `spawn()`  [INFERRED]
  tests/direct_download.rs → src/commands/confirm.rs

## Communities

### Community 0 - "Community 0"
Cohesion: 0.02
Nodes (307): build_show_payload(), build_show_payload_assembles_all_sections(), build_show_payload_degrades_gracefully_per_section(), build_show_payload_drops_legacy_city_keys(), map_email_change_error(), normalize_update_email(), opaque_email_change(), print_email_change_success() (+299 more)

### Community 1 - "Community 1"
Cohesion: 0.02
Nodes (257): a_connection_that_breaks_mid_body_is_tried_again(), a_login_that_is_not_valid_is_told_apart_from_a_server_nobody_could_ask_and_is_asked_about_once(), a_refusal_is_remembered_for_that_login_and_for_the_hold_only(), a_servers_no_and_nobody_answering_are_told_apart_and_both_are_cached(), a_storage_put_is_still_bounded_by_its_total_timeout(), a_storage_put_that_takes_longer_than_the_read_timeout_to_be_answered_still_succeeds(), an_attempt_that_hears_nothing_fails_after_the_idle_limit_and_is_tried_again(), link_to() (+249 more)

### Community 2 - "Community 2"
Cohesion: 0.05
Nodes (97): record(), root_is_unreconciled_only_when_bound_to_another_folder(), live(), peak(), reset_peak(), clear(), db_path(), key() (+89 more)

### Community 3 - "Community 3"
Cohesion: 0.05
Nodes (89): SortField, check(), customize(), main(), already_current_reports_success_after_a_fresh_request(), archive(), brew(), check_and_update() (+81 more)

### Community 4 - "Community 4"
Cohesion: 0.04
Nodes (78): now_ms(), BeebeebFs, PullFailure, a_discarded_partial_file_takes_its_bytes_back_off_the_run_wide_counter(), a_stream_cut_before_its_last_frame_is_unreadable_and_takes_its_bytes_back(), changed(), DirectResult, discard() (+70 more)

### Community 5 - "Community 5"
Cohesion: 0.05
Nodes (92): folder(), webdav_parent_target_file(), Fixture, folder_key(), handle(), item_key(), master(), proof_row() (+84 more)

### Community 6 - "Community 6"
Cohesion: 0.06
Nodes (12): ApiClient, backoff(), confirmation_token_from(), format_request_error(), is_transient_transport_error(), listing_cap_reached(), onboarding_os(), pace_if_needed() (+4 more)

### Community 7 - "Community 7"
Cohesion: 0.04
Nodes (53): a_clock_that_goes_backwards_does_not_keep_a_discovery_parked(), a_discovery_that_gave_up_asking_asks_again_once_the_trust_period_has_passed(), a_given_up_direct_upload_holds_for_the_trust_period_on_the_monotonic_clock(), a_held_file_with_no_retry_after_goes_straight_to_the_api_but_a_stated_wait_is_kept(), a_hold_is_counted_from_when_the_direct_upload_was_given_up_not_from_when_it_is_asked(), a_refusal_says_what_it_was_in_the_words_the_cli_uses_for_it(), a_refusal_the_server_said_no_and_asking_failed_are_one_state_not_three_flags(), a_server_error_with_a_retry_after_is_waited_for_as_told() (+45 more)

### Community 8 - "Community 8"
Cohesion: 0.04
Nodes (38): a_pre_account_document_has_no_account_summary(), absent_delete_capability_fails_closed_with_no_invented_reason(), account_lapsed_takes_the_date_from_the_subscription(), AccountState, AccountSummary, at(), contract_dir(), Denied (+30 more)

### Community 9 - "Community 9"
Cohesion: 0.14
Nodes (58): a_bare_404_on_the_grant_route_sends_the_whole_file_through_the_api(), a_busy_commit_waits_the_stated_time_and_commits_the_same_chunk_again(), a_chunk_that_is_waiting_for_a_rate_limit_stops_when_the_run_is_interrupted(), a_chunk_that_was_not_stored_is_granted_again(), a_commit_held_for_longer_than_the_failure_cap_is_waited_out_not_given_up(), a_commit_that_finds_the_session_gone_starts_the_file_again_too(), a_dead_session_on_a_direct_commit_says_to_log_in_again_like_the_proxy_route(), a_direct_session_given_up_after_some_chunks_took_their_progress_back() (+50 more)

### Community 10 - "Community 10"
Cohesion: 0.09
Nodes (62): a_chunk_storage_never_serves_goes_to_the_pinned_api_route(), a_file_another_device_is_still_uploading_goes_straight_to_the_api(), a_file_held_by_another_operation_is_waited_for_and_stays_direct(), a_file_whose_first_chunk_cannot_be_opened_is_left_to_the_api_paths_legacy_readers(), a_folder_pull_does_not_swallow_a_failure_that_is_not_a_version_change(), a_folder_pull_reads_one_changed_file_again_and_carries_on(), a_refusal_to_serve_this_file_directly_falls_back_to_the_api(), a_storage_redirect_is_never_followed() (+54 more)

### Community 11 - "Community 11"
Cohesion: 0.06
Nodes (56): b64std(), b64url(), build_link(), decode_any_b64(), generate_request_keypair(), keypair_wrap_unwrap_roundtrip_matches_create_then_list(), link_assembly_roundtrips_through_parse(), parse_request_link() (+48 more)

### Community 12 - "Community 12"
Cohesion: 0.06
Nodes (52): a_config_dir_that_is_not_the_services_is_refused(), a_service_definition_carries_the_transport_it_was_installed_with(), account_home(), between(), check_config_dir(), check_home(), current_templates_are_not_migrated(), daemon_args() (+44 more)

### Community 13 - "Community 13"
Cohesion: 0.05
Nodes (38): add_json_body_carries_the_url_and_a_note_never_a_token(), AddAction, enrollment_url_bracketed_ipv6_loopback_is_local_and_port_is_stripped(), enrollment_url_does_not_treat_a_127_0_0_1_labeled_domain_as_local(), enrollment_url_does_not_treat_a_localhost_labeled_domain_as_local(), enrollment_url_for_local_api_points_at_the_local_dev_web_app(), enrollment_url_for_prod_api_points_at_the_prod_web_app(), enrollment_url_for_unrecognized_host_stays_on_that_hosts_origin() (+30 more)

### Community 14 - "Community 14"
Cohesion: 0.05
Nodes (37): aggregate_by_region(), aggregate_by_region_empty_input_is_empty_output(), aggregate_by_region_falls_back_to_europe_never_a_city(), aggregate_by_region_splits_countries_but_merges_two_datacenters_of_one_country(), aggregate_by_region_sums_bytes_per_country_and_skips_folders(), at(), format_date_human(), format_invoice_amount() (+29 more)

### Community 15 - "Community 15"
Cohesion: 0.15
Nodes (28): explicit_output_path_is_honoured(), Fixture, folder_pull_does_not_follow_preexisting_symlink_out_of_target(), folder_pull_does_not_write_through_preexisting_file_symlink(), folder_pull_happy_path_still_works(), folder_pull_rejects_hostile_file_names(), folder_pull_rejects_hostile_folder_names(), folder_pull_request_branch_refuses_to_overwrite_without_force() (+20 more)

### Community 16 - "Community 16"
Cohesion: 0.07
Nodes (36): run(), api_url_is_loopback(), clear_config(), config_path(), refuse_non_loopback_when_asked(), save_config(), current_platform(), device_path() (+28 more)

### Community 17 - "Community 17"
Cohesion: 0.07
Nodes (18): acl_is_not_all(), acl_is_root_and_owner(), BeebeebFsState, CachedDir, default_permissions_is_present(), entry(), file_attribute_is_owner_only(), InodeEntry (+10 more)

### Community 18 - "Community 18"
Cohesion: 0.07
Nodes (32): classify_disable_error(), classify_disable_error_passes_through_unrecognized_errors(), classify_disable_error_reports_ambiguous_on_bare_unauthorized(), classify_disable_error_reports_not_enabled_when_never_set_up(), classify_disable_error_reports_not_enabled_when_row_disabled(), classify_enable_error(), classify_enable_error_passes_through_unrecognized_errors(), classify_enable_error_reports_already_enabled() (+24 more)

### Community 19 - "Community 19"
Cohesion: 0.08
Nodes (34): copy_never_names_the_provider(), is_region_unavailable(), list(), list_marks_the_current_choice_and_coming_soon(), list_signed_out_has_no_marker_and_says_how_to_sign_in(), render_list(), unknown_message(), unknown_message_lists_only_selectable_choices_from_the_api() (+26 more)

### Community 20 - "Community 20"
Cohesion: 0.15
Nodes (37): regenerate_core_1908_v2_fixture(), dry_run_never_writes_account_id(), fixture(), Harness, legacy_readable_root_adopts_once(), legacy_unreadable_root_refuses(), manifest(), mismatch_refuses_before_io() (+29 more)

### Community 21 - "Community 21"
Cohesion: 0.06
Nodes (20): fetch_document(), plan_signup(), print_extra(), run(), signup_url(), signup_url_for_the_default_api_is_the_production_signup(), SignupAction, EnvSnapshot (+12 more)

### Community 22 - "Community 22"
Cohesion: 0.08
Nodes (23): guard_not_current_allows_a_non_current_session(), guard_not_current_refuses_the_current_session(), json_mode_extracts_the_raw_sessions_array_unmodified(), no_yes_and_not_rich_refuses_instead_of_silently_proceeding(), parse_sessions(), parse_sessions_defaults_missing_device_fields_like_the_server_does(), parse_sessions_reads_every_field_from_the_live_shape(), parse_sessions_skips_a_row_missing_the_required_id() (+15 more)

### Community 23 - "Community 23"
Cohesion: 0.12
Nodes (21): authenticated_name(), legacy_invite_child_key(), normalize_wrap(), open_invite_owner_copy(), OwnedKeyResolver, ResolvedCliKey, SharedRoot, text() (+13 more)

### Community 24 - "Community 24"
Cohesion: 0.06
Nodes (24): AccountCmd, AddonsAction, all_help_text(), BillingAction, Cli, Commands, help_links_are_full_urls_to_checked_live_pages(), PasskeyCmd (+16 more)

### Community 25 - "Community 25"
Cohesion: 0.15
Nodes (31): both_blocks_label_the_code_so_scripts_and_people_can_find_it(), browser_block(), browser_block_tells_the_user_to_type_the_code_into_the_page(), browser_login(), device_hostname(), headless_block(), headless_block_tells_the_user_to_type_the_code_into_the_page(), init_frame() (+23 more)

### Community 26 - "Community 26"
Cohesion: 0.26
Nodes (29): a_city_is_no_longer_a_choice_and_the_error_prints_no_city_list(), a_legacy_datacenter_id_is_passed_through_and_the_server_maps_it(), a_region_unavailable_rejection_carried_in_the_message_is_explained(), a_server_rejection_is_explained_and_leaves_the_choice_unchanged(), a_typed_region_unavailable_rejection_is_explained_by_its_code(), account_show_region_row_names_the_country_and_never_a_provider(), an_older_server_still_gets_a_datacenter_id_it_understands(), an_older_server_with_city_rows_is_folded_by_country_and_prints_no_city() (+21 more)

### Community 27 - "Community 27"
Cohesion: 0.13
Nodes (22): a_run_that_names_a_server_on_this_machine_is_not_refused(), a_run_that_names_another_public_server_is_refused_too(), a_run_that_names_no_server_stops_before_calling_the_default(), bb(), enclosing_fn_body(), every_test_that_runs_the_binary_sets_the_guard(), exempt(), is_helper_module() (+14 more)

### Community 28 - "Community 28"
Cohesion: 0.23
Nodes (21): Gateway, scratch(), spawn_bb(), spawn_mock_api(), start_gateway(), strip_ansi(), webdav_accepts_loopback_host_spellings(), webdav_challenges_with_basic_realm() (+13 more)

### Community 29 - "Community 29"
Cohesion: 0.18
Nodes (18): any_origin_is_refused_even_with_a_valid_credential(), basic(), constant_time_eq(), credential_must_match(), enforce(), generate_secret(), good_request_passes(), Guard (+10 more)

### Community 30 - "Community 30"
Cohesion: 0.26
Nodes (4): binary(), executable(), InstallerTests, test_unwritable_existing_directory_is_reported()

### Community 31 - "Community 31"
Cohesion: 0.37
Nodes (20): doc(), bb(), fixture(), IntoResponseCompat, json_of(), Mock, Onboarding, scratch_home() (+12 more)

### Community 32 - "Community 32"
Cohesion: 0.2
Nodes (13): arr32(), build_share_material(), hex(), recover_share_link(), share_link(), share_material_roundtrips_to_the_printed_link(), share_wrap_matches_core_kat_vector(), ShareEntry (+5 more)

### Community 33 - "Community 33"
Cohesion: 0.25
Nodes (16): assert_secret_in_no_request(), b64(), bb_with_typed_password(), Captured, handle(), Mock, Mode, registered() (+8 more)

### Community 34 - "Community 34"
Cohesion: 0.14
Nodes (11): breaker_limit(), breaker_trips(), confirm_tombstones(), empty_listing_with_tracked_files_is_untrusted(), is_tombstone(), ListingHealth, move_to_recovery(), move_to_recovery_failure_leaves_the_file_in_place() (+3 more)

### Community 35 - "Community 35"
Cohesion: 0.26
Nodes (12): an_id_that_is_not_a_uuid_is_refused_by_seen_and_remember(), atomic_write(), canonical(), concurrent_writers_never_lose_an_observation(), CustodyStore, directory(), locked(), Observations (+4 more)

### Community 36 - "Community 36"
Cohesion: 0.23
Nodes (13): b64(), bb_with_typed_password(), Captured, handle(), legacy_account_email_change_still_uses_the_legacy_endpoint(), Mock, Mode, opaque_account_email_change_never_sends_the_password_and_points_to_the_web_app() (+5 more)

### Community 37 - "Community 37"
Cohesion: 0.38
Nodes (7): account_billing_opens_the_web_billing_page_without_a_payment_request(), billing_navigation_derives_the_local_web_app(), billing_navigation_honors_the_api_override(), billing_navigation_honors_the_explicit_web_app(), billing_navigation_still_requires_local_authentication(), billing_portal_opens_the_web_billing_page_without_a_payment_request(), Fixture

### Community 38 - "Community 38"
Cohesion: 0.42
Nodes (7): mentions(), mentions_family(), only_the_index_page_writer_takes_a_path(), sanctioned(), sources(), the_cli_publishes_no_shared_folder_keys(), unsanctioned()

### Community 39 - "Community 39"
Cohesion: 0.33
Nodes (5): build_plan_label(), build_plan_label_breaks_down_extra_when_it_exactly_accounts_for_the_total(), build_plan_label_never_fabricates_a_bonus_for_an_unreconciled_remainder(), build_plan_label_shows_the_servers_total_bytes_directly_never_double_counted(), build_plan_label_with_no_extra_shows_just_plan_and_total()

### Community 40 - "Community 40"
Cohesion: 0.25
Nodes (6): FileEventStatus, SessionInfo, SyncFileEvent, SyncStatus, TuiState, TuiView

### Community 41 - "Community 41"
Cohesion: 0.52
Nodes (6): decrypt_payload_matches_webcrypto(), ecdh_shared_secret_matches_webcrypto(), full_flow_ecdh_to_plaintext_via_core(), hex32(), hex_decode(), hkdf_aes_key_matches_webcrypto()

### Community 42 - "Community 42"
Cohesion: 0.6
Nodes (5): build_plan_label(), build_plan_label_breaks_down_extra_when_it_exactly_accounts_for_the_total(), build_plan_label_never_fabricates_a_bonus_for_an_unreconciled_remainder(), build_plan_label_shows_the_servers_total_bytes_directly_never_double_counted(), build_plan_label_with_no_extra_shows_just_plan_and_total()

### Community 44 - "Community 44"
Cohesion: 1.0
Nodes (1): String

## Knowledge Gaps
- **147 isolated node(s):** `Served`, `Current`, `Session`, `Vault`, `Harness` (+142 more)
  These have ≤1 connection - possible missing edges or undocumented components.
- **Thin community `Community 44`** (2 nodes): `String`, `.from()`
  Too small to be a meaningful cluster - may be noise or needs more connections extracted.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `ok()` connect `Community 0` to `Community 1`, `Community 2`, `Community 3`, `Community 4`, `Community 5`, `Community 6`, `Community 7`, `Community 9`, `Community 10`, `Community 11`, `Community 12`, `Community 14`, `Community 16`, `Community 17`, `Community 18`, `Community 19`, `Community 21`, `Community 23`, `Community 25`, `Community 29`, `Community 32`, `Community 33`, `Community 34`, `Community 35`, `Community 36`?**
  _High betweenness centrality (0.169) - this node is a cross-community bridge._
- **Why does `err()` connect `Community 0` to `Community 1`, `Community 2`, `Community 3`, `Community 4`, `Community 6`, `Community 7`, `Community 9`, `Community 11`, `Community 12`, `Community 14`, `Community 17`, `Community 18`, `Community 19`, `Community 23`, `Community 25`, `Community 26`, `Community 29`, `Community 33`, `Community 35`, `Community 36`?**
  _High betweenness centrality (0.079) - this node is a cross-community bridge._
- **Why does `ApiClient` connect `Community 6` to `Community 0`, `Community 1`, `Community 9`, `Community 7`?**
  _High betweenness centrality (0.035) - this node is a cross-community bridge._
- **Are the 261 inferred relationships involving `ok()` (e.g. with `read_request()` and `handle()`) actually correct?**
  _`ok()` has 261 INFERRED edges - model-reasoned connections that need verification._
- **Are the 157 inferred relationships involving `err()` (e.g. with `bb_with_typed_password()` and `bb_with_typed_password()`) actually correct?**
  _`err()` has 157 INFERRED edges - model-reasoned connections that need verification._
- **Are the 64 inferred relationships involving `spawn()` (e.g. with `.serve()` and `start_mock()`) actually correct?**
  _`spawn()` has 64 INFERRED edges - model-reasoned connections that need verification._
- **Are the 27 inferred relationships involving `run()` (e.g. with `.from_config()` and `ok()`) actually correct?**
  _`run()` has 27 INFERRED edges - model-reasoned connections that need verification._
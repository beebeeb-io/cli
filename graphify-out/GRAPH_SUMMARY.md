# Graph Summary - cli  (2026-10-01)

> Made from GRAPH_REPORT.md by scripts/graphify-summary.sh. Detail: graphify query "<question>" or graphify explain <symbol>. Never read GRAPH_REPORT.md whole.

## Summary
- 1227 nodes · 3291 edges · 27 communities detected
- Extraction: 72% EXTRACTED · 28% INFERRED · 0% AMBIGUOUS · INFERRED: 936 edges (avg confidence: 0.8)
- Token cost: 0 input · 0 output

## God Nodes (most connected - your core abstractions)
1. `ApiClient` - 85 edges
2. `parse_response()` - 54 edges
3. `is_json()` - 51 edges
4. `is_quiet()` - 49 edges
5. `run()` - 38 edges
6. `main()` - 36 edges
7. `spawn()` - 32 edges
8. `load_master_key()` - 29 edges
9. `load_master_key()` - 28 edges
10. `run()` - 23 edges

## Top 20 communities (first symbols)
- Community 0 - "Community 0" | 117 nodes: show(), invoices(), open_invoice_pdf(), portal(), purchase_addon(), usage(), decrypt_listing(), DecryptedFile (+109 more)
- Community 1 - "Community 1" | 95 nodes: render_otpauth(), renders_a_typical_totp_uri(), renders_empty_for_garbage_that_cannot_encode(), Match, Node, walk_mem(), walk_mem_anchored_subtree_only(), walk_mem_dfs_order_paths_and_walked() (+87 more)
- Community 2 - "Community 2" | 4 nodes: ApiClient, pace_if_needed(), parse_response(), parse_response_typed()
- Community 3 - "Community 3" | 58 nodes: is_upload_session_gone(), live(), peak(), reset_peak(), clear(), db_path(), key(), load() (+50 more)
- Community 4 - "Community 4" | 58 nodes: run(), browser_login(), print_browser_block(), print_headless_block(), run(), spawn_countdown(), run(), run() (+50 more)
- Community 5 - "Community 5" | 56 nodes: print_created(), run(), run_recursive(), split_parent_and_leaf(), DestinationParent, parent_target(), ParentError, destination_parent() (+48 more)
- Community 6 - "Community 6" | 43 nodes: add_json_body_carries_the_url_and_a_note_never_a_token(), AddAction, confirm_remove(), enrollment_url_bracketed_ipv6_loopback_is_local_and_port_is_stripped(), enrollment_url_does_not_treat_a_127_0_0_1_labeled_domain_as_local() ...
- Community 7 - "Community 7" | 55 nodes: webdav_move_rejects_invalid_parent_without_patch(), a_successful_init_never_fetches_the_subscription(), a_successful_share_never_fetches_the_subscription(), account_lapsed(), account_lapsed_409_is_explained_with_the_deletion_date_fetched_once() ...
- Community 8 - "Community 8" | 37 nodes: addons(), aggregate_by_region(), aggregate_by_region_defaults_missing_storage_location_to_europe_falkenstein(), aggregate_by_region_empty_input_is_empty_output(), aggregate_by_region_splits_multiple_regions() ...
- Community 9 - "Community 9" | 36 nodes: classify_disable_error(), classify_disable_error_passes_through_unrecognized_errors(), classify_disable_error_reports_ambiguous_on_bare_unauthorized(), classify_disable_error_reports_not_enabled_when_never_set_up() ...
- Community 10 - "Community 10" | 30 nodes: confirm_revoke_all(), extract_sessions_json(), guard_not_current(), guard_not_current_allows_a_non_current_session(), guard_not_current_refuses_the_current_session(), json_mode_extracts_the_raw_sessions_array_unmodified(), marker_cell() ...
- Community 11 - "Community 11" | 38 nodes: b64std(), b64url(), build_link(), decode_any_b64(), generate_request_keypair(), keypair_wrap_unwrap_roundtrip_matches_create_then_list(), link_assembly_roundtrips_through_parse(), parse_expiry_secs() (+30 more)
- Community 12 - "Community 12" | 15 nodes: build_plan_label(), build_plan_label_breaks_down_extra_when_it_exactly_accounts_for_the_total(), build_plan_label_never_fabricates_a_bonus_for_an_unreconciled_remainder(), build_plan_label_shows_the_servers_total_bytes_directly_never_double_counted() ...
- Community 13 - "Community 13" | 15 nodes: BeebeebFs, CachedDir, InodeEntry, PendingCreate, rename_parent_target(), run(), unmount(), AtomicFile (+7 more)
- Community 14 - "Community 14" | 33 nodes: write_private_temp_pdf_refuses_to_follow_an_existing_symlink_at_the_target_path(), bb(), Calls, duplicate_push_without_a_strategy_fails_with_a_hint(), rm_with_force_still_trashes_non_interactively(), rm_without_force_refuses_and_trashes_nothing() ...
- Community 15 - "Community 15" | 8 nodes: plan_signup(), run(), signup_url(), signup_url_for_the_default_api_is_the_production_signup(), SignupAction, host_of(), resolve_web_app_base(), web_app_base_from_api()
- Community 16 - "Community 16" | 19 nodes: AccountCmd, AddonsAction, all_help_text(), BillingAction, build_help_text(), Cli, Commands, help_links_are_full_urls_to_checked_live_pages() (+11 more)
- Community 17 - "Community 17" | 17 nodes: check_and_update(), cooldown_elapsed(), ct_eq_ignore_case(), current_target(), DistArtifact, DistChecksums, DistManifest, extract_binary_from_tarball() (+9 more)
- Community 18 - "Community 18" | 16 nodes: arr32(), build_share_material(), draw_picker(), hex(), parse_hours(), recover_share_link(), run_picker(), share_link() (+8 more)
- Community 19 - "Community 19" | 14 nodes: build_show_payload(), build_show_payload_assembles_all_sections(), build_show_payload_degrades_gracefully_per_section(), map_email_change_error(), normalize_update_email(), opaque_email_change(), print_email_change_success(), render_progress_bar() (+6 ...

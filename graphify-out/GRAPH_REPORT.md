# Graph Report - cli-flow6-4  (2026-09-26)

## Corpus Check
- 66 files · ~113,067 words
- Verdict: corpus is large enough that graph structure adds value.

## Summary
- 1046 nodes · 2793 edges · 22 communities detected
- Extraction: 72% EXTRACTED · 28% INFERRED · 0% AMBIGUOUS · INFERRED: 771 edges (avg confidence: 0.8)
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
7. `load_master_key()` - 29 edges
8. `load_master_key()` - 27 edges
9. `run()` - 23 edges
10. `BeebeebFs` - 21 edges

## Surprising Connections (you probably didn't know these)
- `text()` --calls--> `browser_login()`  [INFERRED]
  tests/auth_errors.rs → src/commands/login.rs
- `oneRun()` --calls--> `String`  [INFERRED]
  scripts/prod-bots/auth-bot.mjs → src/api.rs
- `expected_ciphertext_for()` --calls--> `run()`  [INFERRED]
  src/upload.rs → src/commands/sync.rs
- `main()` --calls--> `code()`  [INFERRED]
  src/main.rs → src/exit.rs
- `mark_uploaded()` --calls--> `push_single_file()`  [INFERRED]
  src/loopback.rs → src/commands/push.rs

## Communities

### Community 0 - "Community 0"
Cohesion: 0.05
Nodes (85): decrypt_listing(), DecryptedFile, LsOpts, print_header(), print_json(), print_recursive(), print_row(), run() (+77 more)

### Community 1 - "Community 1"
Cohesion: 0.04
Nodes (58): addons(), aggregate_by_region(), aggregate_by_region_defaults_missing_storage_location_to_europe_falkenstein(), aggregate_by_region_empty_input_is_empty_output(), aggregate_by_region_splits_multiple_regions(), aggregate_by_region_sums_bytes_per_region_and_skips_folders(), capitalise(), format_date_human() (+50 more)

### Community 2 - "Community 2"
Cohesion: 0.07
Nodes (76): classify_new_local_is_not_a_delete(), classify_prior_synced_modified_is_readd(), classify_prior_synced_unchanged_is_remote_delete(), classify_remote_absent(), compute_file_hash(), create_folder(), do_download(), do_upload() (+68 more)

### Community 3 - "Community 3"
Cohesion: 0.09
Nodes (4): ApiClient, pace_if_needed(), parse_response(), parse_response_typed()

### Community 4 - "Community 4"
Cohesion: 0.04
Nodes (57): upload_file_to(), AccountCmd, AddonsAction, all_help_text(), BillingAction, build_help_text(), Cli, Commands (+49 more)

### Community 5 - "Community 5"
Cohesion: 0.05
Nodes (48): add(), add_json_body_carries_the_url_and_a_note_never_a_token(), AddAction, classify_list_error(), confirm_remove(), enrollment_url_bracketed_ipv6_loopback_is_local_and_port_is_stripped(), enrollment_url_does_not_treat_a_127_0_0_1_labeled_domain_as_local(), enrollment_url_does_not_treat_a_localhost_labeled_domain_as_local() (+40 more)

### Community 6 - "Community 6"
Cohesion: 0.06
Nodes (47): render_otpauth(), renders_a_typical_totp_uri(), renders_empty_for_garbage_that_cannot_encode(), classify_disable_error(), classify_disable_error_passes_through_unrecognized_errors(), classify_disable_error_reports_ambiguous_on_bare_unauthorized(), classify_disable_error_reports_not_enabled_when_never_set_up(), classify_disable_error_reports_not_enabled_when_row_disabled() (+39 more)

### Community 7 - "Community 7"
Cohesion: 0.06
Nodes (46): run(), SortField, Config, check_and_update(), cooldown_elapsed(), ct_eq_ignore_case(), current_target(), DistArtifact (+38 more)

### Community 8 - "Community 8"
Cohesion: 0.07
Nodes (49): acquire_confirm_token(), acquire_confirmed_password(), confirm_password_session_too_old_survives_a_message_that_does_not_mention_the_code(), ConfirmedPassword, map_confirm_error(), Match, Node, run() (+41 more)

### Community 9 - "Community 9"
Cohesion: 0.07
Nodes (34): classify_list_error(), confirm_revoke_all(), extract_sessions_json(), guard_not_current(), guard_not_current_allows_a_non_current_session(), guard_not_current_refuses_the_current_session(), json_mode_extracts_the_raw_sessions_array_unmodified(), list() (+26 more)

### Community 10 - "Community 10"
Cohesion: 0.09
Nodes (43): print_created(), run(), run_recursive(), split_parent_and_leaf(), CachedDir, check_lock(), child_href(), DavState (+35 more)

### Community 11 - "Community 11"
Cohesion: 0.08
Nodes (31): build_show_payload(), build_show_payload_assembles_all_sections(), build_show_payload_degrades_gracefully_per_section(), map_email_change_error(), normalize_update_email(), opaque_email_change(), print_email_change_success(), render_progress_bar() (+23 more)

### Community 12 - "Community 12"
Cohesion: 0.15
Nodes (32): bb(), Calls, duplicate_push_without_a_strategy_fails_with_a_hint(), rm_with_force_still_trashes_non_interactively(), rm_without_force_refuses_and_trashes_nothing(), scratch_home(), start_mock(), sync_once_with_a_failed_upload_exits_non_zero() (+24 more)

### Community 13 - "Community 13"
Cohesion: 0.11
Nodes (13): BeebeebFs, CachedDir, InodeEntry, PendingCreate, unmount(), AtomicFile, buffered_fallback(), DownloadStats (+5 more)

### Community 14 - "Community 14"
Cohesion: 0.12
Nodes (25): b64std(), b64url(), build_link(), create(), decode_any_b64(), generate_request_keypair(), keypair_wrap_unwrap_roundtrip_matches_create_then_list(), link_assembly_roundtrips_through_parse() (+17 more)

### Community 15 - "Community 15"
Cohesion: 0.16
Nodes (19): app_url(), arr32(), build_share_material(), draw_picker(), hex(), list(), parse_hours(), recover_share_link() (+11 more)

### Community 16 - "Community 16"
Cohesion: 0.22
Nodes (19): decrypt_file_chunks(), decrypt_json_chunks(), decrypt_name(), decrypt_name_plaintext_passthrough(), decrypt_name_with_key(), decrypt_names(), decrypt_names_batch_matches_single(), decrypt_raw_chunks() (+11 more)

### Community 17 - "Community 17"
Cohesion: 0.31
Nodes (12): check(), clap_accepts(), code_spans(), expand(), extract(), flag_defined_anywhere(), Invocation, is_flag() (+4 more)

### Community 18 - "Community 18"
Cohesion: 0.58
Nodes (11): bb(), logged_out_whoami_exits_nonzero_with_a_login_hint(), revoked_session_ls_tells_the_user_to_run_bb_login(), revoked_session_quota_tells_the_user_to_run_bb_login(), revoked_session_status_fails_too(), revoked_session_whoami_fails_and_never_shows_placeholder_plan_data(), scratch_home(), spawn_unauthorized_mock() (+3 more)

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
- **66 isolated node(s):** `Calls`, `Mock`, `ThumbnailResult`, `Invocation`, `OutputMode` (+61 more)
  These have ≤1 connection - possible missing edges or undocumented components.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `ApiClient` connect `Community 3` to `Community 8`, `Community 11`?**
  _High betweenness centrality (0.086) - this node is a cross-community bridge._
- **Why does `is_quiet()` connect `Community 0` to `Community 1`, `Community 2`, `Community 3`, `Community 5`, `Community 6`, `Community 9`, `Community 11`, `Community 14`, `Community 15`, `Community 19`?**
  _High betweenness centrality (0.075) - this node is a cross-community bridge._
- **Why does `run()` connect `Community 2` to `Community 0`, `Community 4`, `Community 7`, `Community 8`, `Community 11`, `Community 12`?**
  _High betweenness centrality (0.049) - this node is a cross-community bridge._
- **Are the 49 inferred relationships involving `is_json()` (e.g. with `list()` and `add()`) actually correct?**
  _`is_json()` has 49 INFERRED edges - model-reasoned connections that need verification._
- **Are the 46 inferred relationships involving `is_quiet()` (e.g. with `parse_response_typed()` and `list()`) actually correct?**
  _`is_quiet()` has 46 INFERRED edges - model-reasoned connections that need verification._
- **Are the 16 inferred relationships involving `run()` (e.g. with `.from_config()` and `uninstall_launchagent()`) actually correct?**
  _`run()` has 16 INFERRED edges - model-reasoned connections that need verification._
- **What connects `Calls`, `Mock`, `ThumbnailResult` to the rest of the system?**
  _66 weakly-connected nodes found - possible documentation gaps or missing edges._
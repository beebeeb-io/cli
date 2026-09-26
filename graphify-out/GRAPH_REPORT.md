# Graph Report - cli-cap15  (2026-09-25)

## Corpus Check
- 62 files · ~107,914 words
- Verdict: corpus is large enough that graph structure adds value.

## Summary
- 989 nodes · 2616 edges · 20 communities detected
- Extraction: 72% EXTRACTED · 28% INFERRED · 0% AMBIGUOUS · INFERRED: 730 edges (avg confidence: 0.8)
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
- `text()` --calls--> `browser_login()`  [INFERRED]
  tests/auth_errors.rs → src/commands/login.rs
- `oneRun()` --calls--> `String`  [INFERRED]
  scripts/prod-bots/auth-bot.mjs → src/api.rs
- `set_api_url_override()` --calls--> `main()`  [INFERRED]
  src/config.rs → src/main.rs
- `expected_ciphertext_for()` --calls--> `run()`  [INFERRED]
  src/upload.rs → src/commands/sync.rs
- `mark_uploaded()` --calls--> `push_single_file()`  [INFERRED]
  src/loopback.rs → src/commands/push.rs

## Communities

### Community 0 - "Community 0"
Cohesion: 0.05
Nodes (105): capitalise(), format_number(), purchase_addon(), write_private_temp_pdf_refuses_to_follow_an_existing_symlink_at_the_target_path(), Match, Node, run(), walk_mem() (+97 more)

### Community 1 - "Community 1"
Cohesion: 0.05
Nodes (38): ApiClient, backoff(), build_client(), delete_passkey_sends_delete_to_the_id_path_with_bearer_auth_and_no_confirm_token(), download_invoice_pdf_returns_the_raw_bytes(), download_invoice_pdf_surfaces_a_404_for_an_unknown_id(), echo_method_id_and_headers(), error_without_a_message_field_still_falls_back_to_the_bare_code() (+30 more)

### Community 2 - "Community 2"
Cohesion: 0.05
Nodes (85): decrypt_listing(), DecryptedFile, LsOpts, print_header(), print_json(), print_recursive(), print_row(), run() (+77 more)

### Community 3 - "Community 3"
Cohesion: 0.04
Nodes (54): addons(), aggregate_by_region(), aggregate_by_region_defaults_missing_storage_location_to_europe_falkenstein(), aggregate_by_region_empty_input_is_empty_output(), aggregate_by_region_splits_multiple_regions(), aggregate_by_region_sums_bytes_per_region_and_skips_folders(), format_date_human(), format_invoice_amount() (+46 more)

### Community 4 - "Community 4"
Cohesion: 0.05
Nodes (48): add(), add_json_body_carries_the_url_and_a_note_never_a_token(), AddAction, classify_list_error(), confirm_remove(), enrollment_url_bracketed_ipv6_loopback_is_local_and_port_is_stripped(), enrollment_url_does_not_treat_a_127_0_0_1_labeled_domain_as_local(), enrollment_url_does_not_treat_a_localhost_labeled_domain_as_local() (+40 more)

### Community 5 - "Community 5"
Cohesion: 0.06
Nodes (47): render_otpauth(), renders_a_typical_totp_uri(), renders_empty_for_garbage_that_cannot_encode(), classify_disable_error(), classify_disable_error_passes_through_unrecognized_errors(), classify_disable_error_reports_ambiguous_on_bare_unauthorized(), classify_disable_error_reports_not_enabled_when_never_set_up(), classify_disable_error_reports_not_enabled_when_row_disabled() (+39 more)

### Community 6 - "Community 6"
Cohesion: 0.05
Nodes (49): AccountCmd, AddonsAction, all_help_text(), BillingAction, build_help_text(), Cli, Commands, help_footer_text() (+41 more)

### Community 7 - "Community 7"
Cohesion: 0.07
Nodes (34): classify_list_error(), confirm_revoke_all(), extract_sessions_json(), guard_not_current(), guard_not_current_allows_a_non_current_session(), guard_not_current_refuses_the_current_session(), json_mode_extracts_the_raw_sessions_array_unmodified(), list() (+26 more)

### Community 8 - "Community 8"
Cohesion: 0.09
Nodes (43): print_created(), run(), run_recursive(), split_parent_and_leaf(), CachedDir, check_lock(), child_href(), DavState (+35 more)

### Community 9 - "Community 9"
Cohesion: 0.08
Nodes (34): run(), oneRun(), String, Config, current_platform(), device_path(), DeviceInfo, get_device_id() (+26 more)

### Community 10 - "Community 10"
Cohesion: 0.09
Nodes (29): browser_login(), print_browser_block(), print_headless_block(), run(), spawn_countdown(), run(), run(), build_plan_label() (+21 more)

### Community 11 - "Community 11"
Cohesion: 0.11
Nodes (13): BeebeebFs, CachedDir, InodeEntry, PendingCreate, unmount(), AtomicFile, buffered_fallback(), DownloadStats (+5 more)

### Community 12 - "Community 12"
Cohesion: 0.12
Nodes (23): decrypt_file_chunks(), decrypt_name(), decrypt_name_plaintext_passthrough(), decrypt_name_with_key(), decrypt_names(), decrypt_names_batch_matches_single(), json_blob_legacy_format_detected_and_decrypted(), json_blob_with_binary_uuid_key() (+15 more)

### Community 13 - "Community 13"
Cohesion: 0.14
Nodes (23): b64std(), b64url(), build_link(), create(), decode_any_b64(), generate_request_keypair(), keypair_wrap_unwrap_roundtrip_matches_create_then_list(), link_assembly_roundtrips_through_parse() (+15 more)

### Community 14 - "Community 14"
Cohesion: 0.14
Nodes (15): build_show_payload(), build_show_payload_assembles_all_sections(), build_show_payload_degrades_gracefully_per_section(), map_email_change_error(), normalize_update_email(), opaque_email_change(), print_email_change_success(), render_progress_bar() (+7 more)

### Community 15 - "Community 15"
Cohesion: 0.31
Nodes (12): check(), clap_accepts(), code_spans(), expand(), extract(), flag_defined_anywhere(), Invocation, is_flag() (+4 more)

### Community 16 - "Community 16"
Cohesion: 0.58
Nodes (11): bb(), logged_out_whoami_exits_nonzero_with_a_login_hint(), revoked_session_ls_tells_the_user_to_run_bb_login(), revoked_session_quota_tells_the_user_to_run_bb_login(), revoked_session_status_fails_too(), revoked_session_whoami_fails_and_never_shows_placeholder_plan_data(), scratch_home(), spawn_unauthorized_mock() (+3 more)

### Community 17 - "Community 17"
Cohesion: 0.25
Nodes (6): FileEventStatus, SessionInfo, SyncFileEvent, SyncStatus, TuiState, TuiView

### Community 18 - "Community 18"
Cohesion: 0.52
Nodes (6): decrypt_payload_matches_webcrypto(), ecdh_shared_secret_matches_webcrypto(), full_flow_ecdh_to_plaintext_via_core(), hex32(), hex_decode(), hkdf_aes_key_matches_webcrypto()

### Community 19 - "Community 19"
Cohesion: 0.83
Nodes (3): mark_uploaded(), registry(), was_recently_uploaded()

## Knowledge Gaps
- **63 isolated node(s):** `ThumbnailResult`, `Invocation`, `OutputMode`, `DeviceInfo`, `DownloadStats` (+58 more)
  These have ≤1 connection - possible missing edges or undocumented components.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `is_quiet()` connect `Community 2` to `Community 0`, `Community 1`, `Community 3`, `Community 4`, `Community 5`, `Community 7`, `Community 10`, `Community 13`, `Community 14`?**
  _High betweenness centrality (0.086) - this node is a cross-community bridge._
- **Why does `ApiClient` connect `Community 1` to `Community 10`?**
  _High betweenness centrality (0.080) - this node is a cross-community bridge._
- **Why does `is_json()` connect `Community 2` to `Community 0`, `Community 3`, `Community 4`, `Community 5`, `Community 7`, `Community 8`, `Community 9`, `Community 10`, `Community 13`, `Community 14`?**
  _High betweenness centrality (0.056) - this node is a cross-community bridge._
- **Are the 49 inferred relationships involving `is_json()` (e.g. with `list()` and `add()`) actually correct?**
  _`is_json()` has 49 INFERRED edges - model-reasoned connections that need verification._
- **Are the 46 inferred relationships involving `is_quiet()` (e.g. with `parse_response_typed()` and `list()`) actually correct?**
  _`is_quiet()` has 46 INFERRED edges - model-reasoned connections that need verification._
- **Are the 15 inferred relationships involving `run()` (e.g. with `.from_config()` and `uninstall_launchagent()`) actually correct?**
  _`run()` has 15 INFERRED edges - model-reasoned connections that need verification._
- **What connects `ThumbnailResult`, `Invocation`, `OutputMode` to the rest of the system?**
  _63 weakly-connected nodes found - possible documentation gaps or missing edges._
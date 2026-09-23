# Graph Report - cli-0480  (2026-09-23)

## Corpus Check
- 58 files · ~86,088 words
- Verdict: corpus is large enough that graph structure adds value.

## Summary
- 759 nodes · 2072 edges · 16 communities detected
- Extraction: 70% EXTRACTED · 30% INFERRED · 0% AMBIGUOUS · INFERRED: 628 edges (avg confidence: 0.8)
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

## God Nodes (most connected - your core abstractions)
1. `ApiClient` - 77 edges
2. `parse_response()` - 62 edges
3. `is_json()` - 42 edges
4. `is_quiet()` - 39 edges
5. `run()` - 36 edges
6. `main()` - 28 edges
7. `load_master_key()` - 28 edges
8. `load_master_key()` - 27 edges
9. `run()` - 21 edges
10. `BeebeebFs` - 21 edges

## Surprising Connections (you probably didn't know these)
- `set_api_url_override()` --calls--> `main()`  [INFERRED]
  src/config.rs → src/main.rs
- `expected_ciphertext_for()` --calls--> `run()`  [INFERRED]
  src/upload.rs → src/commands/sync.rs
- `generate_from_file()` --calls--> `push_single_file()`  [INFERRED]
  src/thumbnail.rs → src/commands/push.rs
- `generate_large_from_file()` --calls--> `push_single_file()`  [INFERRED]
  src/thumbnail.rs → src/commands/push.rs
- `is_rich()` --calls--> `run()`  [INFERRED]
  src/ui.rs → src/commands/rm.rs

## Communities

### Community 0 - "Community 0"
Cohesion: 0.05
Nodes (77): build_show_payload(), build_show_payload_assembles_all_sections(), build_show_payload_degrades_gracefully_per_section(), map_email_change_error(), normalize_update_email(), opaque_email_change(), print_email_change_success(), render_progress_bar() (+69 more)

### Community 1 - "Community 1"
Cohesion: 0.07
Nodes (19): ApiClient, backoff(), build_client(), every_request_carries_client_and_version_headers(), format_request_error(), ids(), is_transient_transport_error(), last_page_null_cursor_terminates_the_walk() (+11 more)

### Community 2 - "Community 2"
Cohesion: 0.07
Nodes (75): classify_new_local_is_not_a_delete(), classify_prior_synced_modified_is_readd(), classify_prior_synced_unchanged_is_remote_delete(), classify_remote_absent(), compute_file_hash(), create_folder(), do_download(), do_upload() (+67 more)

### Community 3 - "Community 3"
Cohesion: 0.05
Nodes (47): upload_file_to(), AccountCmd, AddonsAction, BillingAction, Cli, Commands, live(), peak() (+39 more)

### Community 4 - "Community 4"
Cohesion: 0.07
Nodes (51): acquire_confirm_token(), acquire_confirmed_password(), ConfirmedPassword, print_created(), run(), run_recursive(), split_parent_and_leaf(), confirm() (+43 more)

### Community 5 - "Community 5"
Cohesion: 0.07
Nodes (45): build_plan_label(), capitalise(), format_number(), run(), b64std(), b64url(), build_link(), create() (+37 more)

### Community 6 - "Community 6"
Cohesion: 0.07
Nodes (39): render_otpauth(), renders_a_typical_totp_uri(), renders_empty_for_garbage_that_cannot_encode(), classify_disable_error(), classify_disable_error_passes_through_unrecognized_errors(), classify_disable_error_reports_ambiguous_on_bare_unauthorized(), classify_disable_error_reports_not_enabled_when_never_set_up(), classify_disable_error_reports_not_enabled_when_row_disabled() (+31 more)

### Community 7 - "Community 7"
Cohesion: 0.06
Nodes (35): run(), browser_login(), print_browser_block(), print_headless_block(), run(), spawn_countdown(), run(), run() (+27 more)

### Community 8 - "Community 8"
Cohesion: 0.11
Nodes (36): decrypt_name(), collect_zip_entries(), looks_like_id_prefix(), pull_folder(), pull_folder_inner(), pull_single_file(), resolve_as_path(), resolve_request_key() (+28 more)

### Community 9 - "Community 9"
Cohesion: 0.11
Nodes (14): BeebeebFs, CachedDir, InodeEntry, PendingCreate, run(), unmount(), AtomicFile, buffered_fallback() (+6 more)

### Community 10 - "Community 10"
Cohesion: 0.14
Nodes (20): classify_list_error(), extract_sessions_json(), json_mode_extracts_the_raw_sessions_array_unmodified(), list(), marker_cell(), parse_sessions(), parse_sessions_defaults_missing_device_fields_like_the_server_does(), parse_sessions_reads_every_field_from_the_live_shape() (+12 more)

### Community 11 - "Community 11"
Cohesion: 0.13
Nodes (18): check_and_update(), cooldown_elapsed(), ct_eq_ignore_case(), current_target(), DistArtifact, DistChecksums, DistManifest, extract_binary_from_tarball() (+10 more)

### Community 12 - "Community 12"
Cohesion: 0.26
Nodes (16): decrypt_file_chunks(), decrypt_name(), decrypt_name_plaintext_passthrough(), decrypt_name_with_key(), decrypt_names(), decrypt_names_batch_matches_single(), json_blob_legacy_format_detected_and_decrypted(), json_blob_with_binary_uuid_key() (+8 more)

### Community 13 - "Community 13"
Cohesion: 0.19
Nodes (6): box_line(), OutputMode, strip_ansi(), table(), table_aligns_columns_by_widest_cell(), table_width_calculation_ignores_ansi_escapes()

### Community 14 - "Community 14"
Cohesion: 0.25
Nodes (6): FileEventStatus, SessionInfo, SyncFileEvent, SyncStatus, TuiState, TuiView

### Community 15 - "Community 15"
Cohesion: 0.52
Nodes (6): decrypt_payload_matches_webcrypto(), ecdh_shared_secret_matches_webcrypto(), full_flow_ecdh_to_plaintext_via_core(), hex32(), hex_decode(), hkdf_aes_key_matches_webcrypto()

## Knowledge Gaps
- **59 isolated node(s):** `ThumbnailResult`, `OutputMode`, `DeviceInfo`, `DownloadStats`, `PendingDb` (+54 more)
  These have ≤1 connection - possible missing edges or undocumented components.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `ApiClient` connect `Community 1` to `Community 0`?**
  _High betweenness centrality (0.098) - this node is a cross-community bridge._
- **Why does `is_quiet()` connect `Community 0` to `Community 1`, `Community 2`, `Community 4`, `Community 5`, `Community 8`, `Community 10`, `Community 13`?**
  _High betweenness centrality (0.082) - this node is a cross-community bridge._
- **Why does `run()` connect `Community 2` to `Community 0`, `Community 3`, `Community 5`, `Community 7`, `Community 8`?**
  _High betweenness centrality (0.075) - this node is a cross-community bridge._
- **Are the 41 inferred relationships involving `is_json()` (e.g. with `create()` and `list()`) actually correct?**
  _`is_json()` has 41 INFERRED edges - model-reasoned connections that need verification._
- **Are the 38 inferred relationships involving `is_quiet()` (e.g. with `parse_response()` and `create()`) actually correct?**
  _`is_quiet()` has 38 INFERRED edges - model-reasoned connections that need verification._
- **Are the 15 inferred relationships involving `run()` (e.g. with `.from_config()` and `uninstall_launchagent()`) actually correct?**
  _`run()` has 15 INFERRED edges - model-reasoned connections that need verification._
- **What connects `ThumbnailResult`, `OutputMode`, `DeviceInfo` to the rest of the system?**
  _59 weakly-connected nodes found - possible documentation gaps or missing edges._
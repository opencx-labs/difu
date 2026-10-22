# Settings verification — 0.26.0

Implementation is on `ziad-altawil/feat/settings-panel`, based on
`6062039f4d417a0c1392f645414c218ea1401616`. This report covers the complete native
Settings panel, its real runtime consumers, connection flows and packaging.
No install, push, merge, tag or publication is part of this work.

## Final results

All final checks below used the same frozen source. Every file in the immutable
CLI source snapshot was SHA-256 compared with the final worktree. The final
interrupted-read regression is included in these totals.

| Platform | Passed | Failed | Existing ignored | Release archive smoke |
|---|---:|---:|---:|---|
| macOS ARM64 (native) | 862 | 0 | 5 | Passed |
| macOS x86_64 (Rosetta) | 862 | 0 | 5 | Passed |
| Linux ARM64 musl (native VM) | 855 | 0 | 5 | Passed |
| Linux x86_64 musl (emulated) | 855 | 0 | 5 | Passed |

Native ordered checks: `cargo fmt --all -- --check` →
`cargo clippy --locked --all-targets -- -D warnings` → `cargo test --locked`, all
exit **0**. Logs: `/tmp/difu-settings-final-fixed-1dnpvz_r/{fmt,clippy,tests}.log` and `results.json`.
Intel macOS used `cargo test --locked --target x86_64-apple-darwin` under Rosetta.
Linux used `cargo test --offline --locked -- --test-threads=4` on both
architectures. Every suite completed all 16 result groups; no added test is ignored.

`python3 -m unittest discover -s cli/scripts -p 'test_*.py'`: **17 passed**.
These include generated Homebrew install execution for both platform branches,
archive/Cellar helper discovery, bundle rejection, minimum-OS parsing and existing
release provenance checks. Release YAML parsed; `sh -n` passed for the helper
build script. `actionlint` was unavailable locally.

## Packages and reproducible platform setup

All four `cargo build --locked --release` builds passed (Linux also used
`--offline`). macOS builds explicitly used `MACOSX_DEPLOYMENT_TARGET=11.0`.
Both macOS helpers were built for their architecture and ad-hoc signed. The
extracted archives passed executable version and noninteractive-refusal checks
with an empty PATH. macOS checks additionally covered Mach-O architecture,
minimum OS, absence of build-machine library dependencies, helper metadata and
signature, actual non-prompting status and invalid-request responses. These also
passed after relocation into Cellar `bin`/`libexec` and launching through a bin
symlink from an unrelated directory. Linux ELF inspection confirmed the expected
architecture and static musl linkage.

`prepare-release.py` generated the formula and SHA256SUMS from these **four real
archives**; Ruby syntax and every generated checksum passed. Nothing was installed
or published. Archives, checksums, formula, `verification.json`, source hashes,
ELF inspection and per-platform logs are retained under
`/private/tmp/difu-settings-release-verified`.

Linux toolchains: `difu-settings-toolchain:06e5-arm64` and `:06e5-amd64`, based on
Rust 1.99.1 Alpine digest
`sha256:eb63a0c00e4d228daf8bd3250bacaaf0cf5d335965e577535e40388ecb8124a5`,
with git, python3, bash, build-base, pkgconf and dbus. Final runs used one build
container at a time, `--network none --cpus=1.5 --memory=2048m
--memory-swap=2048m`, read-only source, separate task target/cache volumes,
`CARGO_BUILD_JOBS=1`, `CARGO_PROFILE_TEST_DEBUG=0`, `CARGO_INCREMENTAL=0`.
A real isolated `difutest` login (UID/GID 1000) and container-only Git identity
`Fixture <fixture@example.invalid>` provide a normal filesystem/D-Bus/Git test
environment. No HOME/CODEX_HOME override, user credential mount, host port,
privileged mode, or changes to existing user containers were used.

## Real terminal checks

`settings_terminal_lifecycle_uses_real_pty` drives real Crossterm input through
Shell and Ratatui at **42x20 and 140x42**, using isolated Storage. It proves the
old open chord is removed after remapping to F12; bracketed-paste intent search;
Full→Reduced editing; replay; Save; Reduced→Off then Discard; close/reopen with
persisted Reduced; actual panel output and terminal restoration. It starts no
provider or service. Existing native parser tests retain Shift+Left CSI,
Ctrl+Enter CSI-u and Meta-arrow handling.

`auth_terminal_lifecycle_uses_real_pty` runs seven outcomes through a fake native
login executable: both provider success paths with real foreground stdin,
nonzero exit, Ctrl+C, missing executable, HUP and TERM. It checks cooked input
during login and restored raw/alternate-screen/bracketed-paste modes afterward.
This establishes terminal ownership, not live OAuth success.

## Every setting and runtime consumer

Named tests exercise saved/draft/reset/discard behavior and real renderers or
service requests. Narrow helpers do not replace actual dispatch coverage.

| Functionality | Observable verification |
|---|---|
| Legacy config + safe saves | `legacy_values_remain_canonical_and_provider_is_inferred`, `settings_save_patches_only_edited_leaves_and_preserves_unknown_fields`, `concurrent_draft_writers_merge_under_file_lock`, `same_field_conflicts_keep_draft_and_saved_bytes`, `real_write_failure_preserves_saved_config_and_dirty_draft`, `no_op_save_keeps_legacy_document_bytes_and_rejects_malformed_without_writing`. |
| Open Settings | `settings_command_is_available_in_reviews_without_switching_tabs`, `remapped_shell_globals_dispatch_and_replace_the_old_action_keys`; `settings_shortcut_override_replaces_default_and_slash_aliases_request_sections` and final real PTY lifecycle. |
| Draft lifecycle | `section_reset_export_and_escape_never_lose_unsaved_work`, `save_conflict_retains_draft_and_discard_reads_latest`, `intent_search_and_modified_filter_follow_draft_edits`, `two_clients_preview_only_edited_fields_and_keep_same_field_conflicts`. |
| Export | `exported_preferences_exclude_rules_connections_and_session_content`, `section_reset_export_and_escape_never_lose_unsaved_work`: unique private file, draft values, no credentials/rules/session content. |
| Intent search | `intent_search_and_modified_filter_follow_draft_edits`, `supplied_shortcuts_rank_exact_aliases_before_other_label_matches`, `question_metadata_lookup_matches_intent_search`; real PTY bracketed paste. |
| Responsive panel | `keyboard_can_read_full_details_in_a_narrow_terminal`, `narrow_long_label_controls_show_current_value_in_list_and_expanded_details`, `narrow_details_show_in_progress_numeric_value`; real PTY at 42x20 and 140x42. |
| Input isolation | `settings_preserves_agent_draft_selection_and_scroll_from_both_tabs`, `async_action_close_returns_key_and_paste_to_settings_without_touching_composer`, `settings_entry_and_cross_client_disable_drop_active_voice_without_losing_draft`, `editing_reservations_preserve_actual_composer_input_from_invalid_stored_globals`, `explicit_sign_in_handoff_keeps_unsaved_draft_and_separate_status_actions`. |
| Theme | `all_theme_previews_stay_local_and_save_discard_apply_visual_settings` iterates all themes; two-client test covers external refresh.  |
| Density | `production_density_and_separators_keep_chat_copy_selection_and_draft` (native Ui + mouse copy); `compact_and_separator_previews_keep_the_original_text` (shared sample). Review R1: `reading_intent_survives_density_height_clamps_and_missing_entries` restores a nonzero source anchor after Compact/taller-viewport clamping, covers incoming content, follow replacement, deleted source and empty history. |
| Contrast | `high_contrast_resolves_muted_roles_without_mutating_cached_cells`; all-theme Settings preview/Save/Discard test; two-client rendered High contrast. |
| Message separators | production native draw/copy test plus shared sample; `appearance_reflow_keeps_selected_copy_and_tracks_message_rows` verifies selected source/endpoint retention. |
| Motion level | `motion_off_stops_animation_wakeups_but_keeps_notice_and_elapsed_updates`; reduced sample; `reduced_and_off_voice_show_received_words_without_flapping`; sidebar/screen and reveal mode matrices. Review R1: `dedicated_previews_honor_disabled_modes_zero_and_independent_toggles` covers all three dedicated scenes in Reduced/Off; in-flight question refresh test covers both modes. |
| Reply/tool effect + speed | `preference_modes_and_reply_toggle_use_real_reveal_speed_and_zero_cap`; independent real sent/tool/reply sample test; existing `chat_entries_flip_in_when_new_and_flip_only_their_changes` retained. |
| Sent effect + duration | `sent_and_clear_preferences_bound_real_animation_without_touching_new_drafts`; `preview_replays_the_real_sent_message_effect_independently_of_reply_effects`; zero-duration in-flight settlement test. Review R1: `committed_refresh_settles_clearing_without_redisplaying_an_inflight_answer` proves sent-disable settles question clearing, retains pending answer/draft and permits retry. |
| Input clearing duration | real sent/clear renderer timing matrix covers independent zero/max/toggle/modes and new-draft precedence; existing `sent_messages_flip_in_and_the_composer_flips_away_within_the_cap` retained. Review R1: dedicated replay/disabled/cap tests in `settings_preview::tests`; actual panel Ctrl+P test; in-flight question clear-zero refresh regression. All use real production effect renderers. |
| Sidebar motion | `sidebar_and_screen_controls_drive_live_flaps_with_legacy_zero_compatibility` checks live sidebar boards for independent toggle, all modes and raw speed zero. Review R1: `dedicated_control_previews_replay_and_settle_their_own_effects`, disabled-modes matrix, and `motion_controls_route_to_dedicated_synthetic_previews` prove actual selected-control preview/stop/Ctrl+P through production Board. |
| Screen transitions | live sidebar/screen matrix checks real screen buffer and cap0/10; `legacy_zero_speed_never_schedules_a_tab_transition` checks Shell scheduling. Review R1: dedicated scene/replay/disabled-mode and cap tests plus actual panel Ctrl+P test use production `flap::screen`. |
| Live refresh | `committed_refresh_preserves_dirty_settings_and_unsent_session_state`; `two_clients_preview_only_edited_fields_and_keep_same_field_conflicts` covers actual render, conflict and latest Discard. One-second cadence, malformed bytes unchanged. Review R1: `committed_refresh_settles_clearing_without_redisplaying_an_inflight_answer` covers real file refresh plus native question drawing under sent-disable/clear-zero/Off/Reduced/raw-zero, pending suppression and retry. |
| Tool detail | `conversation_tool_modes_and_line_budget_drive_shared_preview` exercises Collapsed/Compact/Expanded and 2/6-row budgets through the shared formatter; `full_entry_modal_stays_full_under_global_detail_changes` verifies native ordinary/full-entry behavior, Escape, and unchanged draft for every mode. Review R1: `reading_intent_survives_tool_preview_discard_and_saved_navigation` covers offset40 inside long output, source/new-entry arrivals, exact Discard anchor/copy/draft restoration, latent intent after Save, and Home replacing it before later expansion. |
| Reasoning | `reasoning_detail_changes_native_transcript_and_preserves_draft` verifies native folded/expanded preview and rollback; shared sample includes multiline reasoning; full-entry modal remains full under both global values and refresh. Review R1: matching `reading_intent_survives_reasoning_preview_discard_and_saved_navigation` covers nonzero long-reasoning offset, arrivals, source copy/draft, Discard and committed Folded/navigation/expansion behavior. |
| Timestamps | `new_entry_timestamps_are_recorded_and_legacy_times_stay_absent` checks actual receipt range, literal stored UTC time rendering, toggle and omission for legacy records even with old tool times; Codex/Claude receipt tests preserve first time across completion/reconciliation; shared preview shows timestamps. Native mouse-copy reflow regression retains source when timestamps add rows. |
| Auto-scroll | `scroll_policies_follow_streaming_or_preserve_the_reading_anchor` covers all policies, arrival and existing follow state; `manual_scroll_keeps_selected_source_draft_and_anchor_when_content_arrives`; `auto_scroll_preview_keeps_live_anchor_copy_and_draft_until_save` proves draft Always stays synthetic until Save, incoming-entry/Discard anchor+copy+draft preservation, and Save activates live Always. Controller-approved behavioral boundary. Review R1 preserves unclamped source intent while not following; actual Window follow replaces it. Explicit Home after a saved collapse replaces intent, so later expansion cannot jump to an old source offset. |
| Sidebar/Changes visibility | `layout_width_visibility_and_bottom_helper_change_real_panes` checks native visibility; `layout_preview_ticks_never_contaminate_reviews_and_discard_loads_latest` checks tick/refresh/Discard isolation and latest committed values; all-control lifecycle explicitly saves/resets/discards both legacy fields. `opening_changes_moves_a_main_helper_to_right_and_survives_refresh` verifies the Changes shortcut atomically persists its Main->Right move and retains Bottom. |
| Sidebar width | native pane rectangles verify Auto=29 at width120, explicit20/40, and width40 terminal bound18 preserving20 columns for main area. Native Settings layout scene uses the same Ui draw. All control Save/reset/Discard lifecycle plus external refresh width35. |
| Helper placement | native Right/Bottom/Main/narrow rendering preserves composer; browser key/paste/local-mouse protocol routing passes for all three. `explicit_main_overrides_legacy_side_and_remains_keyboard_reachable`; `legacy_move_toggle_persists_a_consistent_right_or_main_after_any_setting` verifies Alt+P Right->Main, Main->Right, Bottom->Main and refreshed persisted agreement. Alt+] visibility meaning unchanged. |
| Session ordering | `session_sort_uses_activity_or_recency_inside_existing_pin_groups` checks exact literal orders for Manual/Recent/ActiveFirst, root and pins, retaining manual order. Shared native sample demonstrates active-vs-recent order. All control Save/reset/Discard lifecycle. |
| Startup tab | `startup_tab_respects_explicit_review_routes_and_last_used` covers both last-tab values, Agents/Reviews decisions and explicit PR policy; actual local constructor under all preferences stays Reviews, avoids unrelated PR inbox loading and retains committed prefs. Shared sample uses the same route policy. Next-launch effect verified without switching current view on Save. |
| Composer maxheight | native composer rectangles at limits3/10/20 (5/12/22 including borders), retained multiline source, terminal bounds; shared native sample shows changed visible row count. All control Save/reset/Discard lifecycle. Review R1 Window height-clamp regression retains the nonzero source offset through height5->30->5 rather than overwriting it with the displayed maximum; existing actual composer-bounds/source checks remain enabled. |
| Paste collapse | actual Ui paste -> Editor under 250/500/1000/Never, Unicode sizes249/250/499/500/999/1000; underlying text and Select-All copied source exact. Selected native sample changes fold token for Never. All control Save/reset/Discard lifecycle. Existing pasted tokens are retained; policy applies to subsequent pastes. |
| Running message behavior | `running_preference_changes_primary_send_but_keeps_explicit_queue` inspects actual enqueued controls for Steer, Queue, explicit Ctrl+Enter Queue, and idle Queue preference. Save/reset/Discard test proves behavioral draft stays committed until Save. `send_presets_and_remaps_dispatch_primary_queue_and_newline` covers both presets. |
| Diff layout | `reviews_render_every_diff_context_layout_and_whitespace_mode`, `changes_native_painter_applies_all_modes_without_modifying_copy`, existing split/unified sample and `diff_preview_uses_context_gutters_wrap_and_selected_whitespace`; canonical `unified` now applies to Changes at width80+, preserving stored defaults. All control Shell Save/reset/Discard/external-refresh regression. |
| Diff wrapping | native Changes painter and shared preview assert actual clipped/wrapped row changes; existing Unicode/tab source-offset renderer coverage retained. `diff_reading_intent_and_selection_survive_wrap_preview_discard`, `changes_reading_and_selection_survive_context_and_wrap_clamps`, and explicit wheel/selection/file/chapter/cursor navigation regression retain source reading/copy until intentional navigation. All control lifecycle passes. |
| Diff line numbers | actual Reviews and Changes source gutters disappear while change markers remain; `diff_preferences_hide_gutters_and_mark_whitespace_without_moving_source_links` verifies tab/Unicode cell17 versus original byte14 definition offsets and unchanged source. Native copy and preview controls pass, including Save/reset/Discard/external refresh. |
| Diff context | all four values asserted in actual Reviews and Changes renderer/painter and shared preview. `presentation_context_keeps_canonical_identity_and_manual_expansion`, metadata marker regression, real-Git `changes_context_reads_matching_source_and_rejects_changed_worktree`, and `asynchronous_context_rejects_stale_results_and_preserves_preview_source_intent` cover canonical IDs, manual extent, pending/incomplete/error replies, source/request guards and arrival during hidden selection. Existing pinned revision/context/navigation suite stays enabled. PR context reuses repo snapshot/fetch and pinned file-context; authenticated GitHub transport is not exercised; owned Git/source orchestration is. |
| Whitespace markers | actual Reviews and Changes Ratatui painters assert Off, SelectedLines only with an active source selection, and AllLines without selection. Markers affect spans only; exact original tabs/spaces copied. Shared preview demonstrates a selected new line and unselected old line. Unicode/tab source-link test and lifecycle pass. |
| Every shortcut action | All 189 actions are enumerated in [action coverage](2026-10-22-settings-action-coverage.md), with concrete named tests and no unresolved placeholders. |
| Shortcut conflicts | actual Settings capture rejects coactive embedded host/Review content chords; normalized-equivalent and concurrent different-action drafts reject at final locked save without changing bytes; disjoint modal/filter assignments remain valid. See capture_rejects_embedded_coactive_actions_but_accepts_disjoint_filter and embedded_review_coactive_bindings_reject_cross_draft_and_normalized_collisions. |
| Question ShiftLeft | question_shift_left_requires_consent_at_actual_composer_dispatch and question_open_rebind_replaces_default_and_preserves_draft_selection; safe capture/sample and reset committed behavior retained. |
| Voice shortcut | reviewed voice fake-transport tests cover release/repeat, disable/change cancellation and no preview activation; final fullsuite retains them. Task6 default/provider manager work remains separate. |
| Send presets | send_presets_and_remaps_dispatch_primary_queue_and_newline plus reviewed companion transaction/custom-truth tests; real send/queue/newline effects retained in final fullsuite. |
| CtrlC precedence | standalone filter and embedded compose selection copies before default/custom quit; explicit Super-copy and Control-quit predicate aliases and precise exclusions preserved; native Control+C independent of semantic copy override. Existing raw Browser/terminal cases retained across placements. |
| Agent provider default | PASS Task6 real NewAgent/first turn for both providers; actual Shell Save/Reset/Discard leaves full running session unchanged; report below |
| Agent model/effort | PASS Task6 exact native fake discovery, in-flight switch, immediate stale Save guard, known-invalid refusal and offline preservation; legacy managers use captured dependent merges |
| Agent repository | PASS Task6 actual defaults lifecycle/first-turn owned Git repository, draft rules target, repository/mode both save orders, clear-to-Automatic independent merge and known-invalid retained-mode refusal |
| Agent starting mode | PASS Task6 exact Codex Plan/Claude six native modes, acknowledgement before prompt, refusal/retry and repository restrictions; no active-session mutation |
| Repository rules action | PASS Task6 actual manager receives draft repository and returns to same unsaved Settings; existing rules/export tests retained |
| Provider connections | PASS Task6 exact native status contracts, malformed/missing/nonzero/timeout, sanitized render, explicit two-provider key dispatch and seven fake native real-PTY outcomes; real OAuth intentionally not performed |
| MCP/Slack managers | PASS retained Task4b2 managers and Shell separate-action/draft reconciliation; full Task6 suite rechecks; real Slack/MCP auth not invoked |
| Remote manager | PASS Task6 exact fake IPC proves first and subsequent browse Status, explicit Enable Open and subsequent authorized renewal; remote group tests all existing navigation/actions/remaps without activating listener |
| Effect labels | Registry audit: ordinary preferences use Immediate on save; five agent-default fields use New sessions; startup tab uses Next launch; connection/rules actions use Applies separately. Per-control lifecycle tests and new-session first-turn tests establish those consumers. |

## Notifications

| Control / behavior | Enabled actual-consumer tests |
|---|---|
| Successful completion, question/approval, error toggles | `reducer_success_notifies_but_failure_and_interruption_are_not_completion`, `a_new_pending_identity_not_a_waiting_edge_triggers_question`, `missing_or_unknown_provider_outcome_and_old_json_are_not_success`, `event_toggles_all_scope_soundoff_and_inapp_sound_use_saved_settings` |
| All/background/unfocused scope and multiple clients | `background_and_unfocused_scopes_respect_multiple_clients_expiry_and_close`, `presence_renews_during_continuous_input_and_tracks_review_and_overlay_visibility`, `saved_changes_visibility_suppresses_only_the_conversation_actually_drawn` |
| In-app/Desktop/Both and privacy | `real_store_reducer_path_is_async_deduplicated_and_privacy_limited`, `notification_test_delivery_modes_keep_toasts_local_and_preserve_draft_session_and_feed` |
| Quiet, presets, reset/discard | `quiet_keeps_in_app_and_consumes_event_without_native_or_sound`, `saved_presets_and_reset_or_discard_drive_real_event_policy` |
| Independent sound and suppressed Test | `sound_only_test_never_creates_a_desktop_toast_or_requests_permission`, `standalone_audio_process_receives_only_generated_wav_and_reports_playback_failure`, `preview_respects_soundoff_quiet_and_disabled_selected_event` |
| Explicit permission timing, denial and unavailable state | `explicit_successful_save_authorizes_but_draft_discard_refresh_and_noop_do_not`, `discard_external_enable_failed_save_and_startup_never_prompt`, `actual_test_action_uses_draft_sound_and_shows_native_denial_without_changing_choices`, `queued_explicit_enable_survives_client_close_without_a_ui_tick` |
| Dedupe/restart/no replay | `no_historical_feed_or_delivery_replay_after_reconnect_restart_or_disabled_toggle`, `question_request_ids_can_be_reused_in_subsequent_turns`, `guide_and_conflict_completion_edges_have_durable_distinct_identities`, `reconnect_resets_feed_cursor_instead_of_replaying_missed_activity`; subprocess `cli/tests/notifications.rs` |
| Locking and persistent errors | `blocked_native_delivery_never_holds_session_or_presence_locks`, `corrupt_dispatch_markers_disable_delivery_and_remain_visible_on_status`, `opening_status_preserves_the_last_native_delivery_failure` |
| Shared production preview and idle expiry | `notification_samples_render_production_toast_borders_severity_and_wrapping`, `explicit_inapp_test_renders_real_toast_without_mutating_existing_notice_or_session`, `notification_test_expiry_repaints_idle_motion_off_without_an_animation_timer` |
| Native boundary and helper discovery | `mac_helper_protocol_is_structured_and_reports_denied_or_malformed_replies`, `helper_follows_executable_symlink_into_relocated_cellar_and_archive`, Linux `private_bus_native_notification_payload_and_missing_bus`; actual packaged macOS helper status/error smoke |

## Agent defaults and connection actions

| Control | Effect / draft lifecycle | Production consumer verification and error evidence |
|---|---|---|
|Provider|New sessions; Save/Reset/Discard; Automatic model still uses explicit provider|`new_defaults_choose_explicit_provider_and_acknowledge_mode_before_first_turn` drives real service IPC/new session/first turn for Codex and Claude with model=None; `saving_agent_defaults_changes_only_future_sessions_and_preserves_running_session` drives actual Shell Ctrl+S/Ctrl+R/Ctrl+D/reset+Save for every default and compares complete running session serialization including completion outcome. Legacy raw Launch tests remain in permissions suite.|
|Model|New sessions; native choices; invalid value retained for repair|`native_async_switching_and_failure_preserve_the_current_draft` starts slow fake Codex discovery then switches to Claude, verifies its real model/effort choices and failure preservation; `saving_before_next_tick_ignores_previous_provider_choices` covers immediate Save; explicit mismatch storage regression refuses bytes.|
|Effort|New sessions; model-specific list, Automatic delegates native; Save/Reset/Discard|Same async exact native response test; `discovered_defaults_reject_invalid_selection_without_losing_offline_values` exercises supported/unsupported choice and offline preservation; active-session lifecycle test verifies all fields.|
|Repository|New sessions; Automatic cwd; current draft rules target|First-turn service integration passes owned Git repository; actual defaults lifecycle and `repository_edit_cannot_save_a_known_unavailable_retained_starting_mode`; directional two-client regressions both save orders and clear-to-Automatic merge.|
|Starting mode|Native modes only; pending until acknowledged before first turn; no active mutation|Codex Plan exact turn/start collaborationMode; Claude Plan plus default/acceptEdits/auto/dontAsk/bypassPermissions exact native argv; explicit bypass flag only for bypass; committed repository restriction refuses Claude CLI start. `rejected_starting_mode_stays_pending_and_sends_no_first_turn` verifies error, zero turn/start, pending retry and successful acknowledgement.|
|Rules|Separate explicit manager, not preference Save/Reset/Discard/export|`rules_manager_uses_settings_draft_repository_and_returns_to_same_draft` checks real manager target, Escape/draft retention and untouched saved repository; existing rules manager shortcuts/save/conflict/export exclusions remain full-suite coverage.|
|Provider status/Refresh|Read-only asynchronous status; no draft persistence; explicit refresh retries|`capability_and_status_subprocess_contracts` validates exact fake argv and JSON-lines requests; `native_status_contracts_are_sanitized_and_exit_aware`; `native_status_absent_malformed_signed_out_and_timeout_are_distinct` exercises real missing executable, nonzero false JSON, malformed output and 10s cancellation (<12s). Async renderer checks Connected/separate labels and no fixture email/key.|
|Codex/Claude sign in|Separate explicit native flow; settings draft survives all outcomes; no effect from browsing/reset/discard|`explicit_sign_in_handoff_keeps_unsaved_draft_and_separate_status_actions` activates both via actual Settings Tab/Enter, takes each once, checks cancel/draft/config and status-only refresh. `auth_terminal_lifecycle_uses_real_pty` runs seven real PTY cases: Codex+Claude success (real stdin), native exit7, Ctrl+C, missing executable, HUP, TERM; checks foreground process group, cooked canonical/echo during auth, restored raw/alternate/paste and bounded return. `native_foreground_success_failure_and_cancellation_are_bounded` also covers owned descendant cancellation.|
|MCP/Slack|Existing explicit managers; separate apply; unsaved Settings retained; prerequisite notice|Existing Task4b2 manager tests and Shell action reconciliation remain authoritative and run in full suite; no actual auth, Slack/Keychain writes or MCP service activation during tests. Native Slack remains macOS-only with existing unavailable behavior elsewhere.|
|Remote|Status-only browse and polls; explicit Enable/Pair; existing Disable/remove/copy/navigation|`settings_remote_browse_requests_only_status_and_keeps_explicit_enable` uses exact fake Unix IPC for first Status, expired status-only poll still Status, explicit Enable Open, and authorized renewal Open. Full `remote_control::` group covers effective remaps, old-alias removal, guards and rendered hints without a real listener.|

## Limits and retained behavior

- Five pre-existing tests remain ignored: live session title, live guide
  generation, live voice negotiation, local browser smoke and MCP visual export.
  No newly added Settings test is ignored.
- Real OAuth account login, microphone capture, native permission prompts,
  desktop presentation and audible playback were not invoked. Native helper
  status/error execution, private Linux D-Bus transport, generated WAV/fake audio
  executable, and fake-provider first-turn acknowledgements are tested separately.
- macOS helpers are ad-hoc signed, not Developer ID signed/notarized. Minimum OS,
  architecture and dependencies are inspected; this machine does not emulate
  every supported macOS release or Linux desktop environment.
- Ctrl+Enter requires distinct terminal reporting; existing modifyOtherKeys
  `ESC[27;5;13~` is unsupported. Embedded browser physical BackTab is not encoded;
  Tab+Shift is. Explicit modifier matching replaces incidental loose variants.
- Notification consumption is persisted before delivery to prevent replay;
  crashing between those operations can lose an alert. Focus uses expiring
  leases and the in-app feed is bounded.
- No real provider turn, GitHub write, Slack message, pairing, user configuration
  mutation or installation was performed by the added tests. Exact external
  command boundaries are fake; owned Git/filesystem actions are real.

## Review and failure provenance

The primary reviewed the remaining defaults/provider capability handling,
transaction dependencies, native sign-in lifecycle, remote status/explicit pairing,
release workflow/helper paths and full acceptance inventory. Prior feature slices
had reviewed fixes for real rendering, reading/copy anchors, shortcut contexts,
mouse hit targets, native notification suppression and idle expiry. No further
scope was added after the user requested the primary work alone.

Task6's initial full run found three legacy defaults fixtures treating unsaved
values as persisted baselines; fixtures were corrected while retaining their
shortcut/mouse assertions. Its definitive ordered run passed 859/0/5. The PTY wrapper plus child increased the native checkpoint to 861/0/5; the final
interrupted-read regression brings the final result to 862/0/5.

The first ARM64 package smoke caught an incorrect `lipo` argument order, followed
by an overly broad minimum-OS parser that also read the linker's version. Both
verification defects were corrected; a regression accepts minimum macOS 11 with
a newer linker while rejecting a real macOS 12 requirement. Rust was unchanged.

The initial Linux container ran as root, so the real read-only-directory test
correctly could not provoke a write failure. A UID-only attempt then exposed the missing passwd entry required by D-Bus and
an inaccessible root login home. Linux was rerun through a real isolated
`difutest` login (UID/GID 1000), after all three environment checks passed.
No HOME/CODEX_HOME override or product/test weakening was used. Existing services were preserved. A later integration timeout was isolated to
Git worktree creation without a fixture identity in the network-disabled image.
The same actual service test passed after configuring container-only system Git
identity (`Fixture`, `fixture@example.invalid`). No source assertion or timeout
was weakened. The final container setup includes that identity and a real login
user. A RAM-filesystem comparison did not fix the issue and was discarded.
The original attempts remain in task-local logs; only completed final command
results count as passing evidence.

[All 190 shortcut actions](2026-10-22-settings-action-coverage.md) and
[the ordered decision ledger](2026-10-22-settings-decisions.md) are retained.


## Final runtime correction

The emulated Intel Linux parallel run exposed `Interrupted` while receiving a
service frame, followed by broken pipes in two real action-boundary tests.
`read_line` propagated the transient I/O error and dropped the connection.
A deterministic regression first failed with `operation interrupted`. The narrow
fix retries only that read within the same frame, retaining partial bytes;
it never sends another request. The regression also checks two consecutive frames,
EOF, WouldBlock, timeout and connection reset. Both complete parallel Linux suites
and both complete macOS suites passed afterward. Earlier package/test checkpoints
were superseded by the final source and rebuilt archives reported above.

Focused RED/GREEN logs:
`/tmp/difu-settings-interrupted-read-{red,green}.log`.


## Local testing follow-up: navigation and Motion previews

The user-tested Tab navigation now cycles Search, Sections and Settings, with
Shift+Tab reversing the order. The user then reported that individual Motion
previews did not react and that the shortcut footer was hard to read.

Regression tests reproduced expired preview clocks, missing restart after an
edit, and unrelated effects masking a disabled control. Selecting a new control
or changing motion values now restarts its sample; sent messages, replies and
timing controls use focused scenes through the production effect renderers.
Replay reveals the sample again. Zero durations and disabled dependencies show
an explanation without changing the saved settings. The footer uses styled,
spaced commands, wraps whole shortcuts and displays Replay or Test contextually.

Native Apple Silicon verification after these follow-ups:
- Formatting and strict all-target Clippy passed.
- `cargo test --locked`: **874 passed, zero failed, five existing tests ignored**.
- All nine Motion controls progress, finish and replay; zero durations, toggles,
  reply speed and delay caps have regression coverage.
- The real PTY lifecycle test covers Tab, Shift+Tab, arrows, search and Replay.
- Footer visibility passes at 38, 80, 130 and 220 columns; terminal-buffer renders
  were also reviewed locally. No screenshot assets are included in the repository.
- The release build passed and `/private/tmp/difu-settings-preview/difu --version`
  returned `difu 0.26.0`; its existing notification helper is packaged beside it.

The earlier four-platform archive results above describe the original feature
build. These follow-ups were rebuilt and tested on native Apple Silicon only.
Logs: `/private/tmp/difu-motion-{red,green,settings,clippy,full-tests,build}.log`.

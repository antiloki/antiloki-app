//! antiloki-core-rust — the Rust twin of antiloki-core.
//!
//! THE MAPPING RULE (MIRROR.json): every core code file grows its twin under src/ at the SAME
//! relative path with the SAME file name (kebab-case preserved via #[path]). A module is
//! admitted ONLY when the crossing's conformance proves it equal to the JS original over
//! shared fixtures.
//!
//! ⚖ NAMES ARE CONTRACT (operator 2026-07-28, vital): NO renames — not functions, not
//! variables, not files. The substrate dispatches by name (actions → skills → exports); a
//! rename is a breakage, not a style choice. Rust's snake_case convention is OVERRULED here:
//! `#![allow(non_snake_case)]` is policy, not debt. The only permitted transform is the module
//! identifier (language-forced: `canonical-json` cannot be a Rust ident) — and the file on
//! disk still keeps its true name.
// QUALITY: 90/100 — module manifest: doctrine header + managed block, nothing clever

#![allow(non_snake_case)]
#![allow(non_upper_case_globals)] // pub static ErrorCodes/CONTRACTS/EMOTION… keep exact JS names

pub mod proc;
pub mod js_math; // sentinel-side translation memory (not a core twin — its name is ours)
pub mod db_json; // sentinel-side db-lane helper (rows → JSON, like node:sqlite .all())
pub mod clock; // sentinel-side: the ONE frozen-vs-wall-clock decision (M4b-2)
pub mod test_env_lock; // sentinel-side shared test mutex (ANTILOKI_HOME etc — see module doc)

#[path = "skills/engine/workspace-root.rs"]
pub mod engine_workspace_root;
#[path = "skills/engine/canonical-json.rs"]
pub mod canonical_json;

#[path = "skills/system/_helpers/wav-tools.rs"]
pub mod wav_tools;

// ── EXPANDER MANAGED (expand.mjs) — do not hand-edit this block ──
pub mod conform_registry;
pub mod skill_registry; // GENERATED name→fn dispatch table (runtime cutover) — gen-skill-registry.mjs
#[path = "skills/codebase/code-run.rs"]
pub mod codebase_code_run;
#[path = "skills/codebase/code-signals-snapshot.rs"]
pub mod codebase_code_signals_snapshot;
#[path = "skills/driver/driver-schedule.rs"]
pub mod driver_driver_schedule;
#[path = "skills/system/driver-queue.rs"]
pub mod system_driver_queue;
#[path = "skills/agents/in/interviewer-bootstrap-context.rs"]
pub mod agents_in_interviewer_bootstrap_context;
#[path = "skills/engine/hook-cache.rs"]
pub mod engine_hook_cache;
#[path = "skills/card/card-gate-pl-ratify.rs"]
pub mod card_card_gate_pl_ratify;
#[path = "skills/agents/pl/pl-session-reject.rs"]
pub mod agents_pl_pl_session_reject;
#[path = "skills/contract/contract-gate-human-reject.rs"]
pub mod contract_contract_gate_human_reject;
#[path = "skills/agents/tl/contract-gate-tl-sign.rs"]
pub mod agents_tl_contract_gate_tl_sign;
#[path = "skills/engine/runtime.rs"]
pub mod engine_runtime;
#[path = "skills/entities/law-surface-for-context.rs"]
pub mod entities_law_surface_for_context;
#[path = "skills/codebase/atlas-metrics.rs"]
pub mod codebase_atlas_metrics;
#[path = "skills/constitution/constitution-file.rs"]
pub mod constitution_constitution_file;
#[path = "skills/entities/card-rewind-to-stage.rs"]
pub mod entities_card_rewind_to_stage;
#[path = "skills/audit/card-audit-determinism.rs"]
pub mod audit_card_audit_determinism;
#[path = "skills/codebase/data-story-to-card.rs"]
pub mod codebase_data_story_to_card;
#[path = "skills/codebase/codebase-onboard-start.rs"]
pub mod codebase_codebase_onboard_start;
#[path = "skills/codebase/_helpers/web-cache.rs"]
pub mod codebase__helpers_web_cache;
#[path = "skills/constitution/_helpers/fix-hints.rs"]
pub mod constitution__helpers_fix_hints;
#[path = "skills/system/byok-status.rs"]
pub mod system_byok_status;
#[path = "skills/entities/card-restart-from-failure.rs"]
pub mod entities_card_restart_from_failure;
#[path = "skills/entities/card-rewind-rearm.rs"]
pub mod entities_card_rewind_rearm;
#[path = "skills/ai/learn-insight.rs"]
pub mod ai_learn_insight;
#[path = "skills/agents/in/investigate-research.rs"]
pub mod agents_in_investigate_research;
#[path = "skills/bench/bench-skill-latency.rs"]
pub mod bench_bench_skill_latency;
#[path = "skills/cockpit/cockpit-pane-update.rs"]
pub mod cockpit_cockpit_pane_update;
#[path = "skills/ai/assemble-context-envelope.rs"]
pub mod ai_assemble_context_envelope;
#[path = "skills/codebase/atlas-entry-points.rs"]
pub mod codebase_atlas_entry_points;
#[path = "skills/audit/law-hook-no-lone-skill-dispatch.rs"]
pub mod audit_law_hook_no_lone_skill_dispatch;
#[path = "skills/bench/bench-chain-verify.rs"]
pub mod bench_bench_chain_verify;
#[path = "skills/audit/law-hook-no-drift-on-audit-commit.rs"]
pub mod audit_law_hook_no_drift_on_audit_commit;
#[path = "skills/codebase/atlas-smell-to-card.rs"]
pub mod codebase_atlas_smell_to_card;
#[path = "skills/agents/in/interviewer-workspace-brief.rs"]
pub mod agents_in_interviewer_workspace_brief;
#[path = "skills/blueprint/tl-contract-assemble.rs"]
pub mod blueprint_tl_contract_assemble;
#[path = "skills/entities/brain-session-start.rs"]
pub mod entities_brain_session_start;
#[path = "skills/cockpit/_helpers/context-brief.rs"]
pub mod cockpit__helpers_context_brief;
#[path = "skills/notebooks/notebook-bundle.rs"]
pub mod notebooks_notebook_bundle;
#[path = "skills/constitution/constitution-report.rs"]
pub mod constitution_constitution_report;
#[path = "skills/render/excalidraw-driver.rs"]
pub mod render_excalidraw_driver;
#[path = "skills/render/excalidraw-bundles.rs"]
pub mod render_excalidraw_bundles;
#[path = "skills/constitution/constitution-agents.rs"]
pub mod constitution_constitution_agents;
#[path = "skills/engine/open-db/seed.rs"]
pub mod engine_open_db_seed;
#[path = "skills/render/excalidraw-meta.rs"]
pub mod render_excalidraw_meta;
#[path = "skills/render/excalidraw-fullflow.rs"]
pub mod render_excalidraw_fullflow;
#[path = "skills/render/excalidraw-rag.rs"]
pub mod render_excalidraw_rag;
#[path = "skills/system/drift-questionnaire.rs"]
pub mod system_drift_questionnaire;
#[path = "skills/cockpit/cockpit-context-preview.rs"]
pub mod cockpit_cockpit_context_preview;
#[path = "skills/agents/dev/apply-section-patch.rs"]
pub mod agents_dev_apply_section_patch;
#[path = "skills/agents/dev/dev-consume-qa-suggestions.rs"]
pub mod agents_dev_dev_consume_qa_suggestions;
#[path = "skills/agents/dev/dev-contract-load.rs"]
pub mod agents_dev_dev_contract_load;
#[path = "skills/agents/dev/dev-plan-spec.rs"]
pub mod agents_dev_dev_plan_spec;
#[path = "skills/agents/dev/dev-verify-chain.rs"]
pub mod agents_dev_dev_verify_chain;
#[path = "skills/agents/git-manager/git-manager-merge-gate.rs"]
pub mod agents_git_manager_git_manager_merge_gate;
#[path = "skills/agents/god/god-ai-provider-set-active.rs"]
pub mod agents_god_god_ai_provider_set_active;
#[path = "skills/agents/god/god-card-driver-intent-set.rs"]
pub mod agents_god_god_card_driver_intent_set;
#[path = "skills/agents/god/god-card-skip-pl-set.rs"]
pub mod agents_god_god_card_skip_pl_set;
#[path = "skills/agents/god/god-driver-halt.rs"]
pub mod agents_god_god_driver_halt;
#[path = "skills/agents/god/god-driver-resume.rs"]
pub mod agents_god_god_driver_resume;
#[path = "skills/agents/in/_helpers/session-intelligence.rs"]
pub mod agents_in__helpers_session_intelligence;
#[path = "skills/agents/in/book-atlas.rs"]
pub mod agents_in_book_atlas;
#[path = "skills/agents/in/chat-tool-belt.rs"]
pub mod agents_in_chat_tool_belt;
#[path = "skills/agents/in/interviewer-card-proposal-normalize.rs"]
pub mod agents_in_interviewer_card_proposal_normalize;
#[path = "skills/agents/in/interviewer-classify.rs"]
pub mod agents_in_interviewer_classify;
#[path = "skills/agents/in/interviewer-confirm-gate.rs"]
pub mod agents_in_interviewer_confirm_gate;
#[path = "skills/agents/in/interviewer-context-bundle.rs"]
pub mod agents_in_interviewer_context_bundle;
#[path = "skills/agents/in/interviewer-error-resolve.rs"]
pub mod agents_in_interviewer_error_resolve;
#[path = "skills/agents/in/interviewer-pl-clarify-floor.rs"]
pub mod agents_in_interviewer_pl_clarify_floor;
#[path = "skills/agents/in/interviewer-role-shorthand-parse.rs"]
pub mod agents_in_interviewer_role_shorthand_parse;
#[path = "skills/agents/in/interviewer-route-tier.rs"]
pub mod agents_in_interviewer_route_tier;
#[path = "skills/agents/in/interviewer-route.rs"]
pub mod agents_in_interviewer_route;
#[path = "skills/agents/in/interviewer-session-consolidate.rs"]
pub mod agents_in_interviewer_session_consolidate;
#[path = "skills/agents/in/interviewer-session-patterns-detect.rs"]
pub mod agents_in_interviewer_session_patterns_detect;
#[path = "skills/agents/in/interviewer-session-stale-end.rs"]
pub mod agents_in_interviewer_session_stale_end;
#[path = "skills/agents/in/interviewer-topic-anchor.rs"]
pub mod agents_in_interviewer_topic_anchor;
#[path = "skills/agents/in/interviewer-validate-suggestion.rs"]
pub mod agents_in_interviewer_validate_suggestion;
#[path = "skills/agents/in/interviewer-view-intent-infer.rs"]
pub mod agents_in_interviewer_view_intent_infer;
#[path = "skills/agents/in/interviewer-widget-offers.rs"]
pub mod agents_in_interviewer_widget_offers;
#[path = "skills/agents/in/paper-store.rs"]
pub mod agents_in_paper_store;
#[path = "skills/agents/in/session-create.rs"]
pub mod agents_in_session_create;
#[path = "skills/agents/in/skill-library.rs"]
pub mod agents_in_skill_library;
#[path = "skills/agents/in/turn-widget-save.rs"]
pub mod agents_in_turn_widget_save;
#[path = "skills/agents/in/widget-save-to-notebook.rs"]
pub mod agents_in_widget_save_to_notebook;
#[path = "skills/agents/keeper/keeper-journey-append.rs"]
pub mod agents_keeper_keeper_journey_append;
#[path = "skills/agents/keeper/keeper-memory-capture-correction.rs"]
pub mod agents_keeper_keeper_memory_capture_correction;
#[path = "skills/agents/keeper/keeper-review.rs"]
pub mod agents_keeper_keeper_review;
#[path = "skills/agents/man/man-audit-compliance.rs"]
pub mod agents_man_man_audit_compliance;
#[path = "skills/agents/man/man-audit-drift.rs"]
pub mod agents_man_man_audit_drift;
#[path = "skills/agents/man/man-compose-digital-print.rs"]
pub mod agents_man_man_compose_digital_print;
#[path = "skills/agents/pl/pl-clarify-state.rs"]
pub mod agents_pl_pl_clarify_state;
#[path = "skills/agents/pl/pl-session-answer-record.rs"]
pub mod agents_pl_pl_session_answer_record;
#[path = "skills/agents/pl/pl-session-approve.rs"]
pub mod agents_pl_pl_session_approve;
#[path = "skills/agents/pl/pl-session-finalize.rs"]
pub mod agents_pl_pl_session_finalize;
#[path = "skills/agents/pl/pl-session-start.rs"]
pub mod agents_pl_pl_session_start;
#[path = "skills/agents/qa/qa-plan-coverage.rs"]
pub mod agents_qa_qa_plan_coverage;
#[path = "skills/agents/qa/qa-swarm-coverage.rs"]
pub mod agents_qa_qa_swarm_coverage;
#[path = "skills/agents/tl/_helpers/author-form.rs"]
pub mod agents_tl__helpers_author_form;
#[path = "skills/agents/tl/tl-contract-amend.rs"]
pub mod agents_tl_tl_contract_amend;
#[path = "skills/ai/ai-call-postmortem.rs"]
pub mod ai_ai_call_postmortem;
#[path = "skills/ai/ai-outcome-report.rs"]
pub mod ai_ai_outcome_report;
#[path = "skills/ai/ai-route.rs"]
pub mod ai_ai_route;
#[path = "skills/ai/bandit-pick.rs"]
pub mod ai_bandit_pick;
#[path = "skills/ai/failure-classes.rs"]
pub mod ai_failure_classes;
#[path = "skills/ai/prompt-add.rs"]
pub mod ai_prompt_add;
#[path = "skills/ai/prompt-find.rs"]
pub mod ai_prompt_find;
#[path = "skills/ai/run-session.rs"]
pub mod ai_run_session;
#[path = "skills/ai/switcher-mode.rs"]
pub mod ai_switcher_mode;
#[path = "skills/audit/tui-god-token.rs"]
pub mod audit_tui_god_token;
#[path = "skills/audit/verify-chain.rs"]
pub mod audit_verify_chain;
#[path = "skills/audit/timeline-stats.rs"]
pub mod audit_timeline_stats;
#[path = "skills/audit/brain-decisions-vacuum.rs"]
pub mod audit_brain_decisions_vacuum;
#[path = "skills/audit/law-hook-mutation-via-with-audit.rs"]
pub mod audit_law_hook_mutation_via_with_audit;
#[path = "skills/audit/law-hook-no-destructive-git.rs"]
pub mod audit_law_hook_no_destructive_git;
#[path = "skills/audit/law-hook-no-direct-audit-write.rs"]
pub mod audit_law_hook_no_direct_audit_write;
#[path = "skills/bench/bench-report.rs"]
pub mod bench_bench_report;
#[path = "skills/bench/bench-stats.rs"]
pub mod bench_bench_stats;
#[path = "skills/blueprint/_helpers/snippets-hash.rs"]
pub mod blueprint__helpers_snippets_hash;
#[path = "skills/blueprint/block-shape.rs"]
pub mod blueprint_block_shape;
#[path = "skills/blueprint/blueprint-compose.rs"]
pub mod blueprint_blueprint_compose;
#[path = "skills/blueprint/blueprint-coverage.rs"]
pub mod blueprint_blueprint_coverage;
#[path = "skills/blueprint/snippet-checks.rs"]
pub mod blueprint_snippet_checks;
#[path = "skills/blueprint/blueprint-get.rs"]
pub mod blueprint_blueprint_get;
#[path = "skills/blueprint/blueprint-lint.rs"]
pub mod blueprint_blueprint_lint;
#[path = "skills/blueprint/contract-advance-status.rs"]
pub mod blueprint_contract_advance_status;
#[path = "skills/blueprint/contract-lint.rs"]
pub mod blueprint_contract_lint;
#[path = "skills/blueprint/screening-gate.rs"]
pub mod blueprint_screening_gate;
#[path = "skills/blueprint/snippet-lint.rs"]
pub mod blueprint_snippet_lint;
#[path = "skills/blueprint/stub-checks.rs"]
pub mod blueprint_stub_checks;
#[path = "skills/bugfinder/_helpers/grounding.rs"]
pub mod bugfinder__helpers_grounding;
#[path = "skills/bugfinder/_helpers/schedule-math.rs"]
pub mod bugfinder__helpers_schedule_math;
#[path = "skills/bugfinder/_helpers/schema.rs"]
pub mod bugfinder__helpers_schema;
#[path = "skills/bugfinder/bugfinder-classify.rs"]
pub mod bugfinder_bugfinder_classify;
#[path = "skills/bugfinder/bugfinder-report.rs"]
pub mod bugfinder_bugfinder_report;
#[path = "skills/card/card-draft-auto.rs"]
pub mod card_card_draft_auto;
#[path = "skills/card/card-templates.rs"]
pub mod card_card_templates;
#[path = "skills/cockpit/_helpers/scope.rs"]
pub mod cockpit__helpers_scope;
#[path = "skills/cockpit/cockpit-blackboard-post.rs"]
pub mod cockpit_cockpit_blackboard_post;
#[path = "skills/cockpit/cockpit-blackboard-read.rs"]
pub mod cockpit_cockpit_blackboard_read;
#[path = "skills/cockpit/cockpit-overview.rs"]
pub mod cockpit_cockpit_overview;
#[path = "skills/cockpit/cockpit-pane-claim.rs"]
pub mod cockpit_cockpit_pane_claim;
#[path = "skills/cockpit/cockpit-schema.rs"]
pub mod cockpit_cockpit_schema;
#[path = "skills/cockpit/cockpit-session-audit.rs"]
pub mod cockpit_cockpit_session_audit;
#[path = "skills/cockpit/cockpit-session-create.rs"]
pub mod cockpit_cockpit_session_create;
#[path = "skills/cockpit/cockpit-session-close.rs"]
pub mod cockpit_cockpit_session_close;
#[path = "skills/cockpit/cockpit-session-get.rs"]
pub mod cockpit_cockpit_session_get;
#[path = "skills/codebase/_helpers/browser-capture.rs"]
pub mod codebase_helpers_browser_capture;
#[path = "skills/codebase/_helpers/magazine-db.rs"]
pub mod codebase__helpers_magazine_db;
#[path = "skills/codebase/_helpers/web-research.rs"]
pub mod codebase__helpers_web_research;
#[path = "skills/codebase/atlas-build.rs"]
pub mod codebase_atlas_build;
#[path = "skills/codebase/atlas-file-snapshot-get.rs"]
pub mod codebase_atlas_file_snapshot_get;
#[path = "skills/codebase/atlas-fold.rs"]
pub mod codebase_atlas_fold;
#[path = "skills/codebase/atlas-get.rs"]
pub mod codebase_atlas_get;
#[path = "skills/codebase/atlas-report.rs"]
pub mod codebase_atlas_report;
#[path = "skills/codebase/atlas-report-latest.rs"]
pub mod codebase_atlas_report_latest;
#[path = "skills/codebase/atlas-subgraph.rs"]
pub mod codebase_atlas_subgraph;
#[path = "skills/codebase/atlas-score.rs"]
pub mod codebase_atlas_score;
#[path = "skills/codebase/atlas-smells.rs"]
pub mod codebase_atlas_smells;
#[path = "skills/codebase/atlas-test-map.rs"]
pub mod codebase_atlas_test_map;
#[path = "skills/codebase/atlas-trace-path.rs"]
pub mod codebase_atlas_trace_path;
#[path = "skills/codebase/binding-resolve.rs"]
pub mod codebase_binding_resolve;
#[path = "skills/codebase/blast-radius.rs"]
pub mod codebase_blast_radius;
#[path = "skills/codebase/blueprint-project.rs"]
pub mod codebase_blueprint_project;
#[path = "skills/codebase/course-get.rs"]
pub mod codebase_course_get;
#[path = "skills/codebase/code-decorators-doc.rs"]
pub mod codebase_code_decorators_doc;
#[path = "skills/codebase/code-describe-gather.rs"]
pub mod codebase_code_describe_gather;
#[path = "skills/codebase/code-description-get.rs"]
pub mod codebase_code_description_get;
#[path = "skills/codebase/code-digest-latest.rs"]
pub mod codebase_code_digest_latest;
#[path = "skills/codebase/code-docs-build.rs"]
pub mod codebase_code_docs_build;
#[path = "skills/codebase/code-history.rs"]
pub mod codebase_code_history;
#[path = "skills/codebase/code-patterns.rs"]
pub mod codebase_code_patterns;
#[path = "skills/codebase/code-review-history.rs"]
pub mod codebase_code_review_history;
#[path = "skills/codebase/code-schema.rs"]
pub mod codebase_code_schema;
#[path = "skills/codebase/code-scope-set.rs"]
pub mod codebase_code_scope_set;
#[path = "skills/codebase/context-grounding-check.rs"]
pub mod codebase_context_grounding_check;
#[path = "skills/codebase/stack-facts.rs"]
pub mod codebase_stack_facts;
#[path = "skills/codebase/toolchain-checks.rs"]
pub mod codebase_toolchain_checks;
#[path = "skills/codebase/typecheck-detect.rs"]
pub mod codebase_typecheck_detect;
#[path = "skills/codebase/lint-detect.rs"]
pub mod codebase_lint_detect;
#[path = "skills/codebase/lint-eslint.rs"]
pub mod codebase_lint_eslint;
#[path = "skills/codebase/code-signals-read.rs"]
pub mod codebase_code_signals_read;
#[path = "skills/codebase/codebase-context.rs"]
pub mod codebase_codebase_context;
#[path = "skills/codebase/codebase-file-read.rs"]
pub mod codebase_codebase_file_read;
#[path = "skills/codebase/file-internal-diagram.rs"]
pub mod codebase_file_internal_diagram;
#[path = "skills/codebase/codebase-onboard-control.rs"]
pub mod codebase_codebase_onboard_control;
#[path = "skills/codebase/codebase-onboard-latest.rs"]
pub mod codebase_codebase_onboard_latest;
#[path = "skills/codebase/codebase-onboard-status.rs"]
pub mod codebase_codebase_onboard_status;
#[path = "skills/codebase/codebase-parse-health.rs"]
pub mod codebase_codebase_parse_health;
#[path = "skills/codebase/context-bundle-get.rs"]
pub mod codebase_context_bundle_get;
#[path = "skills/codebase/context-bundle-persist.rs"]
pub mod codebase_context_bundle_persist;
#[path = "skills/codebase/context-contracts.rs"]
pub mod codebase_context_contracts;
#[path = "skills/codebase/data-story-add.rs"]
pub mod codebase_data_story_add;
#[path = "skills/codebase/data-story-delete.rs"]
pub mod codebase_data_story_delete;
#[path = "skills/codebase/data-story-get.rs"]
pub mod codebase_data_story_get;
#[path = "skills/codebase/data-story-list.rs"]
pub mod codebase_data_story_list;
#[path = "skills/codebase/data-story-trace.rs"]
pub mod codebase_data_story_trace;
#[path = "skills/codebase/data-story-update.rs"]
pub mod codebase_data_story_update;
#[path = "skills/codebase/dep-resolve.rs"]
pub mod codebase_dep_resolve;
#[path = "skills/codebase/design-audit.rs"]
pub mod codebase_design_audit;
#[path = "skills/codebase/design-snapshots-get.rs"]
pub mod codebase_design_snapshots_get;
#[path = "skills/codebase/design-system-get.rs"]
pub mod codebase_design_system_get;
#[path = "skills/codebase/digest-spec.rs"]
pub mod codebase_digest_spec;
#[path = "skills/codebase/entry-points-get.rs"]
pub mod codebase_entry_points_get;
#[path = "skills/codebase/feature-inventory.rs"]
pub mod codebase_feature_inventory;
#[path = "skills/codebase/flow-resolve.rs"]
pub mod codebase_flow_resolve;
#[path = "skills/codebase/instrument-detectors.rs"]
pub mod codebase_instrument_detectors;
#[path = "skills/codebase/style-locate.rs"]
pub mod codebase_style_locate;
#[path = "skills/codebase/substrate-telemetry.rs"]
pub mod codebase_substrate_telemetry;
#[path = "skills/codebase/symbol-find.rs"]
pub mod codebase_symbol_find;
#[path = "skills/codebase/symbol-table-build.rs"]
pub mod codebase_symbol_table_build;
#[path = "skills/codebase/test-smoke-gen.rs"]
pub mod codebase_test_smoke_gen;
#[path = "skills/codebase/ui-stories-get.rs"]
pub mod codebase_ui_stories_get;
#[path = "skills/codebase/ui-story-delete.rs"]
pub mod codebase_ui_story_delete;
#[path = "skills/codebase/ui-story-spec.rs"]
pub mod codebase_ui_story_spec;
#[path = "skills/codebase/ui-surface-map.rs"]
pub mod codebase_ui_surface_map;
#[path = "skills/constitution/_helpers/categories.rs"]
pub mod constitution__helpers_categories;
#[path = "skills/constitution/_helpers/law-catalog.rs"]
pub mod constitution__helpers_law_catalog;
#[path = "skills/constitution/_helpers/packs.rs"]
pub mod constitution__helpers_packs;
#[path = "skills/constitution/_helpers/schema.rs"]
pub mod constitution__helpers_schema;
#[path = "skills/constitution/_helpers/seed-laws.rs"]
pub mod constitution__helpers_seed_laws;
#[path = "skills/constitution/constitution-estimate.rs"]
pub mod constitution_constitution_estimate;
#[path = "skills/constitution/constitution-classify.rs"]
pub mod constitution_constitution_classify;
#[path = "skills/constitution/constitution-paper.rs"]
pub mod constitution_constitution_paper;
#[path = "skills/contract/contract-add.rs"]
pub mod contract_contract_add;
#[path = "skills/contract/contract-admin-reject.rs"]
pub mod contract_contract_admin_reject;
#[path = "skills/contract/contract-gate-human-approve.rs"]
pub mod contract_contract_gate_human_approve;
#[path = "skills/contract/sandbox-resolve-path.rs"]
pub mod contract_sandbox_resolve_path;
#[path = "skills/contract/contract-get.rs"]
pub mod contract_contract_get;
#[path = "skills/contract/contract-templates.rs"]
pub mod contract_contract_templates;
#[path = "skills/contract/contract-list.rs"]
pub mod contract_contract_list;
#[path = "skills/contract/contract-sanity-check.rs"]
pub mod contract_contract_sanity_check;
#[path = "skills/contract/contract-supersede.rs"]
pub mod contract_contract_supersede;
#[path = "skills/contract/pipeline-stage-record-failure.rs"]
pub mod contract_pipeline_stage_record_failure;
#[path = "skills/docs/excalidraw-embed.rs"]
pub mod docs_excalidraw_embed;
#[path = "skills/docs/learning-center-section.rs"]
pub mod docs_learning_center_section;
#[path = "skills/docs/learning-center-render.rs"]
pub mod docs_learning_center_render;
#[path = "skills/driver/driver-halt-check.rs"]
pub mod driver_driver_halt_check;
#[path = "skills/driver/_helpers/failure-advice.rs"]
pub mod driver__helpers_failure_advice;
#[path = "skills/driver/action-input-builder.rs"]
pub mod driver_action_input_builder;
#[path = "skills/driver/driver-build-tests.rs"]
pub mod driver_driver_build_tests;
#[path = "skills/driver/driver-failures-list.rs"]
pub mod driver_driver_failures_list;
#[path = "skills/driver/driver-parallel-plan.rs"]
pub mod driver_driver_parallel_plan;
#[path = "skills/driver/failure-feed.rs"]
pub mod driver_failure_feed;
#[path = "skills/driver/model-scoreboard.rs"]
pub mod driver_model_scoreboard;
#[path = "skills/driver/pipeline-name-annotations.rs"]
pub mod driver_pipeline_name_annotations;
#[path = "skills/driver/state-machine-rules.rs"]
pub mod driver_state_machine_rules;
#[path = "skills/driver/driver-state-machine.rs"]
pub mod driver_driver_state_machine;
#[path = "skills/engine/action-run.rs"]
pub mod engine_action_run;
#[path = "skills/engine/canonical-actions/action.rs"]
pub mod engine_canonical_actions_action;
#[path = "skills/engine/canonical-actions/agent.rs"]
pub mod engine_canonical_actions_agent;
#[path = "skills/engine/canonical-actions/ai.rs"]
pub mod engine_canonical_actions_ai;
#[path = "skills/engine/canonical-actions/blueprint.rs"]
pub mod engine_canonical_actions_blueprint;
#[path = "skills/engine/canonical-actions/board-keeper.rs"]
pub mod engine_canonical_actions_board_keeper;
#[path = "skills/engine/canonical-actions/brain.rs"]
pub mod engine_canonical_actions_brain;
#[path = "skills/engine/canonical-actions/bugfinder.rs"]
pub mod engine_canonical_actions_bugfinder;
#[path = "skills/engine/canonical-actions/card.rs"]
pub mod engine_canonical_actions_card;
#[path = "skills/engine/canonical-actions/cockpit.rs"]
pub mod engine_canonical_actions_cockpit;
#[path = "skills/engine/canonical-actions/codebase-1.rs"]
pub mod engine_canonical_actions_codebase_1;
#[path = "skills/engine/canonical-actions/codebase-2.rs"]
pub mod engine_canonical_actions_codebase_2;
#[path = "skills/engine/canonical-actions/constitution.rs"]
pub mod engine_canonical_actions_constitution;
#[path = "skills/engine/canonical-actions/contract.rs"]
pub mod engine_canonical_actions_contract;
#[path = "skills/engine/canonical-actions/cost.rs"]
pub mod engine_canonical_actions_cost;
#[path = "skills/engine/canonical-actions/docs.rs"]
pub mod engine_canonical_actions_docs;
#[path = "skills/engine/canonical-actions/driver.rs"]
pub mod engine_canonical_actions_driver;
#[path = "skills/engine/canonical-actions/git-manager.rs"]
pub mod engine_canonical_actions_git_manager;
#[path = "skills/engine/canonical-actions/god.rs"]
pub mod engine_canonical_actions_god;
#[path = "skills/engine/canonical-actions/heimdall.rs"]
pub mod engine_canonical_actions_heimdall;
#[path = "skills/engine/canonical-actions/keeper.rs"]
pub mod engine_canonical_actions_keeper;
#[path = "skills/engine/canonical-actions/law.rs"]
pub mod engine_canonical_actions_law;
#[path = "skills/engine/canonical-actions/man.rs"]
pub mod engine_canonical_actions_man;
#[path = "skills/engine/canonical-actions/memory.rs"]
pub mod engine_canonical_actions_memory;
#[path = "skills/engine/canonical-actions/migrated.rs"]
pub mod engine_canonical_actions_migrated;
#[path = "skills/engine/canonical-actions/misc-1.rs"]
pub mod engine_canonical_actions_misc_1;
#[path = "skills/engine/canonical-actions/misc-2.rs"]
pub mod engine_canonical_actions_misc_2;
#[path = "skills/engine/canonical-actions/notebook.rs"]
pub mod engine_canonical_actions_notebook;
#[path = "skills/engine/canonical-actions/pl.rs"]
pub mod engine_canonical_actions_pl;
#[path = "skills/engine/canonical-actions/qa.rs"]
pub mod engine_canonical_actions_qa;
#[path = "skills/engine/canonical-actions/tl.rs"]
pub mod engine_canonical_actions_tl;
#[path = "skills/engine/canonical-actions/ultra.rs"]
pub mod engine_canonical_actions_ultra;
#[path = "skills/engine/canonical-agent-stages.rs"]
pub mod engine_canonical_agent_stages;
#[path = "skills/engine/canonical-agents.rs"]
pub mod engine_canonical_agents;
#[path = "skills/engine/canonical-env-flags.rs"]
pub mod engine_canonical_env_flags;
#[path = "skills/engine/canonical-heimdall.rs"]
pub mod engine_canonical_heimdall;
#[path = "skills/engine/canonical-laws.rs"]
pub mod engine_canonical_laws;
#[path = "skills/engine/error.rs"]
pub mod engine_error;
#[path = "skills/engine/json-utils.rs"]
pub mod engine_json_utils;
#[path = "skills/engine/log-decision.rs"]
pub mod engine_log_decision;
#[path = "skills/engine/log-event.rs"]
pub mod engine_log_event;
#[path = "skills/engine/open-db/schemas.rs"]
pub mod engine_open_db_schemas;
#[path = "skills/engine/sync-restore-from-revision.rs"]
pub mod engine_sync_restore_from_revision;
#[path = "skills/engine/with-audit.rs"]
pub mod engine_with_audit;
#[path = "skills/entities/action-add.rs"]
pub mod entities_action_add;
#[path = "skills/entities/action-list.rs"]
pub mod entities_action_list;
#[path = "skills/entities/agent-add.rs"]
pub mod entities_agent_add;
#[path = "skills/entities/agent-custom-prompt-set.rs"]
pub mod entities_agent_custom_prompt_set;
#[path = "skills/entities/agent-grant.rs"]
pub mod entities_agent_grant;
#[path = "skills/entities/agent-grants-check.rs"]
pub mod entities_agent_grants_check;
#[path = "skills/entities/agent-permission-check.rs"]
pub mod entities_agent_permission_check;
#[path = "skills/entities/agent-permissions.rs"]
pub mod entities_agent_permissions;
#[path = "skills/entities/agent-persona-resolve.rs"]
pub mod entities_agent_persona_resolve;
#[path = "skills/entities/agent-revoke.rs"]
pub mod entities_agent_revoke;
#[path = "skills/entities/agent-subagent-update.rs"]
pub mod entities_agent_subagent_update;
#[path = "skills/entities/anatomy-latest.rs"]
pub mod entities_anatomy_latest;
#[path = "skills/entities/brain-decision-record.rs"]
pub mod entities_brain_decision_record;
#[path = "skills/entities/brain-fire-record.rs"]
pub mod entities_brain_fire_record;
#[path = "skills/entities/brain-fire-start.rs"]
pub mod entities_brain_fire_start;
#[path = "skills/entities/brain-lock-acquire.rs"]
pub mod entities_brain_lock_acquire;
#[path = "skills/entities/brain-lock-active.rs"]
pub mod entities_brain_lock_active;
#[path = "skills/entities/brain-lock-release.rs"]
pub mod entities_brain_lock_release;
#[path = "skills/entities/brain-session-end.rs"]
pub mod entities_brain_session_end;
#[path = "skills/entities/brain-session-summary.rs"]
pub mod entities_brain_session_summary;
#[path = "skills/entities/card-accuracy-persist.rs"]
pub mod entities_card_accuracy_persist;
#[path = "skills/entities/card-add.rs"]
pub mod entities_card_add;
#[path = "skills/entities/card-automerge-set.rs"]
pub mod entities_card_automerge_set;
#[path = "skills/entities/card-children.rs"]
pub mod entities_card_children;
#[path = "skills/entities/card-clone-and-archive.rs"]
pub mod entities_card_clone_and_archive;
#[path = "skills/entities/card-clone-insert.rs"]
pub mod entities_card_clone_insert;
#[path = "skills/entities/card-deps-check.rs"]
pub mod entities_card_deps_check;
#[path = "skills/entities/card-run-preflight.rs"]
pub mod entities_card_run_preflight;
#[path = "skills/entities/card-fields-check.rs"]
pub mod entities_card_fields_check;
#[path = "skills/entities/card-list.rs"]
pub mod entities_card_list;
#[path = "skills/entities/card-phase-event-record.rs"]
pub mod entities_card_phase_event_record;
#[path = "skills/entities/card-phase-events-list.rs"]
pub mod entities_card_phase_events_list;
#[path = "skills/entities/card-reorder.rs"]
pub mod entities_card_reorder;
#[path = "skills/entities/card-score-persist.rs"]
pub mod entities_card_score_persist;
#[path = "skills/entities/card-status-change.rs"]
pub mod entities_card_status_change;
#[path = "skills/entities/card-stuck-where.rs"]
pub mod entities_card_stuck_where;
#[path = "skills/entities/card-tag-add.rs"]
pub mod entities_card_tag_add;
#[path = "skills/entities/card-tags-list.rs"]
pub mod entities_card_tags_list;
#[path = "skills/entities/card-update.rs"]
pub mod entities_card_update;
#[path = "skills/entities/catalog-find.rs"]
pub mod entities_catalog_find;
#[path = "skills/entities/compute-complexity-risk.rs"]
pub mod entities_compute_complexity_risk;
#[path = "skills/entities/file-sensitivity.rs"]
pub mod entities_file_sensitivity;
#[path = "skills/entities/inbox-message-add.rs"]
pub mod entities_inbox_message_add;
#[path = "skills/entities/inbox-message-list.rs"]
pub mod entities_inbox_message_list;
#[path = "skills/entities/inbox-message-update.rs"]
pub mod entities_inbox_message_update;
#[path = "skills/entities/knowledge-document-add.rs"]
pub mod entities_knowledge_document_add;
#[path = "skills/entities/knowledge-document-ingest.rs"]
pub mod entities_knowledge_document_ingest;
#[path = "skills/entities/knowledge-extract-add.rs"]
pub mod entities_knowledge_extract_add;
#[path = "skills/entities/knowledge-find.rs"]
pub mod entities_knowledge_find;
#[path = "skills/entities/knowledge-ir.rs"]
pub mod entities_knowledge_ir;
#[path = "skills/entities/knowledge-pack-resolve.rs"]
pub mod entities_knowledge_pack_resolve;
#[path = "skills/entities/law-add.rs"]
pub mod entities_law_add;
#[path = "skills/entities/law-check.rs"]
pub mod entities_law_check;
#[path = "skills/entities/law-list.rs"]
pub mod entities_law_list;
#[path = "skills/entities/law-set-active.rs"]
pub mod entities_law_set_active;
#[path = "skills/entities/law-supersede.rs"]
pub mod entities_law_supersede;
#[path = "skills/entities/law-violation-record.rs"]
pub mod entities_law_violation_record;
#[path = "skills/entities/law-violations-list.rs"]
pub mod entities_law_violations_list;
#[path = "skills/entities/memory-add.rs"]
pub mod entities_memory_add;
#[path = "skills/entities/memory-consolidate.rs"]
pub mod entities_memory_consolidate;
#[path = "skills/entities/memory-embed-store.rs"]
pub mod entities_memory_embed_store;
#[path = "skills/entities/memory-graph.rs"]
pub mod entities_memory_graph;
#[path = "skills/entities/memory-link-add.rs"]
pub mod entities_memory_link_add;
#[path = "skills/entities/memory-list.rs"]
pub mod entities_memory_list;
#[path = "skills/entities/memory-rag-context.rs"]
pub mod entities_memory_rag_context;
#[path = "skills/entities/memory-similar.rs"]
pub mod entities_memory_similar;
#[path = "skills/entities/memory-supersede.rs"]
pub mod entities_memory_supersede;
#[path = "skills/entities/memory-use-list.rs"]
pub mod entities_memory_use_list;
#[path = "skills/entities/memory-use-rate.rs"]
pub mod entities_memory_use_rate;
#[path = "skills/entities/memory-use-record.rs"]
pub mod entities_memory_use_record;
#[path = "skills/entities/memory-validate.rs"]
pub mod entities_memory_validate;
#[path = "skills/entities/recommend-next-work.rs"]
pub mod entities_recommend_next_work;
#[path = "skills/entities/risk-tag.rs"]
pub mod entities_risk_tag;
#[path = "skills/entities/sandbox-phase-set.rs"]
pub mod entities_sandbox_phase_set;
#[path = "skills/entities/sprint-add-item.rs"]
pub mod entities_sprint_add_item;
#[path = "skills/entities/sprint-create.rs"]
pub mod entities_sprint_create;
#[path = "skills/entities/sprint-render.rs"]
pub mod entities_sprint_render;
#[path = "skills/entities/sprint-set-item-phase.rs"]
pub mod entities_sprint_set_item_phase;
#[path = "skills/entities/triage-recommendation-list.rs"]
pub mod entities_triage_recommendation_list;
#[path = "skills/entities/triage-recommendation-mark.rs"]
pub mod entities_triage_recommendation_mark;
#[path = "skills/external/_helpers/ollama-host.rs"]
pub mod external__helpers_ollama_host;
#[path = "skills/external/_helpers/provider-adapter-helpers.rs"]
pub mod external__helpers_provider_adapter_helpers;
#[path = "skills/external/_helpers/provider-adapter.contract.rs"]
pub mod external__helpers_provider_adapter_contract;
#[path = "skills/external/diff-decide.rs"]
pub mod external_diff_decide;
#[path = "skills/external/diff-propose.rs"]
pub mod external_diff_propose;
#[path = "skills/external/mcp-render-board.rs"]
pub mod external_mcp_render_board;
#[path = "skills/external/mcp-render-card.rs"]
pub mod external_mcp_render_card;
#[path = "skills/external/mcp-render-memory.rs"]
pub mod external_mcp_render_memory;
#[path = "skills/external/mcp-render-reasoning.rs"]
pub mod external_mcp_render_reasoning;
#[path = "skills/external/mcp-render-sessions.rs"]
pub mod external_mcp_render_sessions;
#[path = "skills/external/memory-export-training.rs"]
pub mod external_memory_export_training;
#[path = "skills/external/to-mcp-json.rs"]
pub mod external_to_mcp_json;
#[path = "skills/external/to-mcp-markdown.rs"]
pub mod external_to_mcp_markdown;
#[path = "skills/heimdall/_helpers/casting.rs"]
pub mod heimdall__helpers_casting;
#[path = "skills/heimdall/heimdall-ai-calibration.rs"]
pub mod heimdall_heimdall_ai_calibration;
#[path = "skills/heimdall/heimdall-attribute.rs"]
pub mod heimdall_heimdall_attribute;
#[path = "skills/heimdall/heimdall-ceiling-set.rs"]
pub mod heimdall_heimdall_ceiling_set;
#[path = "skills/heimdall/heimdall-classify.rs"]
pub mod heimdall_heimdall_classify;
#[path = "skills/heimdall/heimdall-flipflops.rs"]
pub mod heimdall_heimdall_flipflops;
#[path = "skills/heimdall/heimdall-ledger-rebuild.rs"]
pub mod heimdall_heimdall_ledger_rebuild;
#[path = "skills/heimdall/heimdall-mode.rs"]
pub mod heimdall_heimdall_mode;
#[path = "skills/heimdall/heimdall-overview.rs"]
pub mod heimdall_heimdall_overview;
#[path = "skills/heimdall/heimdall-tag-add.rs"]
pub mod heimdall_heimdall_tag_add;
#[path = "skills/heimdall/heimdall-tag-queue.rs"]
pub mod heimdall_heimdall_tag_queue;
#[path = "skills/heimdall/heimdall-trust-set.rs"]
pub mod heimdall_heimdall_trust_set;
#[path = "skills/heimdall/heimdall-validate.rs"]
pub mod heimdall_heimdall_validate;
#[path = "skills/notebooks/notebook-promote.rs"]
pub mod notebooks_notebook_promote;
#[path = "skills/notebooks/notebook-schema.rs"]
pub mod notebooks_notebook_schema;
#[path = "skills/notebooks/notebook-scope.rs"]
pub mod notebooks_notebook_scope;
#[path = "skills/notebooks/notebook-source.rs"]
pub mod notebooks_notebook_source;
#[path = "skills/notebooks/notebook-store.rs"]
pub mod notebooks_notebook_store;
#[path = "skills/onboarding/onboarding-infer.rs"]
pub mod onboarding_onboarding_infer;
#[path = "skills/onboarding/onboarding-orchestrate.rs"]
pub mod onboarding_onboarding_orchestrate;
#[path = "skills/onboarding/onboarding-write.rs"]
pub mod onboarding_onboarding_write;
#[path = "skills/rag/cosine-rank.rs"]
pub mod rag_cosine_rank;
#[path = "skills/rag/decay-score.rs"]
pub mod rag_decay_score;
#[path = "skills/rag/rag-graph-build.rs"]
pub mod rag_rag_graph_build;
#[path = "skills/rag/rag-graph.rs"]
pub mod rag_rag_graph;
#[path = "skills/reconcilers/orphan-worktree-dirs.rs"]
pub mod reconcilers_orphan_worktree_dirs;
#[path = "skills/reconcilers/orphan-sandbox-dirs.rs"]
pub mod reconcilers_orphan_sandbox_dirs;
#[path = "skills/reconcilers/brain-decisions-bloat.rs"]
pub mod reconcilers_brain_decisions_bloat;
#[path = "skills/reconcilers/brain-envelope-bloat.rs"]
pub mod reconcilers_brain_envelope_bloat;
#[path = "skills/reconcilers/card-blocked-but-intent-play.rs"]
pub mod reconcilers_card_blocked_but_intent_play;
#[path = "skills/reconcilers/card-causes-repeat-failures.rs"]
pub mod reconcilers_card_causes_repeat_failures;
#[path = "skills/reconcilers/card-intent-invariants.rs"]
pub mod reconcilers_card_intent_invariants;
#[path = "skills/reconcilers/card-status-mirrors-work.rs"]
pub mod reconcilers_card_status_mirrors_work;
#[path = "skills/reconcilers/contracts-rejected-no-redraft.rs"]
pub mod reconcilers_contracts_rejected_no_redraft;
#[path = "skills/reconcilers/dev-microtasks-leaked-amendment.rs"]
pub mod reconcilers_dev_microtasks_leaked_amendment;
#[path = "skills/reconcilers/driver-ticks-bloat.rs"]
pub mod reconcilers_driver_ticks_bloat;
#[path = "skills/reconcilers/escalated-contract-no-more-work.rs"]
pub mod reconcilers_escalated_contract_no_more_work;
#[path = "skills/reconcilers/inflight-ai-calls.rs"]
pub mod reconcilers_inflight_ai_calls;
#[path = "skills/reconcilers/pending-warn-notifications.rs"]
pub mod reconcilers_pending_warn_notifications;
#[path = "skills/reconcilers/playing-card-no-driver-progress.rs"]
pub mod reconcilers_playing_card_no_driver_progress;
#[path = "skills/reconcilers/recent-blocked-actions.rs"]
pub mod reconcilers_recent_blocked_actions;
#[path = "skills/reconcilers/red-notifs-blocking-dispatch.rs"]
pub mod reconcilers_red_notifs_blocking_dispatch;
#[path = "skills/reconcilers/runtime-events-bloat.rs"]
pub mod reconcilers_runtime_events_bloat;
#[path = "skills/reconcilers/stuck-contracts.rs"]
pub mod reconcilers_stuck_contracts;
#[path = "skills/reconcilers/stuck-red-notifs.rs"]
pub mod reconcilers_stuck_red_notifs;
#[path = "skills/reconcilers/unread-error-inbox.rs"]
pub mod reconcilers_unread_error_inbox;
#[path = "skills/render/action-result-renderers.rs"]
pub mod render_action_result_renderers;
#[path = "skills/render/agent-stages-html.rs"]
pub mod render_agent_stages_html;
#[path = "skills/render/agents-md.rs"]
pub mod render_agents_md;
#[path = "skills/render/anatomy-html.rs"]
pub mod render_anatomy_html;
#[path = "skills/render/autonomy-score-html.rs"]
pub mod render_autonomy_score_html;
#[path = "skills/render/board-summary.rs"]
pub mod render_board_summary;
#[path = "skills/render/ast-html.rs"]
pub mod render_ast_html;
#[path = "skills/render/card-html.rs"]
pub mod render_card_html;
#[path = "skills/render/card-log.rs"]
pub mod render_card_log;
#[path = "skills/render/card-render-html.rs"]
pub mod render_card_render_html;
#[path = "skills/render/card-render-index-html.rs"]
pub mod render_card_render_index_html;
#[path = "skills/render/concurrency-doc-html.rs"]
pub mod render_concurrency_doc_html;
#[path = "skills/render/contract-render-html.rs"]
pub mod render_contract_render_html;
#[path = "skills/render/contract-render-index-html.rs"]
pub mod render_contract_render_index_html;
#[path = "skills/render/docs-index-html.rs"]
pub mod render_docs_index_html;
#[path = "skills/render/getting-started-html.rs"]
pub mod render_getting_started_html;
#[path = "skills/render/laws-md.rs"]
pub mod render_laws_md;
#[path = "skills/render/log-tail-ansi.rs"]
pub mod render_log_tail_ansi;
#[path = "skills/render/log-tail-html.rs"]
pub mod render_log_tail_html;
#[path = "skills/render/man-compliance-html.rs"]
pub mod render_man_compliance_html;
#[path = "skills/render/memory-graph-html.rs"]
pub mod render_memory_graph_html;
#[path = "skills/render/memory-html.rs"]
pub mod render_memory_html;
#[path = "skills/render/memory-log.rs"]
pub mod render_memory_log;
#[path = "skills/render/pl-questionnaire-md.rs"]
pub mod render_pl_questionnaire_md;
#[path = "skills/render/sandbox-doc-html.rs"]
pub mod render_sandbox_doc_html;
#[path = "skills/render/substrate-defense-html.rs"]
pub mod render_substrate_defense_html;
#[path = "skills/render/timeline-log.rs"]
pub mod render_timeline_log;
#[path = "skills/score/autonomy-score.rs"]
pub mod score_autonomy_score;
#[path = "skills/sources/extract-text.rs"]
pub mod sources_extract_text;
#[path = "skills/sources/source-add.rs"]
pub mod sources_source_add;
#[path = "skills/sources/source-get.rs"]
pub mod sources_source_get;
#[path = "skills/sources/source-list.rs"]
pub mod sources_source_list;
#[path = "skills/sources/source-read.rs"]
pub mod sources_source_read;
#[path = "skills/sources/source-remove.rs"]
pub mod sources_source_remove;
#[path = "skills/sources/sources-schema.rs"]
pub mod sources_sources_schema;
#[path = "skills/swarm/argument-links.rs"]
pub mod swarm_argument_links;
#[path = "skills/swarm/composition-check.rs"]
pub mod swarm_composition_check;
#[path = "skills/swarm/goal-derive.rs"]
pub mod swarm_goal_derive;
#[path = "skills/swarm/judgment-id.rs"]
pub mod swarm_judgment_id;
#[path = "skills/swarm/judgment-score.rs"]
pub mod swarm_judgment_score;
#[path = "skills/swarm/manifest-lint.rs"]
pub mod swarm_manifest_lint;
#[path = "skills/swarm/session-case.rs"]
pub mod swarm_session_case;
#[path = "skills/swarm/subagent-route.rs"]
pub mod swarm_subagent_route;
#[path = "skills/swarm/swarm-block-presence.rs"]
pub mod swarm_swarm_block_presence;
#[path = "skills/system/_helpers/rt-emotion.rs"]
pub mod system__helpers_rt_emotion;
#[path = "skills/system/_helpers/rt-play.rs"]
pub mod system__helpers_rt_play;
#[path = "skills/system/action-verify-coverage.rs"]
pub mod system_action_verify_coverage;
#[path = "skills/system/action-verify.rs"]
pub mod system_action_verify;
#[path = "skills/system/agent-detail.rs"]
pub mod system_agent_detail;
#[path = "skills/system/ai-call-detail.rs"]
pub mod system_ai_call_detail;
#[path = "skills/system/audit-timeline.rs"]
pub mod system_audit_timeline;
#[path = "skills/system/benchmark-status.rs"]
pub mod system_benchmark_status;
#[path = "skills/system/board-evaluate.rs"]
pub mod system_board_evaluate;
#[path = "skills/system/brain-consolidate.rs"]
pub mod system_brain_consolidate;
#[path = "skills/system/byok-local-model-add.rs"]
pub mod system_byok_local_model_add;
#[path = "skills/system/byok-model-set.rs"]
pub mod system_byok_model_set;
#[path = "skills/system/byok-strategy-set.rs"]
pub mod system_byok_strategy_set;
#[path = "skills/system/bypasses-list.rs"]
pub mod system_bypasses_list;
#[path = "skills/system/card-body-recovery.rs"]
pub mod system_card_body_recovery;
#[path = "skills/system/card-phase.rs"]
pub mod system_card_phase;
#[path = "skills/system/card-review-blueprint-get.rs"]
pub mod system_card_review_blueprint_get;
#[path = "skills/system/card-process-events.rs"]
pub mod system_card_process_events;
#[path = "skills/system/card-review-blueprints-list.rs"]
pub mod system_card_review_blueprints_list;
#[path = "skills/system/cards-active.rs"]
pub mod system_cards_active;
#[path = "skills/system/code-rebuild-settings.rs"]
pub mod system_code_rebuild_settings;
#[path = "skills/system/cost-aggregate.rs"]
pub mod system_cost_aggregate;
#[path = "skills/system/byok-key-set.rs"]
pub mod system_byok_key_set;
#[path = "skills/system/db-reset.rs"]
pub mod system_db_reset;
#[path = "skills/system/driver-halt.rs"]
pub mod system_driver_halt;
#[path = "skills/system/naming-derive.rs"]
pub mod system_naming_derive;
#[path = "skills/system/cost-settings.rs"]
pub mod system_cost_settings;
#[path = "skills/system/drift-list.rs"]
pub mod system_drift_list;
#[path = "skills/system/driver-ticks.rs"]
pub mod system_driver_ticks;
#[path = "skills/system/focus-detectors/blocked-status.rs"]
pub mod system_focus_detectors_blocked_status;
#[path = "skills/system/focus-detectors/epic-has-open-children.rs"]
pub mod system_focus_detectors_epic_has_open_children;
#[path = "skills/system/focus-detectors/no-contract-for-doing-card.rs"]
pub mod system_focus_detectors_no_contract_for_doing_card;
#[path = "skills/system/focus-detectors/pl-unanswered.rs"]
pub mod system_focus_detectors_pl_unanswered;
#[path = "skills/system/focus-detectors/red-notification.rs"]
pub mod system_focus_detectors_red_notification;
#[path = "skills/system/focus-detectors/stopped-intent.rs"]
pub mod system_focus_detectors_stopped_intent;
#[path = "skills/system/focus-detectors/stuck-contract.rs"]
pub mod system_focus_detectors_stuck_contract;
#[path = "skills/system/laws-list.rs"]
pub mod system_laws_list;
#[path = "skills/system/log-error.rs"]
pub mod system_log_error;
#[path = "skills/system/board-reads.rs"]
pub mod system_board_reads;
#[path = "skills/system/contract-reads.rs"]
pub mod system_contract_reads;
#[path = "skills/system/be-surface-writes.rs"]
pub mod system_be_surface_writes;
#[path = "skills/system/be-surface-reads.rs"]
pub mod system_be_surface_reads;
#[path = "skills/system/knowledge-reads.rs"]
pub mod system_knowledge_reads;
#[path = "skills/system/memory-reads.rs"]
pub mod system_memory_reads;
#[path = "skills/system/preview-target-resolve.rs"]
pub mod system_preview_target_resolve;
#[path = "skills/context/context-library.rs"]
pub mod context_context_library;
#[path = "skills/system/notifications-add.rs"]
pub mod system_notifications_add;
#[path = "skills/system/notifications-dismiss.rs"]
pub mod system_notifications_dismiss;
#[path = "skills/system/notifications-list.rs"]
pub mod system_notifications_list;
#[path = "skills/system/personas-dashboard.rs"]
pub mod system_personas_dashboard;
#[path = "skills/system/reconcile-resume-analyze.rs"]
pub mod system_reconcile_resume_analyze;
#[path = "skills/system/research-find.rs"]
pub mod system_research_find;
#[path = "skills/system/review-anchor-resolve.rs"]
pub mod system_review_anchor_resolve;
#[path = "skills/system/runtime-events.rs"]
pub mod system_runtime_events;
#[path = "skills/system/stats-overview.rs"]
pub mod system_stats_overview;
#[path = "skills/system/substrate-stream.rs"]
pub mod system_substrate_stream;
#[path = "skills/system/tokens-count.rs"]
pub mod system_tokens_count;
#[path = "skills/ultra/_helpers/schema.rs"]
pub mod ultra__helpers_schema;
#[path = "skills/ultra/ultra-report.rs"]
pub mod ultra_ultra_report;
#[path = "skills/workflow/workflow-add.rs"]
pub mod workflow_workflow_add;
#[path = "skills/workflow/workflow-find.rs"]
pub mod workflow_workflow_find;
#[path = "skills/workflow/workflow-list.rs"]
pub mod workflow_workflow_list;
#[path = "skills/workflow/workflow-run.rs"]
pub mod workflow_workflow_run;
#[path = "skills/system/workspace-list.rs"]
pub mod system_workspace_list;
#[path = "skills/system/byok-local-endpoint-set.rs"]
pub mod system_byok_local_endpoint_set;
#[path = "skills/entities/artefact-store.rs"]
pub mod entities_artefact_store;
#[path = "skills/agents/in/pane-feed.rs"]
pub mod agents_in_pane_feed;
#[path = "skills/system/workspace-activate.rs"]
pub mod system_workspace_activate;
#[path = "skills/blueprint/blueprint-apply.rs"]
pub mod blueprint_blueprint_apply;
#[path = "skills/codebase/design-css-compile.rs"]
pub mod codebase_design_css_compile;
#[path = "skills/reconcilers/stale-pid-files.rs"]
pub mod reconcilers_stale_pid_files;
#[path = "skills/external/auth-init.rs"]
pub mod external_auth_init;
#[path = "skills/agents/in/skill-library-materialize.rs"]
pub mod agents_in_skill_library_materialize;
#[path = "skills/codebase/atlas-register.rs"]
pub mod codebase_atlas_register;
#[path = "skills/driver/get-pipelines-and-phases.rs"]
pub mod driver_get_pipelines_and_phases;
#[path = "skills/render/laws-render-all.rs"]
pub mod render_laws_render_all;
#[path = "skills/blueprint/man-seal.rs"]
pub mod blueprint_man_seal;
#[path = "skills/contract/qa-sandbox-reset.rs"]
pub mod contract_qa_sandbox_reset;
#[path = "skills/audit/brain-compact.rs"]
pub mod audit_brain_compact;
#[path = "skills/constitution/_helpers/cognitive.rs"]
pub mod constitution__helpers_cognitive;
#[path = "skills/audit/driver-token.rs"]
pub mod audit_driver_token;
#[path = "skills/agents/tl/tl-pre-contract-data-query.rs"]
pub mod agents_tl_tl_pre_contract_data_query;
#[path = "skills/agents/god/god-override-verify.rs"]
pub mod agents_god_god_override_verify;
#[path = "skills/external/file-read.rs"]
pub mod external_file_read;
#[path = "skills/agents/in/interviewer-episodic-memory.rs"]
pub mod agents_in_interviewer_episodic_memory;
#[path = "skills/agents/man/man-audit-drift-spec-vs-impl.rs"]
pub mod agents_man_man_audit_drift_spec_vs_impl;
#[path = "skills/system/workspace-scripts.rs"]
pub mod system_workspace_scripts;
#[path = "skills/system/driver-status.rs"]
pub mod system_driver_status;
#[path = "skills/system/driver-snippet-gate.rs"]
pub mod system_driver_snippet_gate;
#[path = "skills/audit/verify-chain-full.rs"]
pub mod audit_verify_chain_full;
#[path = "skills/external/file-write-coverage-audit.rs"]
pub mod external_file_write_coverage_audit;
#[path = "skills/constitution/constitution-mine.rs"]
pub mod constitution_constitution_mine;
#[path = "skills/external/file-list.rs"]
pub mod external_file_list;
#[path = "skills/audit/timeline-digest-md.rs"]
pub mod audit_timeline_digest_md;
#[path = "skills/agents/tl/tl-sandbox-create.rs"]
pub mod agents_tl_tl_sandbox_create;
#[path = "skills/audit/law-hook-card-body-schema.rs"]
pub mod audit_law_hook_card_body_schema;
#[path = "skills/agents/in/interviewer-action-catalog.rs"]
pub mod agents_in_interviewer_action_catalog;
#[path = "skills/engine/canonical-actions.rs"]
pub mod engine_canonical_actions;
#[path = "skills/system/work-start.rs"]
pub mod system_work_start;
#[path = "skills/render/driver-doc-html.rs"]
pub mod render_driver_doc_html;
#[path = "skills/audit/_helpers/timeline-segments.rs"]
pub mod audit__helpers_timeline_segments;
#[path = "skills/system/card-lint.rs"]
pub mod system_card_lint;
#[path = "skills/audit/brain-decisions-archive.rs"]
pub mod audit_brain_decisions_archive;
#[path = "skills/agents/in/narration-compile.rs"]
pub mod agents_in_narration_compile;
#[path = "skills/swarm/court-evidence-gather.rs"]
pub mod swarm_court_evidence_gather;
#[path = "skills/system/card-body-generate.rs"]
pub mod system_card_body_generate;
#[path = "skills/agents/qa/qa-review-scanners.rs"]
pub mod agents_qa_qa_review_scanners;
#[path = "skills/codebase/code-signals-detect.rs"]
pub mod codebase_code_signals_detect;
#[path = "skills/constitution/constitution-law.rs"]
pub mod constitution_constitution_law;
#[path = "skills/system/card-runtime-state.rs"]
pub mod system_card_runtime_state;
#[path = "skills/external/file-write.rs"]
pub mod external_file_write;
#[path = "skills/system/drift-resolve.rs"]
pub mod system_drift_resolve;
#[path = "skills/agents/dev/dev-bundle-slice.rs"]
pub mod agents_dev_dev_bundle_slice;
#[path = "skills/agents/in/book-parse.rs"]
pub mod agents_in_book_parse;
#[path = "skills/system/card-normalize.rs"]
pub mod system_card_normalize;
#[path = "skills/system/_helpers/reasoning-templates.rs"]
pub mod system__helpers_reasoning_templates;
#[path = "skills/agents/in/reasoning-compile.rs"]
pub mod agents_in_reasoning_compile;
#[path = "skills/constitution/constitution-packs.rs"]
pub mod constitution_constitution_packs;
#[path = "skills/render/excalidraw-emit.rs"]
pub mod render_excalidraw_emit;
#[path = "skills/render/excalidraw-board.rs"]
pub mod render_excalidraw_board;
#[path = "skills/render/excalidraw-lifecycle.rs"]
pub mod render_excalidraw_lifecycle;
#[path = "skills/render/excalidraw-pipeline.rs"]
pub mod render_excalidraw_pipeline;
#[path = "skills/agents/in/book-ingest.rs"]
pub mod agents_in_book_ingest;
#[path = "skills/agents/in/interviewer-bundle-slice.rs"]
pub mod agents_in_interviewer_bundle_slice;
#[path = "skills/driver/driver-inspect.rs"]
pub mod driver_driver_inspect;
#[path = "skills/codebase/_helpers/run-cmd.rs"]
pub mod codebase__helpers_run_cmd;
#[path = "skills/codebase/code-git-init.rs"]
pub mod codebase_code_git_init;
#[path = "skills/bench/bench-record.rs"]
pub mod bench_bench_record;
#[path = "skills/agents/in/pane-checkpoint.rs"]
pub mod agents_in_pane_checkpoint;
#[path = "skills/cockpit/cockpit-pane-diff.rs"]
pub mod cockpit_cockpit_pane_diff;
#[path = "skills/constitution/constitution-provenance.rs"]
pub mod constitution_constitution_provenance;
#[path = "skills/cockpit/cockpit-pr-list.rs"]
pub mod cockpit_cockpit_pr_list;
#[path = "skills/agents/git-manager/git-manager-branch-init.rs"]
pub mod agents_git_manager_git_manager_branch_init;
#[path = "skills/agents/git-manager/driver-pr-open.rs"]
pub mod agents_git_manager_driver_pr_open;
#[path = "skills/engine/sync-capture-revision.rs"]
pub mod engine_sync_capture_revision;
#[path = "skills/agents/in/pane-scope-sentry.rs"]
pub mod agents_in_pane_scope_sentry;
#[path = "skills/agents/git-manager/git-manager-stage-validate.rs"]
pub mod agents_git_manager_git_manager_stage_validate;
#[path = "skills/cockpit/cockpit-integrate.rs"]
pub mod cockpit_cockpit_integrate;
#[path = "skills/system/card-commits.rs"]
pub mod system_card_commits;
#[path = "skills/codebase/code-file-review-history.rs"]
pub mod codebase_code_file_review_history;
#[path = "skills/cockpit/cockpit-pr-open.rs"]
pub mod cockpit_cockpit_pr_open;
#[path = "skills/cockpit/cockpit-pr-sync.rs"]
pub mod cockpit_cockpit_pr_sync;
#[path = "skills/agents/qa/qa-scope-check.rs"]
pub mod agents_qa_qa_scope_check;
#[path = "skills/codebase/code-file-history.rs"]
pub mod codebase_code_file_history;
#[path = "skills/cockpit/cockpit-git-graph.rs"]
pub mod cockpit_cockpit_git_graph;
#[path = "skills/codebase/code-coverage.rs"]
pub mod codebase_code_coverage;
#[path = "skills/ultra/ultra-provenance.rs"]
pub mod ultra_ultra_provenance;
#[path = "skills/cockpit/cockpit-script-run.rs"]
pub mod cockpit_cockpit_script_run;
#[path = "skills/codebase/code-git-info.rs"]
pub mod codebase_code_git_info;
#[path = "skills/system/code-tree.rs"]
pub mod system_code_tree;
#[path = "skills/agents/in/shared-diff.rs"]
pub mod agents_in_shared_diff;
#[path = "skills/external/events-flush.rs"]
pub mod external_events_flush;
#[path = "skills/system/code-imports.rs"]
pub mod system_code_imports;
#[path = "skills/codebase/code-authors.rs"]
pub mod codebase_code_authors;
#[path = "skills/reconcilers/uncommitted-dev-output.rs"]
pub mod reconcilers_uncommitted_dev_output;
#[path = "skills/agents/git-manager/git-manager-worktree-ops.rs"]
pub mod agents_git_manager_git_manager_worktree_ops;
#[path = "skills/codebase/code-signals-graph.rs"]
pub mod codebase_code_signals_graph;
#[path = "skills/system/card-grounding-collect.rs"]
pub mod system_card_grounding_collect;
#[path = "skills/external/diff-apply.rs"]
pub mod external_diff_apply;
#[path = "skills/agents/git-manager/git-manager-worktree-session.rs"]
pub mod agents_git_manager_git_manager_worktree_session;
#[path = "skills/audit/law-hook-no-secrets-in-commit.rs"]
pub mod audit_law_hook_no_secrets_in_commit;
#[path = "skills/notebooks/notebook-corpus.rs"]
pub mod notebooks_notebook_corpus;
#[path = "skills/onboarding/onboarding-scan.rs"]
pub mod onboarding_onboarding_scan;
#[path = "skills/ai/ai-prompt-path-grounding.rs"]
pub mod ai_ai_prompt_path_grounding;
#[path = "skills/system/reconcile-external-code-drift.rs"]
pub mod system_reconcile_external_code_drift;
#[path = "skills/agents/dev/dev-test-gate.rs"]
pub mod agents_dev_dev_test_gate;
#[path = "skills/agents/man/man-audit-truth-cross-check.rs"]
pub mod agents_man_man_audit_truth_cross_check;
#[path = "skills/constitution/_helpers/engineering.rs"]
pub mod constitution__helpers_engineering;
#[path = "skills/external/proc-run.rs"]
pub mod external_proc_run;
#[path = "skills/external/git-commit-card.rs"]
pub mod external_git_commit_card;
#[path = "skills/system/git-pending-list.rs"]
pub mod system_git_pending_list;
#[path = "skills/agents/man/man-audit-contract-phase1.rs"]
pub mod agents_man_man_audit_contract_phase1;
#[path = "skills/system/reconcile-truth-fusion.rs"]
pub mod system_reconcile_truth_fusion;
#[path = "skills/contract/qa-sandbox-promote.rs"]
pub mod contract_qa_sandbox_promote;
#[path = "skills/system/card-metrics.rs"]
pub mod system_card_metrics;
// ── END EXPANDER MANAGED ──

// Hand-registered (do NOT run expand.mjs — it regenerates the block above from frontier.json and
// would drop this): skills/constitution/_helpers/ts-query.mjs twin. grammarForPath is a fixtured
// no-db helper; getTree/queryMatches are TRANSITIVE (real native tree-sitter code + native tests,
// no JSON fixture — see the module doc for why).
#[path = "skills/constitution/_helpers/ts-query.rs"]
pub mod constitution__helpers_ts_query;

// Hand-registered (do NOT run expand.mjs — see comment above): skills/codebase/parse-treesitter.mjs
// twin, the AST-walking layer built on ts-query.rs's native tree-sitter primitives. parseTreeSitter
// is a fixtured no-db helper; parseToTree is TRANSITIVE (returns an owning Tree, not a JSON Value —
// no JSON fixture, see the module doc).
#[path = "skills/codebase/parse-treesitter.rs"]
pub mod codebase_parse_treesitter;
// Hand-registered: the walk profiles for the thirteen grammars parse-treesitter.rs does not
// hand-write (go/java/c_sharp/c/cpp/php/ruby/kotlin/swift/scala/lua/elixir/bash). No JS twin.
#[path = "skills/codebase/_helpers/lang-profiles.rs"]
pub mod codebase__helpers_lang_profiles;

// Hand-registered (do NOT run expand.mjs — see comment above): skills/codebase/jsx-to-html.mjs
// twin, built on parse-treesitter.rs's parseToTree. Not yet in frontier.json (this pass adds it);
// once a batch runs `expand.mjs` off the updated frontier this block can move into the managed
// section — left hand-registered here so cargo check works before that regen happens.
#[path = "skills/codebase/jsx-to-html.rs"]
pub mod codebase_jsx_to_html;

// Hand-registered (do NOT run expand.mjs — see comment above): skills/codebase/design-components-extract.mjs
// twin, built on parse-treesitter.rs's parseToTree + codebase-file-read.rs's codebaseFileRead. Not
// yet in frontier.json (this pass adds it); once a batch runs `expand.mjs` off the updated
// frontier this block can move into the managed section — left hand-registered here so cargo
// check works before that regen happens.
#[path = "skills/codebase/design-components-extract.rs"]
pub mod codebase_design_components_extract;

// Hand-registered (do NOT run expand.mjs — see comment above): skills/constitution/_helpers/detectors.mjs
// twin, built on ts-query.rs's queryMatches for the `ast` detector tier. langsOfPath / pathMatchesAny /
// lawApplies / exceptionCovers / resolveLayer are fixtured no-db helpers; globToRegex (returns a Regex)
// and runDetector (needs a live Tree/Language for the `ast` tier) are TRANSITIVE — real native code +
// #[cfg(test)] coverage, no JSON fixture, see the module doc. Not yet in frontier.json (this pass adds
// it); once a batch runs `expand.mjs` off the updated frontier this block can move into the managed
// section — left hand-registered here so cargo check works before that regen happens.
#[path = "skills/constitution/_helpers/detectors.rs"]
pub mod constitution__helpers_detectors;
// Hand-registered (2026-09-12): the project's own lint rules, read so a seed law defers to the
// codebase's decision (`eqeqeq: allow-null`). No JS twin.
#[path = "skills/constitution/_helpers/project-rules.rs"]
pub mod constitution__helpers_project_rules;
// The pane CONTEXT PACK (2026-09-14): what a code-agent pane reads before it explores — the
// selection model, the deterministic markdown renderers, the brief block, and the bake action
// that materialises it all into the worktree. No JS twins.
#[path = "skills/cockpit/_helpers/context-pack.rs"]
pub mod cockpit__helpers_context_pack;
#[path = "skills/cockpit/_helpers/pane-brief-md.rs"]
pub mod cockpit__helpers_pane_brief_md;
// An agent's skills, specialists and laws in the place ITS seat reads (Cursor's .cursor/, Grok's .grok/, Antigravity's .agents/rules).
#[path = "skills/cockpit/_helpers/seat-files.rs"]
pub mod cockpit__helpers_seat_files;
#[path = "skills/cockpit/_helpers/context-catalog.rs"]
pub mod cockpit__helpers_context_catalog;
// The web apps installed on this machine, the store of official MCPs, and the servers an agent's box hears of.
#[path = "skills/cockpit/_helpers/webapps.rs"]
pub mod cockpit__helpers_webapps;
#[path = "skills/cockpit/_helpers/engine-usage.rs"]
pub mod cockpit__helpers_engine_usage;
#[path = "skills/cockpit/_helpers/engine-handoff.rs"]
pub mod cockpit__helpers_engine_handoff;
#[path = "skills/constitution/_helpers/review-markdown.rs"]
pub mod constitution__helpers_review_markdown;
#[path = "skills/cockpit/cockpit-pane-context-bake.rs"]
pub mod cockpit_cockpit_pane_context_bake;

// Hand-registered (do NOT run expand.mjs — see comment above): skills/constitution/constitution-check.mjs
// twin, THE MEASURED merge gate. Built on constitution-law.rs (composedConstitution/resolveFrameworks),
// ts-query.rs (grammarForPath/getTree), cognitive.rs's analyzeTree (newly ported this pass — see that
// module's doc), and detectors.rs (lawApplies/exceptionCovers/runDetector). Not yet in frontier.json
// (this pass adds it); once a batch runs `expand.mjs` off the updated frontier this block can move into
// the managed section — left hand-registered here so cargo check works before that regen happens.
#[path = "skills/constitution/constitution-check.rs"]
pub mod constitution_constitution_check;

// Hand-registered (do NOT run expand.mjs — see comment above): skills/codebase/parse-js.mjs twin,
// the acorn/acorn-jsx JS/JSX language adapter for ast-parse.mjs. parseJs is a fixtured no-db
// helper built on the native `oxc_parser` crate (see the module doc for the architecture decision
// and the AST-shape differences from acorn's ESTree this twin bridges). Not yet in frontier.json
// (this pass adds it); once a batch runs `expand.mjs` off the updated frontier this block can move
// into the managed section — left hand-registered here so cargo check works before that regen happens.
#[path = "skills/codebase/parse-js.rs"]
pub mod codebase_parse_js;

// Flow-typed JS -> something oxc's TSX grammar reads (the React repo's 1160 `@flow` files were "not parsed").
#[path = "skills/codebase/flow-strip.rs"]
pub mod codebase_flow_strip;

// Where the index stops, said in words before an agent trusts the intel (the crates/ blind spot, 2026-10-01).
#[path = "skills/codebase/coverage-guard.rs"]
pub mod codebase_coverage_guard;

// Hand-registered (do NOT run expand.mjs — see comment above): skills/codebase/ast-parse.mjs
// twin, the language-agnostic parse dispatcher wiring the parse-js (JS/JSX) and parse-treesitter
// (TS/TSX/Python) adapters together. astParse (default) is a fixtured standard-lane fn;
// supportedExtensions is a fixtured 0-arg no-db helper; adapterFor is class:"transitive" (returns
// the matched adapter's langs/exts, dropping the unportable `parse` closure — see the module doc).
// Not yet in frontier.json (this pass adds it); once a batch runs `expand.mjs` off the updated
// frontier this block can move into the managed section — left hand-registered here so cargo
// check works before that regen happens.
#[path = "skills/codebase/ast-parse.rs"]
pub mod codebase_ast_parse;

// Hand-registered (do NOT run expand.mjs — see comment above): skills/codebase/conformance-verify.mjs
// twin, the author-time QA/SECURITY gate. conformanceVerify (default) is a fixtured standard-lane
// fn built on ast-parse.rs's astParse (parses the authored files, AST #2) plus code_nodes/code_edges
// reads for the blueprint conformance / scope / security / blast-radius checks. Not yet in
// frontier.json (this pass adds it); once a batch runs `expand.mjs` off the updated frontier this
// block can move into the managed section — left hand-registered here so cargo check works before
// that regen happens.
#[path = "skills/codebase/conformance-verify.rs"]
pub mod codebase_conformance_verify;

// Hand-registered (do NOT run expand.mjs — see comment above): skills/external/auth-sign.mjs
// twin. computeSig is a fixtured no-db helper; authSign (the default export) is BESPOKE NATIVE —
// real key-file fs I/O + a genuinely-random nonce that directly affects the observable output, no
// JSON fixture, see the module doc. Not yet in frontier.json (this pass adds it); once a batch
// runs `expand.mjs` off the updated frontier this block can move into the managed section — left
// hand-registered here so cargo check works before that regen happens.
#[path = "skills/external/auth-sign.rs"]
pub mod external_auth_sign;

// Hand-registered (do NOT run expand.mjs — see comment above): skills/external/auth-verify.mjs
// twin. authVerify (the sole export) is BESPOKE NATIVE — real key-file fs I/O against a
// real-fs-anchored fixed path plus real wall-clock freshness checking, no JSON fixture, see the
// module doc. Not yet in frontier.json (this pass adds it); once a batch runs `expand.mjs` off
// the updated frontier this block can move into the managed section — left hand-registered here
// so cargo check works before that regen happens.
#[path = "skills/external/auth-verify.rs"]
pub mod external_auth_verify;

// Hand-registered (do NOT run expand.mjs — see comment above): skills/codebase/code-scope-get.mjs
// twin. codeScopeGet (default) is a fixtured standard-lane fn; loadScope (named) is a fixtured
// db-first helper shared with codebase-onboard's rebuild path. Not yet in frontier.json (this pass
// adds it); once a batch runs `expand.mjs` off the updated frontier this block can move into the
// managed section — left hand-registered here so cargo check works before that regen happens.
#[path = "skills/codebase/code-scope-get.rs"]
pub mod codebase_code_scope_get;

// Hand-registered (do NOT run expand.mjs — see comment above): skills/blueprint/_helpers/stub-verify.mjs
// twin, the deterministic AST-skeleton VERIFY. verifyAgainstStub is a fixtured no-db helper (3
// positional args) built on blueprint-compose.rs's blueprintCompose and ast-parse.rs's astParse
// (both standard-lane twins, called here with a throwaway in-memory Connection). Not yet in
// frontier.json (this pass adds it); once a batch runs `expand.mjs` off the updated frontier this
// block can move into the managed section — left hand-registered here so cargo check works before
// that regen happens.
#[path = "skills/blueprint/_helpers/stub-verify.rs"]
pub mod blueprint__helpers_stub_verify;

// Hand-registered (do NOT run expand.mjs — see comment above): skills/blueprint/_helpers/code-checks.mjs
// twin, the shared deterministic check primitives for the blueprint linters. All 6 named exports
// are no-db helpers; parseDecls is the module's only async fn (awaited to its resolved value per
// the async-lane rule) and is built on ast-parse.rs's astParse (a standard-lane twin, called here
// with a throwaway in-memory Connection). Not yet in frontier.json (this pass adds it); once a
// batch runs `expand.mjs` off the updated frontier this block can move into the managed section —
// left hand-registered here so cargo check works before that regen happens.
#[path = "skills/blueprint/_helpers/code-checks.rs"]
pub mod blueprint__helpers_code_checks;

// Hand-registered (do NOT run expand.mjs — see comment above): skills/blueprint/_helpers/author-accept.mjs
// twin, THE ONE accept rule for any code-emitting AI call. Both exports (isTruncatedCall,
// acceptAuthoredCode) are db-first helpers; acceptAuthoredCode is the module's only async fn
// (awaited to its resolved value per the async-lane rule) and cross-calls
// blueprint__helpers_code_checks's parseDecls/isDegenerateSource twins. Not yet in
// frontier.json (this pass adds it); once a batch runs `expand.mjs` off the updated frontier
// this block can move into the managed section — left hand-registered here so cargo check
// works before that regen happens.
#[path = "skills/blueprint/_helpers/author-accept.rs"]
pub mod blueprint__helpers_author_accept;

// Hand-registered (do NOT run expand.mjs — see comment above): skills/ai/ai-call.mjs twin, the
// AI-call chokepoint 147 other skills depend on. 9 named exports (canonicalize, hashInput,
// detectStringifiedJsonCollection, validateAndDecompose, validateInputs, renderTemplate,
// extractPlaceholders, checkFeedQuality, reconstructRenderedPrompt) are fixtured no-db helpers;
// `aiCall` (the default export) is PARITY-DEFERRED — a real network BYOK provider dispatch, no
// fixture/frontier entry, see the module doc. Not yet in frontier.json (this pass adds it); once a
// batch runs `expand.mjs` off the updated frontier this block can move into the managed section —
// left hand-registered here so cargo check works before that regen happens.
#[path = "skills/ai/ai-call.rs"]
pub mod ai_ai_call;

// Hand-registered (do NOT run expand.mjs — see comment above): skills/ai/prompt-lint.mjs twin, a
// read-only scorecard over every ACTIVE prompt template (brain.prompt_templates +
// brain.prompt_output_fields, cross-db `attach: ["brain"]`). Single default-export unit
// (promptLint, db READ lane); reuses ai_ai_call's extractPlaceholders for placeholder_count. Not
// yet in frontier.json (this pass adds it); once a batch runs `expand.mjs` off the updated
// frontier this block can move into the managed section — left hand-registered here so cargo
// check works before that regen happens.
#[path = "skills/ai/prompt-lint.rs"]
pub mod ai_prompt_lint;

// ── BESPOKE-NATIVE wave 1 (2026-08-07) — real Rust + native tests, zero frontier units ──
#[path = "skills/cockpit/cockpit-pane-remove.rs"]
pub mod cockpit_cockpit_pane_remove;
#[path = "skills/codebase/design-snapshot-build.rs"]
pub mod codebase_design_snapshot_build;
#[path = "skills/cockpit/cockpit-pane-add.rs"]
pub mod cockpit_cockpit_pane_add;
#[path = "skills/cockpit/cockpit-pr-merge.rs"]
pub mod cockpit_cockpit_pr_merge;
#[path = "skills/agents/in/terminal-sandbox.rs"]
pub mod agents_in_terminal_sandbox;
#[path = "skills/codebase/lint-install.rs"]
pub mod codebase_lint_install;
#[path = "skills/codebase/design-tokens-extract.rs"]
pub mod codebase_design_tokens_extract;
#[path = "skills/bench/bench-laws-overhead.rs"]
pub mod bench_bench_laws_overhead;
#[path = "skills/codebase/lint-ruff.rs"]
pub mod codebase_lint_ruff;
#[path = "skills/audit/timeline-legacy-read.rs"]
pub mod audit_timeline_legacy_read;
#[path = "skills/agents/dev/dev-build-gate.rs"]
pub mod agents_dev_dev_build_gate;
#[path = "skills/agents/in/interviewer-dispatch.rs"]
pub mod agents_in_interviewer_dispatch;
#[path = "skills/agents/qa/qa-senior-review.rs"]
pub mod agents_qa_qa_senior_review;
#[path = "skills/agents/man/man-diagnose-failure.rs"]
pub mod agents_man_man_diagnose_failure;

// ── BESPOKE-NATIVE wave 2 (2026-08-07) ──
#[path = "skills/codebase/test-suite-run.rs"]
pub mod codebase_test_suite_run;
#[path = "skills/codebase/code-regenerate-stage.rs"]
pub mod codebase_code_regenerate_stage;
#[path = "skills/codebase/typecheck-ts.rs"]
pub mod codebase_typecheck_ts;
#[path = "skills/codebase/typecheck-py.rs"]
pub mod codebase_typecheck_py;
#[path = "skills/codebase/typecheck-install.rs"]
pub mod codebase_typecheck_install;
#[path = "skills/engine/registry.rs"]
pub mod engine_registry;
#[path = "skills/engine/load-skill.rs"]
pub mod engine_load_skill;
#[path = "skills/driver/gate-telemetry.rs"]
pub mod driver_gate_telemetry;
#[path = "skills/ai/ai-suggest-files-touched.rs"]
pub mod ai_ai_suggest_files_touched;
#[path = "skills/codebase/atlas-ingest.rs"]
pub mod codebase_atlas_ingest;
#[path = "skills/eval/eval-run-one.rs"]
pub mod eval_eval_run_one;
#[path = "skills/contract/contract-draft-auto.rs"]
pub mod contract_contract_draft_auto;
#[path = "skills/constitution/constitution-neighborhood.rs"]
pub mod constitution_constitution_neighborhood;
#[path = "skills/agents/dev/dev-implement-code.rs"]
pub mod agents_dev_dev_implement_code;

// ── BESPOKE-NATIVE wave 3 (2026-08-07) ──
#[path = "skills/reconcilers/index.rs"]
pub mod reconcilers_index;
#[path = "skills/external/test-run.rs"]
pub mod external_test_run;
#[path = "skills/codebase/typecheck-project.rs"]
pub mod codebase_typecheck_project;
#[path = "skills/system/byok-test.rs"]
pub mod system_byok_test;
#[path = "skills/system/code-file.rs"]
pub mod system_code_file;
#[path = "skills/system/git-pending-commit.rs"]
pub mod system_git_pending_commit;
#[path = "skills/system/focus-resolve.rs"]
pub mod system_focus_resolve;
#[path = "skills/system/provider-test-call.rs"]
pub mod system_provider_test_call;
#[path = "skills/audit/consistency-scan.rs"]
pub mod audit_consistency_scan;
#[path = "skills/engine/open-db.rs"]
pub mod engine_open_db;
#[path = "skills/system/review-diff-compute.rs"]
pub mod system_review_diff_compute;
#[path = "skills/system/reconcile.rs"]
pub mod system_reconcile;
#[path = "skills/codebase/magazine-observe.rs"]
pub mod codebase_magazine_observe;
#[path = "skills/ultra/ultra-run.rs"]
pub mod ultra_ultra_run;
#[path = "skills/render/rag-flow-html.rs"]
pub mod render_rag_flow_html;

// ── conformable wave 4 (2026-08-07) ──
#[path = "skills/bench/bench-openDb.rs"]
pub mod bench_bench_open_db;
#[path = "skills/blueprint/blueprint-parse.rs"]
pub mod blueprint_blueprint_parse;
#[path = "skills/codebase/design-rebuild.rs"]
pub mod codebase_design_rebuild;

// ── BESPOKE-NATIVE wave 5 re-route (2026-08-07) ──
#[path = "skills/codebase/code-regenerate-merge.rs"]
pub mod codebase_code_regenerate_merge;
#[path = "skills/codebase/typecheck-run.rs"]
pub mod codebase_typecheck_run;
#[path = "skills/codebase/codebase-scope.rs"]
pub mod codebase_codebase_scope;
#[path = "skills/codebase/lint-run.rs"]
pub mod codebase_lint_run;
#[path = "skills/agents/man/man-interpret-diagnosis.rs"]
pub mod agents_man_man_interpret_diagnosis;
#[path = "skills/codebase/codebase-sync.rs"]
pub mod codebase_codebase_sync;
#[path = "skills/render/consistency-report.rs"]
pub mod render_consistency_report;
#[path = "skills/system/project-registry.rs"]
pub mod system_project_registry;
#[path = "skills/system/ai-log-export.rs"]
pub mod system_ai_log_export;

// ── LLM-partial wave 6 (2026-08-07) ──
#[path = "skills/agents/git-manager/git-manager-commit-msg-write.rs"]
pub mod agents_git_manager_git_manager_commit_msg_write;
#[path = "skills/agents/in/interviewer-ai-fallback.rs"]
pub mod interviewer_ai_fallback;
#[path = "skills/agents/dev/dev-exec-sandbox.rs"]
pub mod agents_dev_dev_exec_sandbox;
#[path = "skills/agents/in/interviewer-clarify-questionnaire.rs"]
pub mod agents_in_interviewer_clarify_questionnaire;
#[path = "skills/agents/in/episode-consolidate.rs"]
pub mod agents_in_episode_consolidate;
#[path = "skills/agents/in/interviewer-present.rs"]
pub mod agents_in_interviewer_present;
#[path = "skills/agents/in/interviewer-ideation-multistep.rs"]
pub mod agents_in_interviewer_ideation_multistep;
#[path = "skills/agents/in/investigate-present.rs"]
pub mod investigate_present;
#[path = "skills/agents/in/book-run.rs"]
pub mod agents_in_book_run;
#[path = "skills/agents/in/skill-library-mint.rs"]
pub mod agents_in_skill_library_mint;
#[path = "skills/agents/in/investigate-synthesize.rs"]
pub mod agents_in_investigate_synthesize;
#[path = "skills/agents/in/book-swarm.rs"]
pub mod agents_in_book_swarm;
#[path = "skills/agents/in/turn-rewrite.rs"]
pub mod agents_in_turn_rewrite;
#[path = "skills/agents/in/repl-direct.rs"]
pub mod agents_in_repl_direct;

// ── LLM-partial wave 7 (2026-08-08) ──
#[path = "skills/agents/pl/pl-contract-review.rs"]
pub mod agents_pl_pl_contract_review;
#[path = "skills/agents/pl/pl-session-advance-decompose.rs"]
pub mod agents_pl_pl_session_advance_decompose;
#[path = "skills/agents/pl/pl-research.rs"]
pub mod agents_pl_pl_research;
#[path = "skills/agents/pl/pl-session-advance-clarify.rs"]
pub mod agents_pl_pl_session_advance_clarify;
#[path = "skills/agents/keeper/keeper-card-consolidate.rs"]
pub mod agents_keeper_keeper_card_consolidate;
#[path = "skills/agents/man/man-card-review.rs"]
pub mod agents_man_man_card_review;
#[path = "skills/agents/qa/qa-feedback-generate.rs"]
pub mod agents_qa_qa_feedback_generate;
#[path = "skills/ai/ai-call-replay.rs"]
pub mod ai_ai_call_replay;
#[path = "skills/agents/tl/tl-contract-redraft.rs"]
pub mod agents_tl_tl_contract_redraft;
#[path = "skills/agents/qa/qa-contract-review.rs"]
pub mod agents_qa_qa_contract_review;
#[path = "skills/blueprint/lint-explain.rs"]
pub mod blueprint_lint_explain;
#[path = "skills/blueprint/blueprint-describe.rs"]
pub mod blueprint_blueprint_describe;
#[path = "skills/ai/prompt-quality-report.rs"]
pub mod ai_prompt_quality_report;
#[path = "skills/agents/tl/_helpers/refine-shared.rs"]
pub mod agents_tl__helpers_refine_shared;

// ── LLM-partial wave 8 (2026-08-08) ──
#[path = "skills/bugfinder/bugfinder-shards.rs"]
pub mod bugfinder_bugfinder_shards;
#[path = "skills/bugfinder/bugfinder-sweep.rs"]
pub mod bugfinder_bugfinder_sweep;
#[path = "skills/blueprint/pl-ratify-contract.rs"]
pub mod blueprint_pl_ratify_contract;
#[path = "skills/cockpit/cockpit-pr-analyze.rs"]
pub mod cockpit_cockpit_pr_analyze;
#[path = "skills/cockpit/cockpit-session-narrate.rs"]
pub mod cockpit_cockpit_session_narrate;
#[path = "skills/codebase/architecture-digest.rs"]
pub mod codebase_architecture_digest;
#[path = "skills/cockpit/cockpit-promote.rs"]
pub mod cockpit_cockpit_promote;
#[path = "skills/codebase/atlas-explain-snippet.rs"]
pub mod codebase_atlas_explain_snippet;
#[path = "skills/codebase/atlas-logic-spec.rs"]
pub mod codebase_atlas_logic_spec;
#[path = "skills/cockpit/cockpit-plan-tribunal.rs"]
pub mod cockpit_cockpit_plan_tribunal;
#[path = "skills/codebase/atlas-file-snapshot.rs"]
pub mod codebase_atlas_file_snapshot;
#[path = "skills/agents/tl/tl-plan-contract.rs"]
pub mod agents_tl_tl_plan_contract;
#[path = "skills/cockpit/cockpit-plan-generate.rs"]
pub mod cockpit_cockpit_plan_generate;

// ── wave 9 + telemetry/atlas-diagram unblock (2026-08-08) ──
#[path = "skills/ai/_helpers/ai-telemetry.rs"]
pub mod ai__helpers_ai_telemetry;
#[path = "skills/codebase/atlas-architecture-diagram.rs"]
pub mod codebase_atlas_architecture_diagram;
#[path = "skills/codebase/atlas-review.rs"]
pub mod codebase_atlas_review;
#[path = "skills/codebase/code-describe.rs"]
pub mod codebase_code_describe;
#[path = "skills/codebase/code-docs-overview.rs"]
pub mod codebase_code_docs_overview;
#[path = "skills/codebase/atlas-insights-review.rs"]
pub mod codebase_atlas_insights_review;
#[path = "skills/codebase/atlas-symbol-explain.rs"]
pub mod codebase_atlas_symbol_explain;
#[path = "skills/codebase/code-regenerate.rs"]
pub mod codebase_code_regenerate;
#[path = "skills/codebase/atlas-code-sample.rs"]
pub mod codebase_atlas_code_sample;
#[path = "skills/cockpit/cockpit-plan-judge.rs"]
pub mod cockpit_cockpit_plan_judge;
#[path = "skills/agents/in/interviewer-regenerate-presentation.rs"]
pub mod agents_in_interviewer_regenerate_presentation;
#[path = "skills/codebase/data-story-propose.rs"]
pub mod codebase_data_story_propose;

// ── LLM-partial wave 10 (2026-08-08) ──
#[path = "skills/codebase/test-flow-gen.rs"]
pub mod codebase_test_flow_gen;
#[path = "skills/codebase/file-summarize.rs"]
pub mod codebase_file_summarize;
#[path = "skills/codebase/design-style-guide.rs"]
pub mod codebase_design_style_guide;
#[path = "skills/codebase/symbol-summarize.rs"]
pub mod codebase_symbol_summarize;
#[path = "skills/constitution/constitution-brief.rs"]
pub mod constitution_constitution_brief;
#[path = "skills/cockpit/pr-test-run.rs"]
pub mod cockpit_pr_test_run;
#[path = "skills/constitution/constitution-business-gen.rs"]
pub mod constitution_constitution_business_gen;
#[path = "skills/entities/card-accuracy-score.rs"]
pub mod entities_card_accuracy_score;
#[path = "skills/constitution/constitution-insights.rs"]
pub mod constitution_constitution_insights;
#[path = "skills/constitution/constitution-law-build.rs"]
pub mod constitution_law_build;
#[path = "skills/constitution/constitution-pack-gen.rs"]
pub mod constitution_pack_gen;
#[path = "skills/entities/card-decompose-into-children.rs"]
pub mod entities_card_decompose_into_children;

// ── LLM-partial wave 11 (2026-08-08) ──
#[path = "skills/entities/card-score.rs"]
pub mod entities_card_score;
#[path = "skills/entities/card-digest.rs"]
pub mod entities_card_digest;
#[path = "skills/entities/epic-evaluate.rs"]
pub mod entities_epic_evaluate;
#[path = "skills/entities/risk-classify.rs"]
pub mod entities_risk_classify;
#[path = "skills/entities/card-evaluate.rs"]
pub mod entities_card_evaluate;
#[path = "skills/notebooks/notebook-compare.rs"]
pub mod notebooks_notebook_compare;
#[path = "skills/heimdall/heimdall-prompt-patch.rs"]
pub mod heimdall_heimdall_prompt_patch;
#[path = "skills/heimdall/heimdall-ai-tag.rs"]
pub mod heimdall_heimdall_ai_tag;
#[path = "skills/notebooks/notebook-mindmap.rs"]
pub mod notebooks_notebook_mindmap;
#[path = "skills/notebooks/notebook-quiz.rs"]
pub mod notebooks_notebook_quiz;
#[path = "skills/notebooks/notebook-flashcards.rs"]
pub mod notebooks_notebook_flashcards;
#[path = "skills/swarm/author-blocks.rs"]
pub mod swarm_author_blocks;
#[path = "skills/heimdall/prompt-section-improve.rs"]
pub mod heimdall_prompt_section_improve;
#[path = "skills/swarm/manifest-design.rs"]
pub mod swarm_manifest_design;

// ── wave 12 (2026-08-08) ──
#[path = "skills/swarm/subagent-tune.rs"]
pub mod swarm_subagent_tune;
#[path = "skills/heimdall/heimdall-prompt-tags.rs"]
pub mod heimdall_heimdall_prompt_tags;
#[path = "skills/system/card-body-enrich.rs"]
pub mod system_card_body_enrich;
#[path = "skills/system/card-body-reshape.rs"]
pub mod system_card_body_reshape;
#[path = "skills/system/card-discuss.rs"]
pub mod system_card_discuss;
#[path = "skills/heimdall/heimdall-tag-assist.rs"]
pub mod heimdall_heimdall_tag_assist;
#[path = "skills/system/code-metadata.rs"]
pub mod system_code_metadata;
#[path = "skills/driver/driver-forensics.rs"]
pub mod driver_driver_forensics;
#[path = "skills/audit/timeline-rotate.rs"]
pub mod audit_timeline_rotate;
#[path = "skills/system/workspace-provision.rs"]
pub mod system_workspace_provision;
#[path = "skills/swarm/review.rs"]
pub mod swarm_review;
#[path = "skills/system/workspace-remove.rs"]
pub mod system_workspace_remove;
#[path = "skills/system/workspace-runtime.rs"]
pub mod system_workspace_runtime;
#[path = "skills/constitution/constitution-judge.rs"]
pub mod constitution_constitution_judge;

// ── wave 13 (2026-08-08) ──
#[path = "skills/system/card-body-enrich-one.rs"]
pub mod system_card_body_enrich_one;
#[path = "skills/bench/bench-run-all.rs"]
pub mod bench_bench_run_all;
#[path = "skills/system/card-body-enrich-canary.rs"]
pub mod system_card_body_enrich_canary;
#[path = "skills/system/card-body-reshape-one.rs"]
pub mod system_card_body_reshape_one;
#[path = "skills/system/workspace-add.rs"]
pub mod system_workspace_add;
#[path = "skills/system/card-body-enrich-bulk.rs"]
pub mod system_card_body_enrich_bulk;
#[path = "skills/codebase/atlas-snapshot-all.rs"]
pub mod codebase_atlas_snapshot_all;
#[path = "skills/agents/dev/dev-plan-impl-split.rs"]
pub mod agents_dev_dev_plan_impl_split;
#[path = "skills/swarm/review-contract.rs"]
pub mod swarm_review_contract;
#[path = "skills/codebase/code-digest-snapshot.rs"]
pub mod codebase_code_digest_snapshot;
#[path = "skills/agents/tl/tl-author-snippets.rs"]
pub mod agents_tl_tl_author_snippets;
#[path = "skills/system/reconcile-apply.rs"]
pub mod system_reconcile_apply;
#[path = "skills/heimdall/heimdall-ai-tag-batch.rs"]
pub mod heimdall_heimdall_ai_tag_batch;
#[path = "skills/blueprint/blueprint-judge.rs"]
pub mod blueprint_blueprint_judge;

// ── wave 14 tail (2026-08-08) ──
#[path = "skills/agents/in/interviewer-greeting-floor.rs"]
pub mod agents_in_interviewer_greeting_floor;
#[path = "skills/system/card-score-backfill.rs"]
pub mod system_card_score_backfill;
#[path = "skills/system/card-body-enrich-persist.rs"]
pub mod system_card_body_enrich_persist;
#[path = "skills/system/card-review-blueprint-generate.rs"]
pub mod system_card_review_blueprint_generate;
#[path = "skills/blueprint/_helpers/typecheck-change.rs"]
pub mod blueprint__helpers_typecheck_change;
#[path="skills/external/anthropic-call.rs"]
pub mod external_anthropic_call;
#[path = "skills/external/deepseek-call.rs"]
pub mod external_deepseek_call;
#[path = "skills/external/openai-call.rs"]
pub mod external_openai_call;
#[path = "skills/system/byok-local-models-list.rs"]
pub mod system_byok_local_models_list;
#[path = "skills/external/ollama-call.rs"]
pub mod external_ollama_call;
#[path = "skills/external/openrouter-call.rs"]
pub mod external_openrouter_call;
#[path = "skills/driver/driver-tick.rs"]
pub mod driver_driver_tick;
#[path = "skills/external/openrouter-fetch-logs.rs"]
pub mod external_openrouter_fetch_logs;

// ── wave 15 trunks (2026-08-08) ──
#[path = "skills/codebase/code-decorate.rs"]
pub mod codebase_code_decorate;
#[path = "skills/audit/extract-jsdoc-description.rs"]
pub mod audit_extract_jsdoc_description;
#[path = "skills/audit/extract-rust-skill-meta.rs"]
pub mod audit_extract_rust_skill_meta;
#[path = "skills/audit/extract-input-schema.rs"]
pub mod audit_extract_input_schema;
#[path = "skills/agents/tl/tl-refine-microtasks.rs"]
pub mod agents_tl_tl_refine_microtasks;
#[path = "skills/cockpit/cockpit-pane-message.rs"]
pub mod cockpit_cockpit_pane_message;
#[path = "skills/blueprint/lint-gate.rs"]
pub mod blueprint_lint_gate;
#[path = "skills/constitution/constitution-agent-run.rs"]
pub mod constitution_constitution_agent_run;
#[path = "skills/bugfinder/bugfinder-run.rs"]
pub mod bugfinder_bugfinder_run;
#[path = "skills/driver/driver-run.rs"]
pub mod driver_driver_run;

// ── wave 16 cascade (2026-08-08) ──
#[path = "skills/cockpit/cockpit-pane-file.rs"]
pub mod cockpit_cockpit_pane_file;
#[path = "skills/cockpit/cockpit-pane-brief.rs"]
pub mod cockpit_cockpit_pane_brief;
#[path = "skills/cockpit/cockpit-swarm-run.rs"]
pub mod cockpit_cockpit_swarm_run;
#[path = "skills/constitution/constitution-patterns.rs"]
pub mod constitution_constitution_patterns;
#[path = "skills/cockpit/cockpit-formation-run.rs"]
pub mod cockpit_cockpit_formation_run;
#[path = "skills/audit/catalog-refresh.rs"]
pub mod audit_catalog_refresh;
#[path = "skills/constitution/constitution-class.rs"]
pub mod constitution_constitution_class;
#[path = "skills/constitution/constitution-editor.rs"]
pub mod constitution_constitution_editor;
#[path = "skills/external/mcp-descriptors.rs"]
pub mod external_mcp_descriptors;
#[path = "skills/bugfinder/bugfinder-schedule.rs"]
pub mod bugfinder_bugfinder_schedule;

// ── wave 17 endgame (2026-08-08) ──
#[path = "skills/agents/in/deepsearch-run.rs"]
pub mod agents_in_deepsearch_run;
#[path = "skills/ai/ai-image-call.rs"]
pub mod ai_ai_image_call;
#[path = "skills/agents/pl/pl-session-advance-research.rs"]
pub mod agents_pl_pl_session_advance_research;
#[path = "skills/agents/dev/dev-acceptance-run.rs"]
pub mod agents_dev_dev_acceptance_run;
#[path = "skills/agents/in/interviewer-chat.rs"]
pub mod agents_in_interviewer_chat;
#[path = "skills/codebase/feature-wizard.rs"]
pub mod codebase_feature_wizard;


// ── wave 18 (2026-08-08) qa-acceptance-run + network adapters ──
#[path = "skills/agents/qa/qa-acceptance-run.rs"]
pub mod agents_qa_qa_acceptance_run;
#[path = "skills/external/gemini-call.rs"]
pub mod external_gemini_call;
#[path = "skills/external/openrouter-image-call.rs"]
pub mod external_openrouter_image_call;
#[path = "skills/ai/openrouter-spend.rs"]
pub mod ai_openrouter_spend;
#[path = "skills/codebase/atlas-deps-audit.rs"]
pub mod codebase_atlas_deps_audit;
#[path = "skills/notebooks/notebook-ingest.rs"]
pub mod notebooks_notebook_ingest;

// ── wave 19 (2026-08-08) deferred-boundary bespoke-native ──
#[path = "skills/agents/qa/qa-gate-contract-verify.rs"]
pub mod agents_qa_qa_gate_contract_verify;
#[path = "skills/constitution/constitution-research.rs"]
pub mod constitution_constitution_research;
#[path = "skills/codebase/magazine-press.rs"]
pub mod codebase_magazine_press;
#[path = "skills/codebase/design-capture-live.rs"]
pub mod codebase_design_capture_live;
#[path = "skills/rag/recall-iterative.rs"]
pub mod rag_recall_iterative;
#[path = "skills/engine/roots.rs"]
pub mod engine_roots;

// ── wave 20 (2026-08-08) embed chain (deferred embed.rs stub) ──
#[path = "skills/external/embed.rs"]
pub mod external_embed;
#[path = "skills/rag/rag-recall.rs"]
pub mod rag_rag_recall;
#[path = "skills/rag/rag-sweep.rs"]
pub mod rag_rag_sweep;
#[path = "skills/codebase/smart-context-bundle.rs"]
pub mod codebase_smart_context_bundle;
#[path = "skills/codebase/context-bundle.rs"]
pub mod codebase_context_bundle;
#[path = "skills/entities/memory-rag-query.rs"]
pub mod entities_memory_rag_query;

// ── wave 21 (2026-08-08) embed/magazine/constitution/browser fan-out ──
#[path="skills/notebooks/notebook-autoscope.rs"]
pub mod notebooks_notebook_autoscope;
#[path = "skills/codebase/codebase-refresh.rs"]
pub mod codebase_codebase_refresh;
#[path = "skills/notebooks/notebook-generate.rs"]
pub mod notebooks_notebook_generate;
#[path = "skills/notebooks/notebook-ask.rs"]
pub mod notebooks_notebook_ask;
#[path = "skills/agents/in/widget-scope-resolve.rs"]
pub mod agents_in_widget_scope_resolve;
#[path="skills/blueprint/pl-screening-clarify.rs"]
pub mod blueprint_pl_screening_clarify;
#[path = "skills/blueprint/pl-screening-research.rs"]
pub mod blueprint_pl_screening_research;
#[path="skills/agents/in/interviewer-embed-turn.rs"]
pub mod agents_in_interviewer_embed_turn;
#[path="skills/codebase/codebase-onboard.rs"]
pub mod codebase_codebase_onboard;
#[path="skills/blueprint/pl-screening-refine.rs"]
pub mod blueprint_pl_screening_refine;
#[path = "skills/entities/catalog-embed-all.rs"]
pub mod entities_catalog_embed_all;
#[path = "skills/codebase/ui-explore.rs"]
pub mod codebase_ui_explore;
#[path="skills/ultra/ultra-concepts.rs"]
pub mod ultra_ultra_concepts;
#[path = "skills/codebase/ui-story-replay.rs"]
pub mod codebase_ui_story_replay;
#[path="skills/codebase/ui-record.rs"]
pub mod codebase_ui_record;
#[path="skills/constitution/constitution-research-suggest.rs"]
pub mod constitution_constitution_research_suggest;
#[path = "skills/codebase/course-press.rs"]
pub mod codebase_course_press;
#[path = "skills/blueprint/blueprint-design.rs"]
pub mod blueprint_blueprint_design;
#[path = "skills/blueprint/snippet-author.rs"]
pub mod blueprint_snippet_author;

// ── wave 22 (2026-08-08) cascades + kokoro deferred-stub chain ──
#[path="skills/codebase/codebase-integrate.rs"]
pub mod codebase_codebase_integrate;
#[path="skills/system/kokoro-speak.rs"]
pub mod system_kokoro_speak;
#[path="skills/constitution/constitution-soliloquy.rs"]
pub mod constitution_constitution_soliloquy;
#[path = "skills/agents/in/widget-generate.rs"]
pub mod agents_in_widget_generate;
#[path="skills/ultra/ultra-deep.rs"]
pub mod ultra_ultra_deep;
#[path="skills/agents/in/narration-render.rs"]
pub mod agents_in_narration_render;

// ── wave 23 (2026-08-08) genuine oxc AST ports ──
#[path="skills/notebooks/notebook-diagram.rs"]
pub mod notebooks_notebook_diagram;
#[path = "skills/codebase/code-lint.rs"]
pub mod codebase_code_lint;
#[path = "skills/bugfinder/bugfinder-existence-check.rs"]
pub mod bugfinder_bugfinder_existence_check;

// ── wave 24 (2026-08-08) FINAL: code-transform cascade + lint/anatomy oxc chains ──
#[path="skills/audit/lint-all-skills.rs"]
pub mod audit_lint_all_skills;
#[path = "skills/cockpit/cockpit-pane-turn.rs"]
pub mod cockpit_cockpit_pane_turn;
#[path="skills/entities/anatomy-snapshot.rs"]
pub mod entities_anatomy_snapshot;
#[path = "skills/audit/contract-audit-determinism.rs"]
pub mod audit_contract_audit_determinism;
#[path = "skills/audit/detect-bypass.rs"]
pub mod audit_detect_bypass;
#[path = "skills/audit/lint-error-class.rs"]
pub mod audit_lint_error_class;
#[path = "skills/audit/lint-skill.rs"]
pub mod audit_lint_skill;
#[path="skills/audit/extract-anatomy.rs"]
pub mod audit_extract_anatomy;
#[path = "skills/codebase/code-transform.rs"]
pub mod codebase_code_transform;
#[path = "skills/agents/in/interviewer-turn.rs"]
pub mod agents_in_interviewer_turn;

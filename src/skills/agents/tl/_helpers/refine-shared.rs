//! refine-shared.rs — twin of skills/agents/tl/_helpers/refine-shared.mjs (bespoke-native _helpers
//! module, not a dispatchable skill — like author-form.rs / apply-section-patch.rs, no frontier
//! entry/fixture). NAMES ARE CONTRACT: every JS `export` below keeps its exact identifier.
//!
//! PARITY-DEFERRED (single boundary, repeated per the module's two AI call sites — plan + per-file
//! author): both ultimately dispatch through `crate::ai_ai_call::aiCall`, the always-`Err` network
//! stub (see ai-call.rs's module doc). Everything AROUND that boundary is ported faithfully — this
//! is the "everything except the network leg" convention used throughout this session (book-run.rs,
//! man-interpret-diagnosis.rs): guards, prompt/template data, plan/author read-back, scope
//! expansion, coverage/echo/dry-run gates, sliced-retry pure helpers, failure-context assembly, the
//! deterministic spec/code lints, and the per-file authoring engine's control flow (its 3-attempt
//! ladder always exhausts to `failedFiles` here, since the AI leg never succeeds — same shape as
//! book-run.rs's `fac_call`).
//!
//! Cross-module (already-converted twins, reused not reimplemented): `crate::ai_prompt_find::promptFind`,
//! `crate::ai_prompt_add::promptAdd`, `crate::agents_dev_apply_section_patch::applyPatchToSource`,
//! `crate::agents_tl__helpers_author_form::{renderAuthorForm, parseAuthorForm, formatFormErrors}`,
//! `crate::engine_workspace_root::getWorkspaceRoot`, `crate::ai_ai_call::aiCall`.
//!
//! `AntilokiError`/`ErrorCodes` (engine/error.mjs) are themselves PARITY-DEFERRED at the class
//! level (error.rs's own doc) — every throw site here collapses to `Err("<skill>: <cause>".into())`,
//! the same convention as dev-implement-code.rs and every other bespoke twin in this crate.
//!
//! `applyPatchToSource`'s Rust twin returns `Value::Null` on failure instead of throwing (its own
//! module doc: "the Err path throws in JS ... error-message-fragile, not fixtured") — call sites
//! here that need the JS "did it throw" boolean treat `Null` as failure and synthesize a generic
//! reason string (the exact anchor-failure message text is not reachable through the public
//! `applyPatchToSource(Value, Value) -> Value` signature; downstream consumers here only branch on
//! ok/fail, never match the JS message verbatim — same disclosed gap as gateAuthoredPatches).
//!
//! `SUBSTRATE_ROOT` (workspace-root.mjs) has no `pub` accessor in its own twin (private there —
//! see that module's doc). `dryRunPatches` only uses it to build a real filesystem path for a
//! `fs::read_to_string`, not for a string-comparison containment check, so this file inlines the
//! same `ANTILOKI_SUBSTRATE_ROOT` env-override-else-`CARGO_MANIFEST_DIR`-parent derivation via a
//! plain `std::path::Path::join` (no win32 string-algorithm precision needed for a real fs read).
// QUALITY: 78/100 — deferred-aiCall dispatcher class (matches book-run.rs precedent); pure surface
// (lints, read-back, scope/coverage/sliced-retry helpers) is fully ported and native-tested.

use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::sync::OnceLock;

// ── standard helpers (playbook) ─────────────────────────────────────────────────────────────
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
fn js_string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Array(a) => a.iter().map(js_string).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}

// ══ Deterministic lint vocabulary — single source of truth ════════════════════════════════
// (See module doc — VAGUE_SPEC_RE etc. NAMES ARE CONTRACT: exported as fns since Rust has no
// `export const Regex`; .source() text below is byte-identical to the JS regex source.)
fn vague_spec_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)\b(as appropriate|appropriate(?:ly)?|as needed|if (?:necessary|needed|required)|as required|suitable|standard (?:configuration|settings|setup)|proper(?:ly)? configured|necessary (?:settings|options|configuration)|and so on|etc\.)\b").unwrap())
}
fn code_file_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)\.(?:ts|tsx|js|jsx|mjs|cjs)$").unwrap())
}
fn code_concrete_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\bexport\b|\bimport\b|module\.exports|\bfunction\b|=>").unwrap())
}
fn versioned_dep_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[\w@/.-]+@[\^~]?\d").unwrap())
}
const CODE_FENCE_PAT: &str = "```";
fn prose_preamble_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)^\s*(here (is|'s)|this (file|code|snippet)|the following|sure[,!]|certainly)").unwrap())
}

pub const VAGUE_SPEC_SRC: &str = r"\b(as appropriate|appropriate(?:ly)?|as needed|if (?:necessary|needed|required)|as required|suitable|standard (?:configuration|settings|setup)|proper(?:ly)? configured|necessary (?:settings|options|configuration)|and so on|etc\.)\b";
pub const CODE_FILE_SRC: &str = r"\.(?:ts|tsx|js|jsx|mjs|cjs)$";
pub const CODE_CONCRETE_SRC: &str = r"\bexport\b|\bimport\b|module\.exports|\bfunction\b|=>";
pub const VERSIONED_DEP_SRC: &str = r"[\w@/.-]+@[\^~]?\d";
pub const CODE_FENCE_SRC: &str = "```";
pub const PROSE_PREAMBLE_SRC: &str = r"^\s*(here (is|'s)|this (file|code|snippet)|the following|sure[,!]|certainly)";

// ══ prompt data (verbatim; `${...}` regex-source interpolations already resolved into the text,
// matching what the JS template literal produces once at module load) ═════════════════════════
pub const PROMPT_NAME: &str = "tl.refine.microtasks.v13";
pub const PLAN_SYSTEM_MD: &str = "You are TL (Tech Lead) of an autonomous software substrate: the senior engineer who owns scope, decomposition, and patch authorship. DEV is a deterministic executor with zero judgment — every decision must be made by you, grounded in the real source and the real failure evidence, never invented. Speak machine: exact values, exact versions, exact signatures — a vague phrase anywhere downstream is a mechanical failure, not a style issue.";

pub fn prompt_template_md() -> String {
    let self_lint = format!(
        "═══ SELF-LINT — the substrate executes EXACTLY this JavaScript on your answer ═══\nRun it mentally on your DRAFT first, fix every fail(), and only then emit. Never print this analysis — output only the final JSON.\n\nfunction lintSpec(file, spec) {{\n  const vague = spec.match(/{VAGUE_SPEC_SRC}/i);\n  if (vague) fail(file, 'vague phrase \"' + vague[1] + '\" — write the exact value instead');\n  if (/(^|\\/)package\\.json$/i.test(file)) {{\n    if (!/{VERSIONED_DEP_SRC}/.test(spec)) fail(file, 'no versioned dependency (react@^18.2.0 style) listed');\n    if (!/\\bscripts\\b/i.test(spec)) fail(file, 'scripts map not defined');\n  }} else if (/{CODE_FILE_SRC}/i.test(file) && !/{CODE_CONCRETE_SRC}/.test(spec)) {{\n    fail(file, 'no concrete structure named — name the exact exports/imports/functions');\n  }}\n}}\n// RUN: for each microtask m → lintSpec(m.file, m.file_spec_md).\n// Your answer must produce ZERO fail() calls — a failing answer is refused mechanically and costs you a retry.",
    );
    format!("You are TL (Tech Lead) planning a contract refinement. You DECIDE AND PLAN in one pass — a separate per-file authoring pass (one small AI call per file) writes the literal code patches afterward, so do NOT output any code, patches, or anchors here. Every decision about WHAT each file must contain is yours: the per-file author is an obedient executor that sees ONE file at a time and cannot invent requirements you omitted from that file's spec.\n\nYou must output a complete senior-mentor briefing AND an ordered microtask plan in which EVERY microtask targets exactly ONE file and carries an EXHAUSTIVE file_spec_md — the per-file author will see ONLY that spec plus the contract constraints and the file's current source, so anything you omit from the spec will not exist in the file.\n\n═══ THE CARD (operator intent — the source of truth for WHAT this work must achieve) ═══\n{{{{card_md}}}}\n\n═══ CONTRACT BRIEFING ═══\n{{{{briefing_md}}}}\n\n═══ ACCEPTANCE (QA executes these LITERALLY — every decision must serve them) ═══\n{{{{acceptance}}}}\n\n═══ ACCEPTANCE SEMANTICS — what each AC kind demands from YOUR plan ═══\n  file-exists    → a microtask (op_kind=create) for that path, OR a generated_paths entry (with the bootstrap_cmd that generates it). Never leave a net-new path uncovered.\n  grep-contains  → the EXACT BYTES of the pattern are injected into the per-file authoring call for that file as a mandatory must-contain line. Your file_spec_md must describe content the pattern fits into naturally — whitespace and punctuation count; the check is mechanical, not semantic.\n  command / test → your job is to make the command PASS via bootstrap_cmd, build artifacts, and the files you plan. Do NOT plan \"execution microtasks\" — the QA runner executes commands.\n  scaffold tasks → prefer ONE bootstrap_cmd that generates the skeleton (npm create …, etc.) + generated_paths covering its output + small microtasks for customization. Hand-specifying files a generator produces is fragile and wrong.\n\n═══ CONTRACT TO REFINE ═══\n  slug:            {{{{contract_slug}}}}\n  title:           {{{{contract_title}}}}\n  size:            {{{{contract_size}}}}\n  tier:            {{{{contract_tier}}}}\n  complexity:      {{{{contract_complexity}}}}\n  risk:            {{{{contract_risk}}}}\n  files_touched:   {{{{files_touched}}}}\n  must_obey:       {{{{must_obey}}}}\n  out_of_scope:    {{{{out_of_scope}}}}\n  edge_cases:      {{{{edge_cases}}}}\n\n═══ CURRENT SOURCE (context for your file specs) ═══\n{{{{file_bundle}}}}\n\n═══ WHAT TO OUTPUT ═══\n\nA senior-mentor briefing has SEVEN sections plus the microtask plan:\n\n1. briefing_md (rich markdown, 200+ chars)\n   - Mission paragraph (what + why in operator terms)\n   - Background context DEV needs (what's in the codebase that's relevant)\n   - Constraints (must_obey + out_of_scope re-summarized)\n\n2. approach_md (the chosen technical approach — use these EXACT markdown headers, verbatim)\n   ## Chosen approach\n   - WHICH approach you picked + HOW it works in 2-3 sentences\n   ## Why\n   - WHY it's the right call for this codebase\n\n3. alternatives_considered_md\n   - List 1-3 other approaches you considered\n   - For each: why you rejected it (cost, risk, fit)\n   - If only one reasonable approach exists, say so plainly\n\n4. edge_cases_list\n   - Each edge case as a string\n   - Include the ones from the contract.edge_cases input PLUS any you anticipate\n\n5. anti_patterns_list\n   - Things DEV must NOT do\n\n6. hints_for_dev_list\n   - Concrete pointers: relevant files, similar patterns, gotchas\n\n7. microtasks (ordered plan — a NATIVE JSON array of objects, NO code)\n   Each microtask object:\n   - microtask_id: kebab-id, unique within the plan\n   - intent: one imperative sentence\n   - file: EXACTLY ONE repo-relative file path. Split multi-file work into multiple microtasks — one file each.\n   - op_kind: \"create\" (file does not exist yet, or is fully rewritten) or \"modify\" (file exists and is edited)\n   - file_spec_md: EXHAUSTIVE description of what the file must contain after this microtask — every export, every function signature, every constant value, every import, every behavior. Minimum 80 chars. The per-file author sees ONLY this + contract constraints + the file's current source.\n     DECIDE THE EXACT VALUES NOW and write them into the spec: dependency names@versions, config keys with their literal values, export/component names and signatures, script commands. A deterministic spec lint REFUSES vague filler — the words \"appropriate\", \"as needed\", \"if necessary\", \"suitable\", \"standard configuration\" are FORBIDDEN; write the exact value instead.\n   - acceptance: one-sentence verifiable summary for this microtask (non-empty)\n   - depends_on: comma-separated microtask ids this one depends on (\"\" when none)\n\n═══ AUTHORED vs GENERATED (scaffold/dependency files) ═══\nSome files in files_touched are NOT authored by hand — they are PRODUCED by\na tool: node_modules/, package-lock.json, dist/, build output. You CANNOT and\nMUST NOT plan authoring microtasks for these (they are huge + machine-generated).\nDeclare them instead:\n  - bootstrap_cmd: the ONE shell command that produces them, run with cwd at\n    the sandbox root before acceptance. Example: \"cd apps/react-app && npm install\".\n    Leave \"\" if nothing needs generating.\n  - generated_paths: the repo-relative path prefixes bootstrap_cmd produces.\n    Example: [\"apps/react-app/node_modules/\", \"apps/react-app/package-lock.json\"].\n    These are excluded from the authoring-coverage gate and from QA scope/diff.\nEvery other NEW file in files_touched (source you plan — package.json, src/*,\nconfig) MUST be covered by an op_kind=create microtask. A net-new file that is\nneither planned nor under generated_paths is a coverage gap and will be REJECTED.\n\n═══ RULES ═══\n  - Produce between 1 and {{{{max_microtasks}}}} microtasks; ONE file per microtask.\n  - briefing_md MUST be ≥ 200 chars (substrate validator rejects thin briefings).\n  - Each microtask's acceptance MUST be non-empty; file_spec_md MUST be ≥ 80 chars.\n  - Each microtask's file should come from files_touched; add a NEW concrete path only when the contract demands it — never globs or placeholders.\n  - Total plan budget ≤ {{{{total_max_cost_usd}}}} USD.\n\n═══ OUTPUT JSON SHAPE ═══\nReturn JSON only (no markdown fences, no prose). microtasks is a NATIVE array of objects — never a JSON-encoded string.\n\n{{\n  \"briefing_md\": \"## Mission\\n...\\n## Background\\n...\\n## Constraints\\n...\",\n  \"approach_md\": \"## Chosen approach\\n...\\n## Why\\n...\",\n  \"alternatives_considered_md\": \"## Alternative 1\\n...\\n## Alternative 2\\n...\",\n  \"edge_cases_list\": [\"empty array\", \"missing field\", \"...\"],\n  \"anti_patterns_list\": [\"don't catch and swallow\", \"...\"],\n  \"hints_for_dev_list\": [\"See pattern in src/foo/bar.ts line N\", \"...\"],\n  \"rationale\": \"<1-3 sentences on how you decomposed>\",\n  \"bootstrap_cmd\": \"<shell cmd that generates deps/scaffold artifacts, run with cwd=sandbox; \\\"\\\" if none>\",\n  \"generated_paths\": [\"apps/x/node_modules/\", \"apps/x/package-lock.json\"],\n  \"microtasks\": [{{\"microtask_id\": \"kebab-id\", \"intent\": \"imperative single sentence\", \"file\": \"src/x.ts\", \"op_kind\": \"create\", \"file_spec_md\": \"exhaustive description of everything src/x.ts must contain ...\", \"acceptance\": \"verifiable assertion\", \"depends_on\": \"\"}}]\n}}\n\n{self_lint}\n\n═══ THIS ATTEMPT (volatile context — read carefully, it overrides defaults) ═══\n\n═══ RETRY DIRECTIVE ═══\n{{{{retry_directive}}}}\n\n═══ KEPT MICROTASKS (sliced re-author only) ═══\n{{{{kept_microtasks_block}}}}\n\n═══ PRIOR QA FEEDBACK ═══\n{{{{qa_feedback}}}}\n\nIf the block above is a QA failure (not a first attempt): your plan MUST\ntarget the SPECIFIC failing acceptance criteria using their real evidence. Do\nNOT re-emit the prior plan unchanged — it already failed. Do NOT regress\nthe criteria that already passed.")
}

pub const AUTHOR_PROMPT_NAME: &str = "tl.author.file.v2";
pub fn author_template_md() -> String {
    format!("You are TL authoring EXACTLY ONE FILE of an approved plan. A deterministic applier will splice your snippet verbatim — you make every code decision for this file; nothing outside this file is yours to change. You emit CODE ONLY: no commentary, no explanations, no markdown fences.\n\n═══ CONTRACT CONSTRAINTS ═══\n  must_obey:    {{{{must_obey}}}}\n  out_of_scope: {{{{out_of_scope}}}}\n\n═══ TARGET FILE ═══\n  path:      {{{{file}}}}\n  operation: {{{{op_kind}}}}\n\n═══ FILE SPEC — everything this file must contain after your snippet ═══\n{{{{file_spec_md}}}}\n\n═══ MANDATORY BYTE-EXACT LINES (acceptance greps — each must appear in the final file content EXACTLY as written; whitespace and punctuation count) ═══\n{{{{must_contain_lines}}}}\n\n═══ CURRENT SOURCE of {{{{file}}}} ('(new file)' when it does not exist) ═══\n{{{{current_source}}}}\n\n═══ ALL FILES IN THE PLAN (for import coherence — do NOT author them here) ═══\n{{{{plan_files}}}}\n\n═══ PLAN BRIEFING (excerpt) ═══\n{{{{briefing_excerpt}}}}\n\n═══ SNIPPET RULES ═══\nYou emit EXACTLY ONE snippet — one splice the applier performs deterministically:\n  - op=create:          the file is written from scratch. `code` IS the complete file. Anchors stay \"\".\n  - op=insert_after:    `code` is inserted immediately AFTER `anchor_before`. anchor_after stays \"\".\n  - op=replace_section: everything BETWEEN `anchor_before` and `anchor_after` is replaced by `code`.\nANCHOR DISCIPLINE (the applier cannot fix a bad anchor — it will refuse):\n  - Copy anchors VERBATIM from CURRENT SOURCE above — same characters, same indentation. Do NOT paraphrase or re-type from memory.\n  - An anchor must be UNIQUE in the file. If a short line repeats, extend the anchor with the surrounding lines until it is unique.\n  - Never anchor on text that is not shown above.\noperation=create ⇒ op=create with the COMPLETE file in `code`.\n`code` carries ONLY file content — no prose, no fences, no \"here is the file\".\n\n═══ SELF-LINT — the substrate executes EXACTLY this JavaScript on your answer ═══\nRun it mentally on your DRAFT first, fix every fail(), and only then emit.\n\nfunction lintCode(code) {{\n  if (/{CODE_FENCE_SRC}/.test(code)) fail('markdown fence — emit RAW file content, never fenced blocks');\n  if (/{PROSE_PREAMBLE_SRC}/i.test(code)) fail('conversational preamble — code only, no commentary');\n}}\n// RUN: lintCode(code). Also checked mechanically after this: your anchors must\n// resolve byte-exact against CURRENT SOURCE, and every MANDATORY BYTE-EXACT\n// line must appear in the final file content. Zero fail() calls or the answer\n// is refused and costs you a retry.\n\n═══ CORRECTION (volatile — when present, fix EXACTLY this) ═══\n{{{{correction_block}}}}\n\nReturn JSON only (no markdown fences, no prose):\n{{\"op\": \"create\", \"anchor_before\": \"\", \"anchor_after\": \"\", \"code\": \"<the code>\"}}")
}

pub const AUTHOR_PROMPT_NAME_V3: &str = "tl.author.file.v3";
pub fn author_template_v3_md() -> String {
    "You are TL authoring EXACTLY ONE FILE of an approved plan — by COMPLETING A FORM. The form below is pre-filled: the path and the operation are already decided. You fill ONLY the blanks. RETURN ONLY THE COMPLETED FORM — your answer starts at the `# FILE` line and ends after the code block. No JSON, no commentary, nothing outside the form.\n\n═══ CONTRACT CONSTRAINTS ═══\n  must_obey:    {{must_obey}}\n  out_of_scope: {{out_of_scope}}\n\n═══ TARGET FILE ═══\n  path:      {{file}}\n  operation: {{op_kind}}\n\n═══ FILE SPEC — everything this file must contain ═══\n{{file_spec_md}}\n\n═══ MANDATORY BYTE-EXACT LINES (acceptance greps — each must appear in the final file content EXACTLY as written; whitespace and punctuation count) ═══\n{{must_contain_lines}}\n\n═══ CURRENT SOURCE of {{file}} ('(new file)' when it does not exist) ═══\n{{current_source}}\n\n═══ ALL FILES IN THE PLAN (for import coherence — do NOT author them here) ═══\n{{plan_files}}\n\n═══ PLAN BRIEFING (excerpt) ═══\n{{briefing_excerpt}}\n\n═══ FORM RULES ═══\n- ## ANCHOR_BEFORE / ## ANCHOR_AFTER: for create, leave the \"(none …)\" placeholder untouched. Otherwise REPLACE the placeholder line with anchor text copied BYTE-EXACT from CURRENT SOURCE (unique in the file — extend with surrounding lines if a short line repeats).\n- ## CODE: exactly one fenced block containing ONLY the file content / splice code.\n- FENCE ESCALATION (REQUIRED when the file you author itself contains ``` lines — e.g. a README or .md with code blocks): the OUTER ## CODE fence MUST be strictly LONGER than any fence inside the content, or the inner fence closes yours early and everything after is rejected as \"content outside sections\".\n    WRONG  → ```        (content has ```bash … ```  → inner fence closes the outer one)\n    CORRECT → ````       (4 backticks wrap content containing ``` blocks)\n    CORRECT → `````     (5 backticks wrap content containing ```` blocks)\n  Rule: outer fence length = (longest inner fence) + 1, minimum 3.\n- A deterministic COMPILER parses your answer: a missing section, an unclosed fence, content outside sections, or a leftover placeholder is refused mechanically with line numbers and costs you a retry.\n- The compiled code is then anchor-dry-run against the real source and byte-checked against the MANDATORY lines — same as always.\n\n═══ PROHIBITED — DO NOT OUTPUT (the substrate runs all verification itself) ═══\nYour answer ENDS at the closing fence of ## CODE. NEVER emit anything after it.\nIn particular NEVER output these sections — each is \"content outside sections\"\nand is mechanically REFUSED, costing you a retry:\n  ## MANDATORY_LINES_CHECK     ## ANCHOR_MATCH_CHECK     ## FILE_WRITE_CHECK\n  — nor any PENDING / PASS / status / checklist / report line of your own.\nThe mandatory-lines check, anchor dry-run and file write are run BY THE SUBSTRATE.\n\n═══ THE FORM (complete it and return it) ═══\n{{form_skeleton}}\n\n═══ CORRECTION (volatile — when present, fix EXACTLY this) ═══\n{{correction_block}}".to_string()
}

fn prompt_output_fields() -> Value {
    json!([
        { "field_name": "briefing_md", "field_type": "string", "required": true, "min_length": 200, "required_sections": ["## Mission", "## Background", "## Constraints"] },
        { "field_name": "approach_md", "field_type": "string", "required": true, "min_length": 80, "required_sections": ["## Chosen approach", "## Why"] },
        { "field_name": "alternatives_considered_md", "field_type": "string", "required": true, "min_length": 60 },
        { "field_name": "edge_cases_list", "field_type": "array_of_string", "required": false },
        { "field_name": "anti_patterns_list", "field_type": "array_of_string", "required": false },
        { "field_name": "hints_for_dev_list", "field_type": "array_of_string", "required": false },
        { "field_name": "rationale", "field_type": "string", "required": true, "min_length": 20 },
        { "field_name": "bootstrap_cmd", "field_type": "string", "required": false },
        { "field_name": "generated_paths", "field_type": "array_of_string", "required": false },
        { "field_name": "microtasks", "field_type": "array_of_object", "required": false,
          "description": "each: {microtask_id, intent, file (ONE path), op_kind: create|modify, file_spec_md (≥80 chars), acceptance, depends_on (comma ids or \"\")}" },
    ])
}

fn prompt_cross_field_rules() -> Value {
    json!([
        { "id": "empty_microtasks_requires_refusal_rationale",
          "expression": "(() => { const a = Array.isArray(output.microtasks) ? output.microtasks : []; if (a.length > 0) return true; return /refus|cannot|empty|no microtask|impossible/i.test(output.rationale || \"\"); })()",
          "message": "when microtasks is empty, rationale must explain the refusal (mention refus/cannot/empty/impossible)" },
        { "id": "every_microtask_one_file",
          "expression": "(Array.isArray(output.microtasks) ? output.microtasks : []).every(m => m && typeof m.file === \"string\" && m.file.trim().length > 0 && !m.file.includes(\",\") && !/\\s/.test(m.file.trim()))",
          "message": "every microtask must target exactly ONE file (a single path — no commas, no whitespace; split multi-file work into multiple microtasks)" },
        { "id": "every_microtask_has_file_spec",
          "expression": "(Array.isArray(output.microtasks) ? output.microtasks : []).every(m => m && typeof m.file_spec_md === \"string\" && m.file_spec_md.length >= 80)",
          "message": "every microtask must carry file_spec_md ≥ 80 chars (the per-file author sees ONLY this + contract context)" },
        { "id": "every_microtask_has_acceptance",
          "expression": "(Array.isArray(output.microtasks) ? output.microtasks : []).every(m => m && ((typeof m.acceptance === \"string\" && m.acceptance.trim().length > 0) || (Array.isArray(m.acceptance) && m.acceptance.length > 0)))",
          "message": "every microtask must have a non-empty acceptance summary" },
        { "id": "every_microtask_valid_op_kind",
          "expression": "(Array.isArray(output.microtasks) ? output.microtasks : []).every(m => m && (m.op_kind === \"create\" || m.op_kind === \"modify\"))",
          "message": "every microtask.op_kind must be \"create\" or \"modify\"" },
    ])
}

fn author_output_fields() -> Value {
    json!([
        { "field_name": "op", "field_type": "enum", "required": true, "enum": ["create", "insert_after", "replace_section"] },
        { "field_name": "anchor_before", "field_type": "string", "required": false, "description": "verbatim text from CURRENT SOURCE; \"\" for create" },
        { "field_name": "anchor_after", "field_type": "string", "required": false, "description": "verbatim text from CURRENT SOURCE; \"\" unless replace_section" },
        { "field_name": "code", "field_type": "string", "required": true, "min_length": 1, "description": "the snippet — file content only, no prose" },
    ])
}

fn author_cross_field_rules() -> Value {
    json!([
        { "id": "anchors_match_op",
          "expression": "(output.op !== \"insert_after\" || (typeof output.anchor_before === \"string\" && output.anchor_before.length > 0)) && (output.op !== \"replace_section\" || (typeof output.anchor_before === \"string\" && output.anchor_before.length > 0 && typeof output.anchor_after === \"string\" && output.anchor_after.length > 0))",
          "message": "insert_after requires anchor_before; replace_section requires both anchors (copied verbatim from CURRENT SOURCE)" },
    ])
}

/// `AUTHOR_FORM_ENABLED` — JS `() => process.env.ANTILOKI_AUTHOR_FORM !== 'off'`. Default ON.
pub fn AUTHOR_FORM_ENABLED() -> bool {
    std::env::var("ANTILOKI_AUTHOR_FORM").map(|v| v != "off").unwrap_or(true)
}

// try promptFind; on template drift (or missing) promptAdd then promptFind again — mirrors JS
// ensurePromptCurrent (find-or-add + re-register when template_md differs).
fn ensure_prompt_current(db: &Connection, opts: &Value) -> Result<Value, String> {
    let name = opts.get("name").and_then(Value::as_str).unwrap_or("");
    let template_md = opts.get("template_md").and_then(Value::as_str).unwrap_or("");
    if let Ok(existing) = crate::ai_prompt_find::promptFind(&json!({ "name": name }), db) {
        if existing.get("template_md").and_then(Value::as_str) == Some(template_md) {
            return Ok(existing);
        }
    }
    crate::ai_prompt_add::promptAdd(opts, db)?;
    crate::ai_prompt_find::promptFind(&json!({ "name": name }), db)
}

/// `export function ensurePlanPrompts(db)` — registers/re-registers the 3 prompt templates.
pub fn ensurePlanPrompts(db: &Connection) -> Result<Value, String> {
    ensure_prompt_current(db, &json!({
        "name": AUTHOR_PROMPT_NAME, "template_md": author_template_md(),
        "default_tier": "low", "default_temperature": "freezed",
        "phase_flow": "single_only",
        "max_output_tokens": 3000, "output_fields": author_output_fields(),
        "cross_field_rules": author_cross_field_rules(),
    }))?;
    ensure_prompt_current(db, &json!({
        "name": AUTHOR_PROMPT_NAME_V3, "template_md": author_template_v3_md(),
        "default_tier": "low", "default_temperature": "freezed",
        "phase_flow": "single_only",
        "max_output_tokens": 3000, "output_fields": author_output_fields(),
        "cross_field_rules": author_cross_field_rules(),
    }))?;
    ensure_prompt_current(db, &json!({
        "name": PROMPT_NAME, "template_md": prompt_template_md(),
        "default_tier": "low", "default_temperature": "freezed",
        "phase_flow": "single_only", "system_md": PLAN_SYSTEM_MD,
        "max_output_tokens": 8000, "output_fields": prompt_output_fields(),
        "cross_field_rules": prompt_cross_field_rules(),
    }))
}

// ══ v11 plan read-back ══════════════════════════════════════════════════════════════════════

/// `export function toList(v)`. Children of an array_of_object row persist as strings; arrays the
/// AI emitted anyway arrive JSON-stringified. Heal both shapes.
pub fn toList(v: &Value) -> Value {
    if v.is_null() {
        return json!([]);
    }
    if let Some(a) = v.as_array() {
        return Value::Array(a.clone());
    }
    let s = js_string(v);
    let s = s.trim();
    if s.is_empty() {
        return json!([]);
    }
    if s.starts_with('[') {
        if let Ok(Value::Array(a)) = serde_json::from_str::<Value>(s) {
            return Value::Array(a);
        }
    }
    json!(s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect::<Vec<_>>())
}

fn out_row_cell(children: &[&Value], k: &str) -> Value {
    for c in children {
        if c.get("field_name").and_then(Value::as_str) == Some(k) {
            for key in ["value_text", "value_number", "value_boolean"] {
                if let Some(v) = c.get(key) {
                    if !v.is_null() {
                        return v.clone();
                    }
                }
            }
            return Value::Null;
        }
    }
    Value::Null
}

/// `export function readMicrotaskDefs(outputKv)`.
pub fn readMicrotaskDefs(output_kv: &Value) -> Value {
    let empty: Vec<Value> = Vec::new();
    let rows = output_kv.as_array().unwrap_or(&empty);
    let mut parents: Vec<&Value> = rows.iter().filter(|o| {
        o.get("field_name").and_then(Value::as_str) == Some("microtasks")
            && o.get("parent_output_id").map_or(true, Value::is_null)
            && o.get("value_text").map_or(true, Value::is_null)
            && o.get("value_number").map_or(true, Value::is_null)
            && o.get("value_boolean").map_or(true, Value::is_null)
    }).collect();
    parents.sort_by_key(|o| o.get("order_index").and_then(Value::as_i64).unwrap_or(0));

    let mut out = Vec::new();
    for parent in parents {
        let parent_id = parent.get("id").cloned().unwrap_or(Value::Null);
        let children: Vec<&Value> = rows.iter().filter(|x| x.get("parent_output_id") == Some(&parent_id) && !parent_id.is_null()).collect();
        let file = out_row_cell(&children, "file");
        let file = if let Some(s) = file.as_str() { s.trim().to_string() } else { String::new() };
        let acceptance_raw = out_row_cell(&children, "acceptance");
        let acceptance = if acceptance_raw.is_array()
            || (acceptance_raw.as_str().map_or(false, |s| s.trim().starts_with('[')))
        {
            toList(&acceptance_raw)
        } else if !acceptance_raw.is_null() && !js_string(&acceptance_raw).trim().is_empty() {
            json!([js_string(&acceptance_raw)])
        } else {
            json!([])
        };
        let op_kind_raw = out_row_cell(&children, "op_kind");
        let op_kind = if op_kind_raw.as_str() == Some("modify") { "modify" } else { "create" };
        out.push(json!({
            "microtask_id": nz_str(out_row_cell(&children, "microtask_id")),
            "intent": nz_str(out_row_cell(&children, "intent")),
            "file": file,
            "file_spec_md": nz_str(out_row_cell(&children, "file_spec_md")),
            "op_kind": op_kind,
            "acceptance": acceptance,
            "depends_on": toList(&out_row_cell(&children, "depends_on")),
        }));
    }
    Value::Array(out)
}

// `cell(k) ?? ''` — JS defaults a missing cell to the empty string for microtask_id/intent/file_spec_md.
fn nz_str(v: Value) -> Value {
    if v.is_null() {
        json!("")
    } else {
        v
    }
}

/// `export function buildPlanFromOutput(outputKv, contract, totalMaxCostUsd)`.
pub fn buildPlanFromOutput(output_kv: &Value, contract: &Value, total_max_cost_usd: f64) -> Value {
    let mut rationale = String::new();
    if let Some(rows) = output_kv.as_array() {
        for row in rows {
            if row.get("field_name").and_then(Value::as_str) == Some("rationale")
                && row.get("value_text").map_or(false, |v| !v.is_null())
                && row.get("parent_output_id").map_or(true, Value::is_null)
            {
                rationale = row.get("value_text").and_then(Value::as_str).unwrap_or("").to_string();
            }
        }
    }
    let defs = readMicrotaskDefs(output_kv);
    let defs_arr = defs.as_array().cloned().unwrap_or_default();
    let card_slug = contract.get("card_slug").filter(|v| !v.is_null()).cloned()
        .unwrap_or_else(|| contract.get("slug").cloned().unwrap_or(Value::Null));
    let slug = contract.get("slug").and_then(Value::as_str).unwrap_or("");

    if defs_arr.is_empty() {
        return json!({
            "plan_id": format!("plan-{slug}-rejected"),
            "card_slug": card_slug,
            "rationale": rationale,
            "total_max_cost_usd": total_max_cost_usd,
            "microtasks": [],
            "empty_refusal": true,
        });
    }

    // FLOOR (not round): guarantees perTaskCost * count <= totalMaxCostUsd (see JS doc comment).
    let per_task_cost = (0.05f64.min(total_max_cost_usd / defs_arr.len() as f64) * 1e4).floor() / 1e4;
    let microtasks: Vec<Value> = defs_arr.iter().map(|d| {
        let op_kind = d.get("op_kind").and_then(Value::as_str).unwrap_or("create");
        let file = d.get("file").cloned().unwrap_or(json!(""));
        json!({
            "microtask_id": d.get("microtask_id").cloned().unwrap_or(json!("")),
            "intent": d.get("intent").cloned().unwrap_or(json!("")),
            "file": file,
            "file_spec_md": d.get("file_spec_md").cloned().unwrap_or(json!("")),
            "op_kind": op_kind,
            "files_touched": json!([file]),
            "acceptance": d.get("acceptance").cloned().unwrap_or(json!([])),
            "depends_on": d.get("depends_on").cloned().unwrap_or(json!([])),
            "max_tokens": 1024,
            "max_cost_usd": per_task_cost,
            "expected_diff_shape": if op_kind == "create" { "new_module" } else { "single_file" },
        })
    }).collect();

    json!({
        "plan_id": format!("plan-{slug}-{}", crate::clock::now_ms()),
        "card_slug": card_slug,
        "rationale": rationale,
        "total_max_cost_usd": total_max_cost_usd,
        "microtasks": microtasks,
    })
}

// ══ #380 sanity check + scope expansion ═════════════════════════════════════════════════════
fn placeholder_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^<[A-Z_]+>$").unwrap())
}
fn glob_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[*?\[\]]").unwrap())
}
fn shell_metachars_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[`$;&|<>]").unwrap())
}
const PATH_CAP: usize = 20;

/// `export function sanityCheckPath(p)`.
pub fn sanityCheckPath(p: &Value) -> Value {
    let s = match p.as_str() {
        Some(s) => s,
        None => return json!("not a string"),
    };
    if s.is_empty() {
        return json!("empty");
    }
    if s.trim() != s {
        return json!("surrounding whitespace");
    }
    if placeholder_re().is_match(s) {
        return json!("placeholder pattern");
    }
    if glob_re().is_match(s) {
        return json!("glob metachars");
    }
    if shell_metachars_re().is_match(s) {
        return json!("shell metachars");
    }
    Value::Null
}

/// `export function expandScopeOrReject(plan, contract, skill = 'tl-refine-microtasks')`.
pub fn expandScopeOrReject(plan: &Value, contract: &Value, skill: &str) -> Result<Value, String> {
    let allowed: Vec<Value> = contract.get("files_touched").and_then(Value::as_array).cloned().unwrap_or_default();
    let allowed_set: std::collections::HashSet<String> = allowed.iter().filter_map(|v| v.as_str().map(String::from)).collect();
    let mut invalid_paths: Vec<Value> = Vec::new();
    let mut new_paths_added: Vec<String> = Vec::new();
    let mut new_paths_set: std::collections::HashSet<String> = std::collections::HashSet::new();

    let empty: Vec<Value> = Vec::new();
    for m in plan.get("microtasks").and_then(Value::as_array).unwrap_or(&empty) {
        for f in m.get("files_touched").and_then(Value::as_array).unwrap_or(&empty) {
            let fs = match f.as_str() {
                Some(s) => s,
                None => continue,
            };
            if allowed_set.contains(fs) {
                continue;
            }
            let violation = sanityCheckPath(f);
            if !violation.is_null() {
                invalid_paths.push(json!({
                    "microtask_id": m.get("microtask_id").cloned().unwrap_or(Value::Null),
                    "file": fs,
                    "violation": violation,
                }));
                continue;
            }
            if !new_paths_set.contains(fs) {
                new_paths_set.insert(fs.to_string());
                new_paths_added.push(fs.to_string());
            }
        }
    }

    if !invalid_paths.is_empty() {
        let joined = invalid_paths.iter().map(|o| {
            format!("{}→{} ({})", js_string(&o["microtask_id"]), js_string(&o["file"]), js_string(&o["violation"]))
        }).collect::<Vec<_>>().join(", ");
        return Err(format!("{skill}: microtask files contain invalid paths (sanity-failed): {joined}"));
    }

    let cleaned_existing: Vec<String> = allowed.iter()
        .filter_map(|v| v.as_str())
        .filter(|s| !placeholder_re().is_match(s))
        .map(String::from)
        .collect();
    let mut union: Vec<String> = cleaned_existing.clone();
    for p in &new_paths_added {
        if !union.contains(p) {
            union.push(p.clone());
        }
    }
    if union.len() > PATH_CAP {
        return Err(format!(
            "{skill}: union of contract.files_touched + microtask paths exceeds cap ({} > {PATH_CAP}). Card is too big for one contract — decompose via pl.session.start.",
            union.len()
        ));
    }
    let expanded = !new_paths_added.is_empty() || cleaned_existing.len() != allowed.len();
    Ok(json!({
        "expanded": expanded,
        "files_touched": union,
        "newPathsAdded": new_paths_added,
    }))
}

/// `export function coverageGaps(filesTouched, microtasks, generatedPaths, existsFn)`.
/// `existsFn` is a Rust closure standing in for the JS injectable fn.
pub fn coverageGaps(files_touched: &Value, microtasks: &Value, generated_paths: &Value, exists_fn: impl Fn(&str) -> bool) -> Value {
    let empty: Vec<Value> = Vec::new();
    let mut patched: std::collections::HashSet<String> = std::collections::HashSet::new();
    for m in microtasks.as_array().unwrap_or(&empty) {
        for p in m.get("patches").and_then(Value::as_array).unwrap_or(&empty) {
            if let Some(f) = p.get("file").and_then(Value::as_str) {
                patched.insert(f.to_string());
            }
        }
    }
    let generated: Vec<&str> = generated_paths.as_array().map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
    let is_generated = |f: &str| generated.iter().any(|g| {
        f == *g || f.starts_with(&if g.ends_with('/') { g.to_string() } else { format!("{g}/") })
    });
    let mut gaps = Vec::new();
    for f in files_touched.as_array().unwrap_or(&empty) {
        let fs = match f.as_str() {
            Some(s) => s,
            None => continue,
        };
        if patched.contains(fs) {
            continue;
        }
        if is_generated(fs) {
            continue;
        }
        if exists_fn(fs) {
            continue;
        }
        gaps.push(json!(fs));
    }
    Value::Array(gaps)
}

// ══ FEAT-SLICED-RETRIES pure helpers ════════════════════════════════════════════════════════
fn pathish_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[\w@./-]*[\w-]+\.(?:mjs|cjs|js|ts|tsx|jsx|json|md|css|html|sql|py|yml|yaml|toml)\b").unwrap())
}

/// `export function extractPathTokens(acItem, evidenceMd)`.
pub fn extractPathTokens(ac_item: &Value, evidence_md: &Value) -> Value {
    let mut out: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut push = |s: String, out: &mut Vec<String>, seen: &mut std::collections::HashSet<String>| {
        if seen.insert(s.clone()) {
            out.push(s);
        }
    };
    if ac_item.is_object() {
        if let Some(p) = ac_item.get("path").and_then(Value::as_str) {
            push(p.to_string(), &mut out, &mut seen);
        }
        for p in ac_item.get("paths").and_then(Value::as_array).unwrap_or(&Vec::new()) {
            if let Some(s) = p.as_str() {
                push(s.to_string(), &mut out, &mut seen);
            }
        }
        for field in [ac_item.get("cmd"), ac_item.get("query")] {
            if let Some(s) = field.and_then(Value::as_str) {
                for m in pathish_re().find_iter(s) {
                    push(m.as_str().to_string(), &mut out, &mut seen);
                }
            }
        }
    }
    if let Some(s) = evidence_md.as_str() {
        for m in pathish_re().find_iter(s) {
            push(m.as_str().to_string(), &mut out, &mut seen);
        }
    }
    json!(out)
}

/// `export function implicateMicrotasks(failingAcs, acceptanceJson, microtasks)`.
pub fn implicateMicrotasks(failing_acs: &Value, acceptance_json: &Value, microtasks: &Value) -> Value {
    let empty: Vec<Value> = Vec::new();
    let failing = failing_acs.as_array().unwrap_or(&empty);
    let acceptance = acceptance_json.as_array().cloned().unwrap_or_default();

    for f in failing {
        let idx = f.get("ac_index").and_then(Value::as_i64);
        match idx {
            None => {}
            Some(i) if i < 0 || (i as usize) >= acceptance.len() => {
                return json!({ "ok": false, "reason": "ac-index-out-of-range", "implicated_ids": [], "unmapped_ac_indices": [] });
            }
            _ => {}
        }
        if f.get("ac_index").map_or(true, Value::is_null) {
            return json!({ "ok": false, "reason": "ac-index-out-of-range", "implicated_ids": [], "unmapped_ac_indices": [] });
        }
    }

    struct PerAc { ac_index: Value, paths: Vec<String>, mapped: bool }
    let mut per_ac: Vec<PerAc> = failing.iter().map(|f| {
        let idx = f.get("ac_index").and_then(Value::as_i64).unwrap_or(0) as usize;
        let item = acceptance.get(idx).cloned().unwrap_or(Value::Null);
        let evidence_md = f.get("evidence_md").cloned().unwrap_or(Value::Null);
        let paths_v = extractPathTokens(&item, &evidence_md);
        let paths: Vec<String> = paths_v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
        PerAc { ac_index: f.get("ac_index").cloned().unwrap_or(Value::Null), paths, mapped: false }
    }).collect();

    let mut implicated: Vec<String> = Vec::new();
    let mut implicated_set: std::collections::HashSet<String> = std::collections::HashSet::new();
    let empty_mt: Vec<Value> = Vec::new();
    for mt in microtasks.as_array().unwrap_or(&empty_mt) {
        let mut mt_files: std::collections::HashSet<String> = std::collections::HashSet::new();
        for f in mt.get("files_touched").and_then(Value::as_array).unwrap_or(&empty) {
            if let Some(s) = f.as_str() { mt_files.insert(s.to_string()); }
        }
        for p in mt.get("patches").and_then(Value::as_array).unwrap_or(&empty) {
            if let Some(s) = p.get("file").and_then(Value::as_str) { mt_files.insert(s.to_string()); }
        }
        let mid = mt.get("microtask_id").and_then(Value::as_str).unwrap_or("").to_string();
        for entry in per_ac.iter_mut() {
            if entry.paths.iter().any(|p| mt_files.contains(p)) {
                if implicated_set.insert(mid.clone()) {
                    implicated.push(mid.clone());
                }
                entry.mapped = true;
            }
        }
    }
    let unmapped: Vec<Value> = per_ac.iter().filter(|e| !e.mapped).map(|e| e.ac_index.clone()).collect();
    let ok = unmapped.is_empty() && !implicated.is_empty();
    let reason = if !unmapped.is_empty() {
        json!("unmapped-failing-ac")
    } else if implicated.is_empty() {
        json!("nothing-implicated")
    } else {
        Value::Null
    };
    json!({ "ok": ok, "reason": reason, "implicated_ids": implicated, "unmapped_ac_indices": unmapped })
}

/// `export function decideRetryMode({ buildGateRed, qaRun, failingAcs, totalAcs, implication, planSize, refineHistory })`.
pub fn decideRetryMode(opts: &Value) -> Value {
    let build_gate_red = truthy(opts.get("buildGateRed"));
    if build_gate_red {
        return json!({ "mode": "full", "why": "build-gate-red" });
    }
    let qa_run = opts.get("qaRun").cloned().unwrap_or(Value::Null);
    let failing_acs = opts.get("failingAcs").and_then(Value::as_array).cloned().unwrap_or_default();
    if qa_run.is_null() || failing_acs.is_empty() {
        return json!({ "mode": "full", "why": "no-per-ac-signal" });
    }
    let total_acs = opts.get("totalAcs").and_then(Value::as_i64).unwrap_or(0);
    let passing = total_acs - failing_acs.len() as i64;
    if passing < (total_acs as f64 / 2.0).ceil() as i64 {
        return json!({ "mode": "full", "why": "most-acs-failing" });
    }
    let implication = opts.get("implication").cloned().unwrap_or(Value::Null);
    let implication_ok = implication.get("ok").map_or(false, |v| truthy(Some(v)));
    if !implication_ok {
        let reason = implication.get("reason").and_then(Value::as_str).unwrap_or("unmappable").to_string();
        return json!({ "mode": "full", "why": reason });
    }
    let implicated_ids: Vec<Value> = implication.get("implicated_ids").and_then(Value::as_array).cloned().unwrap_or_default();
    let plan_size = opts.get("planSize").and_then(Value::as_i64).unwrap_or(0);
    if implicated_ids.len() as i64 > (plan_size / 2) {
        return json!({ "mode": "full", "why": "majority-implicated" });
    }
    let refine_history = opts.get("refineHistory").and_then(Value::as_array).cloned().unwrap_or_default();
    let real: Vec<&Value> = refine_history.iter().filter(|e| e.get("kind").and_then(Value::as_str) != Some("partial_authoring")).collect();
    let prev = real.last();
    let now_set: std::collections::HashSet<Value> = failing_acs.iter().filter_map(|f| f.get("ac_index").cloned()).collect();
    if let Some(prev) = prev {
        if prev.get("mode").and_then(Value::as_str) == Some("sliced") {
            if let Some(prev_failing) = prev.get("failing_ac_indices").and_then(Value::as_array) {
                if now_set.iter().all(|i| prev_failing.contains(i)) {
                    return json!({ "mode": "full", "why": "sliced-already-failed-same-acs" });
                }
            }
        }
    }
    json!({ "mode": "sliced", "why": "minority-implicated", "replaced_ids": implicated_ids })
}

/// `export function mergeSlicedPlan(priorMicrotasks, keptIds, replacements, origin)`.
pub fn mergeSlicedPlan(prior_microtasks: &Value, kept_ids: &Value, replacements: &Value, origin: &Value) -> Value {
    let empty: Vec<Value> = Vec::new();
    let kept_set: std::collections::HashSet<String> = kept_ids.as_array().unwrap_or(&empty)
        .iter().filter_map(|v| v.as_str().map(String::from)).collect();
    let prior = prior_microtasks.as_array().unwrap_or(&empty);
    let kept: Vec<Value> = prior.iter()
        .filter(|m| m.get("microtask_id").and_then(Value::as_str).map_or(false, |s| kept_set.contains(s)))
        .cloned().collect();

    let mut kept_only_files: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut replaced_files: std::collections::HashSet<String> = std::collections::HashSet::new();
    for m in prior {
        let mid = m.get("microtask_id").and_then(Value::as_str).unwrap_or("");
        let mut files: Vec<String> = m.get("files_touched").and_then(Value::as_array).unwrap_or(&empty)
            .iter().filter_map(|v| v.as_str().map(String::from)).collect();
        files.extend(m.get("patches").and_then(Value::as_array).unwrap_or(&empty)
            .iter().filter_map(|p| p.get("file").and_then(Value::as_str).map(String::from)));
        let target = if kept_set.contains(mid) { &mut kept_only_files } else { &mut replaced_files };
        for f in files {
            target.insert(f);
        }
    }
    for f in &replaced_files {
        kept_only_files.remove(f);
    }

    let replacements_arr = replacements.as_array().cloned().unwrap_or_default();
    for r in &replacements_arr {
        let rid = r.get("microtask_id").and_then(Value::as_str).unwrap_or("");
        if kept_set.contains(rid) {
            return json!({ "ok": false, "reason": format!("replacement re-emits kept microtask '{rid}'") });
        }
        for p in r.get("patches").and_then(Value::as_array).unwrap_or(&empty) {
            if let Some(f) = p.get("file").and_then(Value::as_str) {
                if kept_only_files.contains(f) {
                    return json!({ "ok": false, "reason": format!("replacement '{rid}' patches '{f}' which only kept microtasks touch") });
                }
            }
        }
    }

    let stamped: Vec<Value> = replacements_arr.into_iter().map(|r| {
        let mut o = r.as_object().cloned().unwrap_or_default();
        o.insert("origin".to_string(), origin.clone());
        Value::Object(o)
    }).collect();
    let mut merged = kept.clone();
    merged.extend(stamped);
    json!({ "ok": true, "merged": merged, "kept_count": kept.len() })
}

/// `export function priorPatchesBlock(contract)`.
pub fn priorPatchesBlock(contract: &Value) -> String {
    let raw = contract.get("microtasks_json").and_then(Value::as_str).unwrap_or("[]");
    let mts: Value = serde_json::from_str(raw).unwrap_or(json!([]));
    let mts_arr = match mts.as_array() {
        Some(a) => a,
        None => return "(none recorded)".to_string(),
    };
    let mut lines = Vec::new();
    for mt in mts_arr {
        for p in mt.get("patches").and_then(Value::as_array).unwrap_or(&Vec::new()) {
            let file = p.get("file").and_then(Value::as_str).unwrap_or("");
            let op = p.get("op").and_then(Value::as_str).unwrap_or("");
            let anchor_before = p.get("anchor_before").and_then(Value::as_str);
            let suffix = anchor_before.map(|a| format!(" after \"{}\"", a.chars().take(60).collect::<String>())).unwrap_or_default();
            lines.push(format!("• {file} [{op}]{suffix}"));
        }
    }
    if lines.is_empty() {
        "(none recorded)".to_string()
    } else {
        lines.join("\n")
    }
}

// ══ failure-context assembly (db reads) ═════════════════════════════════════════════════════

/// `export function buildGateFailureBlock(db, contract)`.
pub fn buildGateFailureBlock(db: &Connection, contract: &Value) -> Value {
    let slug = contract.get("slug").and_then(Value::as_str).unwrap_or("");
    let rows = match crate::db_json::query_rows(
        db,
        "SELECT id, build_cmd, evidence_md FROM build_gate_runs WHERE contract_slug = ? AND passed = 0 AND COALESCE(skipped, 0) = 0 ORDER BY id DESC LIMIT 1",
        &[json!(slug)],
    ) {
        Ok(v) => v,
        Err(_) => return Value::Null, // table missing on a partial DB
    };
    let row = match rows.as_array().and_then(|a| a.first()) {
        Some(r) => r,
        None => return Value::Null,
    };
    let build_cmd = row.get("build_cmd").and_then(Value::as_str).filter(|s| !s.is_empty()).unwrap_or("(derived default)");
    let evidence = row.get("evidence_md").and_then(Value::as_str).filter(|s| !s.is_empty()).unwrap_or("(no evidence captured)");
    json!([
        "The previous patches were APPLIED to the sandbox but FAILED to compile (deterministic build gate RED).",
        "Fix the SPECIFIC compiler errors below. Do NOT re-emit the prior patches verbatim — they produced exactly this error.",
        "",
        "═ BUILD COMMAND ═",
        build_cmd,
        "",
        "═ COMPILER / BUILD OUTPUT (real, un-truncated) ═",
        evidence,
        "",
        "═ PRIOR PATCHES THAT FAILED TO COMPILE (do not repeat verbatim) ═",
        &priorPatchesBlock(contract),
    ].iter().map(|s| s.to_string()).collect::<Vec<_>>().join("\n"))
}

/// `export function qaReviewFailureBlock(db, contract)`.
pub fn qaReviewFailureBlock(db: &Connection, contract: &Value) -> Value {
    let slug = contract.get("slug").and_then(Value::as_str).unwrap_or("");
    let rows = match crate::db_json::query_rows(
        db,
        "SELECT id, ts, findings_md, issues_json FROM contract_qa_reviews WHERE contract_slug = ? AND passed = 0 AND id > COALESCE((SELECT MAX(id) FROM contract_qa_reviews WHERE contract_slug = ? AND passed = 1), 0) ORDER BY id DESC LIMIT 1",
        &[json!(slug), json!(slug)],
    ) {
        Ok(v) => v,
        Err(_) => return Value::Null,
    };
    let row = match rows.as_array().and_then(|a| a.first()) {
        Some(r) => r,
        None => return Value::Null,
    };
    let mut issue_lines = "(no structured issues recorded)".to_string();
    if let Some(issues_raw) = row.get("issues_json").and_then(Value::as_str) {
        if let Ok(Value::Array(issues)) = serde_json::from_str::<Value>(issues_raw) {
            if !issues.is_empty() {
                issue_lines = issues.iter().map(|i| {
                    let sev = i.get("severity").and_then(Value::as_str).unwrap_or("issue");
                    let mid = i.get("microtask_id").and_then(Value::as_str).map(|s| format!(" {s}")).unwrap_or_default();
                    let file = i.get("file").and_then(Value::as_str).map(|s| format!(" ({s})")).unwrap_or_default();
                    let problem = i.get("problem").and_then(Value::as_str).unwrap_or("");
                    let fix = i.get("fix_hint").and_then(Value::as_str).unwrap_or("(no hint)");
                    format!("- [{sev}]{mid}{file} {problem}\n  FIX: {fix}")
                }).collect::<Vec<_>>().join("\n");
            }
        }
    }
    let findings = row.get("findings_md").and_then(Value::as_str).filter(|s| !s.is_empty()).unwrap_or("(no narrative)");
    json!([
        "QA reviewed your PREVIOUS plan (the thinking gate, before DEV) and FAILED it.".to_string(),
        "Address EVERY issue below — each carries QA's exact fix. Do NOT re-emit the prior plan unchanged.".to_string(),
        "".to_string(),
        "═ QA REVIEW FINDINGS ═".to_string(),
        findings.to_string(),
        "".to_string(),
        "═ ISSUES + QA'S FIX HINTS (apply them literally) ═".to_string(),
        issue_lines,
    ].join("\n"))
}

/// `export function gatherFailureSignal(db, contract)` — returned as `{buildGateBlock, reviewBlock,
/// run, failing}`; `failing` is a native Vec of rows for `buildQaFailureContext`'s use.
pub fn gatherFailureSignal(db: &Connection, contract: &Value) -> Value {
    let build_gate_block = buildGateFailureBlock(db, contract);
    let review_block = qaReviewFailureBlock(db, contract);
    let slug = contract.get("slug").and_then(Value::as_str).unwrap_or("");
    let run_rows = crate::db_json::query_rows(
        db,
        "SELECT id, feedback_md, acceptance_fail, acceptance_pass FROM qa_runs WHERE contract_slug = ? AND all_passed = 0 ORDER BY id DESC LIMIT 1",
        &[json!(slug)],
    ).unwrap_or(json!([]));
    let run = run_rows.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null);
    let failing = if !run.is_null() {
        let run_id = run.get("id").cloned().unwrap_or(Value::Null);
        crate::db_json::query_rows(
            db,
            "SELECT ac_index, kind, evidence_md FROM qa_run_results WHERE qa_run_id = ? AND passed = 0 ORDER BY ac_index",
            &[run_id],
        ).unwrap_or(json!([]))
    } else {
        json!([])
    };
    json!({ "buildGateBlock": build_gate_block, "reviewBlock": review_block, "run": run, "failing": failing })
}

/// `export function buildQaFailureContext(db, contract, signal)` — `signal` optional; when `Null`,
/// computed via `gatherFailureSignal`.
pub fn buildQaFailureContext(db: &Connection, contract: &Value, signal: &Value) -> Value {
    let owned;
    let signal = if signal.is_null() {
        owned = gatherFailureSignal(db, contract);
        &owned
    } else {
        signal
    };
    let build_gate_block = signal.get("buildGateBlock").cloned().unwrap_or(Value::Null);
    let review_block = signal.get("reviewBlock").cloned().unwrap_or(Value::Null);
    let run = signal.get("run").cloned().unwrap_or(Value::Null);
    let failing = signal.get("failing").and_then(Value::as_array).cloned().unwrap_or_default();

    if run.is_null() {
        let parts: Vec<String> = [&build_gate_block, &review_block].iter()
            .filter_map(|b| b.as_str().map(String::from)).collect();
        return if parts.is_empty() { Value::Null } else { json!(parts.join("\n\n")) };
    }

    let ac_block = if !failing.is_empty() {
        failing.iter().map(|r| {
            let idx = r.get("ac_index").cloned().unwrap_or(Value::Null);
            let kind = r.get("kind").cloned().unwrap_or(Value::Null);
            let ev = r.get("evidence_md").and_then(Value::as_str).filter(|s| !s.is_empty()).unwrap_or("(no evidence captured)");
            format!("── AC#{} ({}) FAILED ──\n{ev}", js_string(&idx), js_string(&kind))
        }).collect::<Vec<_>>().join("\n\n")
    } else {
        "(no per-AC results recorded)".to_string()
    };

    let acceptance_fail = run.get("acceptance_fail").cloned().unwrap_or(json!("?"));
    let feedback = run.get("feedback_md").and_then(Value::as_str).filter(|s| !s.is_empty()).unwrap_or("(no narrative)");
    let qa_block = [
        format!("A previous attempt FAILED QA ({} acceptance criteria still failing).", js_string(&acceptance_fail)),
        "Fix the SPECIFIC failing criteria below using their real evidence. Do NOT re-emit the prior patches verbatim. Do NOT regress the criteria that already passed.".to_string(),
        "".to_string(),
        "═ FAILING ACCEPTANCE CRITERIA (real build/test evidence) ═".to_string(),
        ac_block,
        "".to_string(),
        "═ QA NARRATIVE ═".to_string(),
        feedback.to_string(),
        "".to_string(),
        "═ PRIOR PATCHES THAT WERE APPLIED AND STILL FAILED (do not repeat verbatim) ═".to_string(),
        priorPatchesBlock(contract),
    ].join("\n");

    let parts: Vec<String> = [&build_gate_block, &review_block, &json!(qa_block)].iter()
        .filter_map(|b| b.as_str().map(String::from)).collect();
    if parts.is_empty() { Value::Null } else { json!(parts.join("\n\n")) }
}

// ══ grep-AC echo gate + anchor dry-run ══════════════════════════════════════════════════════

/// `export function grepAcEchoGaps(acceptanceParsed, microtasks)`.
pub fn grepAcEchoGaps(acceptance_parsed: &Value, microtasks: &Value) -> Value {
    let empty: Vec<Value> = Vec::new();
    let mut by_file: Vec<(String, Vec<Value>)> = Vec::new();
    for mt in microtasks.as_array().unwrap_or(&empty) {
        for p in mt.get("patches").and_then(Value::as_array).unwrap_or(&empty) {
            let file = match p.get("file").and_then(Value::as_str) { Some(f) => f, None => continue };
            match by_file.iter_mut().find(|(f, _)| f == file) {
                Some((_, list)) => list.push(p.clone()),
                None => by_file.push((file.to_string(), vec![p.clone()])),
            }
        }
    }
    let mut authored: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for (file, patches) in &by_file {
        if patches.first().and_then(|p| p.get("op")).and_then(Value::as_str) != Some("create") {
            continue;
        }
        let mut content = String::new();
        let mut ok = true;
        for p in patches {
            let r = crate::agents_dev_apply_section_patch::applyPatchToSource(&json!(content), p);
            match r.as_str() {
                Some(s) => content = s.to_string(),
                None => { ok = false; break; }
            }
        }
        if ok {
            authored.insert(file.clone(), content);
        }
    }
    let mut gaps = Vec::new();
    for ac in acceptance_parsed.as_array().unwrap_or(&empty) {
        if !ac.is_object() || ac.get("kind").and_then(Value::as_str) != Some("grep-contains") {
            continue;
        }
        let file = ac.get("path").or_else(|| ac.get("file"));
        let pattern = ac.get("pattern").or_else(|| ac.get("must_contain")).or_else(|| ac.get("contains"));
        let (file, pattern) = match (file.and_then(Value::as_str), pattern.and_then(Value::as_str)) {
            (Some(f), Some(p)) if !p.is_empty() => (f, p),
            _ => continue,
        };
        match authored.get(file) {
            Some(content) if !content.contains(pattern) => gaps.push(json!({ "file": file, "pattern": pattern })),
            _ => {}
        }
    }
    Value::Array(gaps)
}

/// `export function dryRunPatches(contract, microtasks, workspaceRoot, fsApi)`. `fsApi` is
/// hardwired to real `std::fs` (the JS injectable defaults to `{existsSync, readFileSync}`, and
/// no test-side override crosses this boundary here).
pub fn dryRunPatches(contract: &Value, microtasks: &Value, workspace_root: &str) -> Value {
    let empty: Vec<Value> = Vec::new();
    let mut by_file: Vec<(String, Vec<Value>)> = Vec::new();
    for mt in microtasks.as_array().unwrap_or(&empty) {
        for p in mt.get("patches").and_then(Value::as_array).unwrap_or(&empty) {
            let file = match p.get("file").and_then(Value::as_str) { Some(f) => f, None => continue };
            match by_file.iter_mut().find(|(f, _)| f == file) {
                Some((_, list)) => list.push(p.clone()),
                None => by_file.push((file.to_string(), vec![p.clone()])),
            }
        }
    }
    let sandbox_abs_root = contract.get("sandbox_path").and_then(Value::as_str)
        .map(|sp| std::path::Path::new(&substrate_root_local()).join(sp));

    let mut failures = Vec::new();
    for (file_path, patches) in &by_file {
        let mut source = String::new();
        if patches.first().and_then(|p| p.get("op")).and_then(Value::as_str) != Some("create") {
            let sb_abs = sandbox_abs_root.as_ref().map(|r| r.join(file_path));
            if let Some(sb) = &sb_abs {
                if sb.exists() {
                    source = std::fs::read_to_string(sb).unwrap_or_default();
                } else {
                    let orig = std::path::Path::new(workspace_root).join(file_path);
                    if orig.exists() {
                        source = std::fs::read_to_string(&orig).unwrap_or_default();
                    }
                }
            } else {
                let orig = std::path::Path::new(workspace_root).join(file_path);
                if orig.exists() {
                    source = std::fs::read_to_string(&orig).unwrap_or_default();
                }
            }
        }
        let mut current = source;
        for p in patches {
            let r = crate::agents_dev_apply_section_patch::applyPatchToSource(&json!(current), p);
            match r.as_str() {
                Some(s) => current = s.to_string(),
                None => {
                    let op = p.get("op").and_then(Value::as_str).unwrap_or("replace_section");
                    failures.push(json!({ "file": file_path, "op": op, "reason": "anchor dry-run failed (see applyPatchToSource — exact message not surfaced through the Rust twin's Value-returning signature)" }));
                    break;
                }
            }
        }
    }
    Value::Array(failures)
}

// SUBSTRATE_ROOT (see module doc — inlined real-fs derivation, no win32 string algorithm needed).
fn substrate_root_local() -> String {
    if let Ok(v) = std::env::var("ANTILOKI_SUBSTRATE_ROOT") {
        if !v.is_empty() {
            return v;
        }
    }
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    match std::path::Path::new(manifest_dir).parent() {
        Some(p) => p.to_string_lossy().into_owned(),
        None => manifest_dir.to_string(),
    }
}

// ══ FEAT-PLAN-SPEC-LINT ══════════════════════════════════════════════════════════════════════

/// `export function lintPlanDefs(defs)`.
pub fn lintPlanDefs(defs: &Value) -> Value {
    let empty: Vec<Value> = Vec::new();
    let mut violations = Vec::new();
    for d in defs.as_array().unwrap_or(&empty) {
        let spec = d.get("file_spec_md").and_then(Value::as_str).unwrap_or("").to_string();
        let file = d.get("file").and_then(Value::as_str).unwrap_or("").to_string();
        let id = d.get("microtask_id").cloned().unwrap_or(json!("(unknown)"));

        if let Some(m) = vague_spec_re().captures(&spec) {
            let phrase = m.get(1).map(|x| x.as_str()).unwrap_or("");
            violations.push(json!({
                "microtask_id": id, "file": file,
                "problem": format!("spec uses the vague phrase \"{phrase}\" — name the EXACT value/setting instead (the per-file author cannot invent what the spec leaves open)"),
            }));
        }
        let is_package_json = Regex::new(r"(?i)(^|/)package\.json$").unwrap().is_match(&file);
        if is_package_json {
            if !versioned_dep_re().is_match(&spec) {
                violations.push(json!({
                    "microtask_id": id, "file": file,
                    "problem": "package.json spec names no exact versioned dependency (expected tokens like react@^18.2.0, @vitejs/plugin-react@^4.2.1) — list EVERY dependency and devDependency with its version",
                }));
            }
            if !Regex::new(r"(?i)\bscripts\b").unwrap().is_match(&spec) {
                violations.push(json!({
                    "microtask_id": id, "file": file,
                    "problem": "package.json spec does not define the scripts map — spell out each script name and its exact command",
                }));
            }
        } else if code_file_re().is_match(&file) && !code_concrete_re().is_match(&spec) {
            violations.push(json!({
                "microtask_id": id, "file": file,
                "problem": "code-file spec names no concrete structure (no export/import/function mentioned) — name the exact exports, imports and signatures the file must contain",
            }));
        }
    }
    Value::Array(violations)
}

/// `export function formatPlanLintBlock(violations)`.
pub fn formatPlanLintBlock(violations: &Value) -> String {
    let empty: Vec<Value> = Vec::new();
    let lines: Vec<String> = violations.as_array().unwrap_or(&empty).iter().map(|v| {
        format!("• [{}] {}: {}", js_string(&v["microtask_id"]), js_string(&v["file"]), js_string(&v["problem"]))
    }).collect();
    let mut out = vec![
        "═══ PLAN LINT FAILED (deterministic — fix exactly this, nothing else) ═══".to_string(),
        "Your microtask file specs were refused by the spec-quality lint BEFORE any review.".to_string(),
        "Rewrite ONLY the offending specs so each names exact values — dependency".to_string(),
        "names@versions, config keys with their values, export/component names.".to_string(),
        "Vague words are forbidden; the lint re-runs mechanically on your next answer.".to_string(),
        "".to_string(),
    ];
    out.extend(lines);
    out.join("\n")
}

// ══ v11 per-file authoring helpers ═══════════════════════════════════════════════════════════

/// `export function mustContainLinesFor(acceptanceParsed, file)`.
pub fn mustContainLinesFor(acceptance_parsed: &Value, file: &str) -> Value {
    let empty: Vec<Value> = Vec::new();
    let mut out = Vec::new();
    for ac in acceptance_parsed.as_array().unwrap_or(&empty) {
        if !ac.is_object() || ac.get("kind").and_then(Value::as_str) != Some("grep-contains") {
            continue;
        }
        let ac_file = ac.get("path").or_else(|| ac.get("file")).and_then(Value::as_str);
        let pattern = ac.get("pattern").or_else(|| ac.get("must_contain")).or_else(|| ac.get("contains")).and_then(Value::as_str);
        match (ac_file, pattern) {
            (Some(f), Some(p)) if f == file && !p.is_empty() => out.push(json!(p)),
            _ => {}
        }
    }
    Value::Array(out)
}

/// `export function planAuthoringHash(defs)`.
pub fn planAuthoringHash(defs: &Value) -> Value {
    let empty: Vec<Value> = Vec::new();
    let projected: Vec<Value> = defs.as_array().unwrap_or(&empty).iter().map(|d| json!([
        d.get("microtask_id").cloned().unwrap_or(Value::Null),
        d.get("file").cloned().unwrap_or(Value::Null),
        d.get("file_spec_md").cloned().unwrap_or(Value::Null),
    ])).collect();
    let s = serde_json::to_string(&Value::Array(projected)).unwrap_or_default();
    json!(hex::encode(Sha256::digest(s.as_bytes())))
}

/// `export function lintAuthoredCode(patches, { allow_fences = false })`.
pub fn lintAuthoredCode(patches: &Value, allow_fences: bool) -> Value {
    let empty: Vec<Value> = Vec::new();
    for p in patches.as_array().unwrap_or(&empty) {
        let code = p.get("replacement").and_then(Value::as_str).unwrap_or("");
        if !allow_fences && code.contains(CODE_FENCE_PAT) {
            return json!("code contains a markdown fence (```) — emit RAW file content only, never fenced blocks");
        }
        if prose_preamble_re().is_match(code) {
            let head: String = code.trim().chars().take(30).collect();
            return json!(format!("code starts with conversational prose (\"{head}…\") — emit RAW file content only, no commentary"));
        }
    }
    Value::Null
}

/// `export function gateAuthoredPatches(patches, source, mustContain, { allow_fences = false })`.
pub fn gateAuthoredPatches(patches: &Value, source: &Value, must_contain: &Value, allow_fences: bool) -> Value {
    let patches_arr = match patches.as_array() {
        Some(a) if !a.is_empty() => a,
        _ => return json!({ "ok": false, "kind": "anchor", "reason": "no patches emitted" }),
    };
    let code_lint = lintAuthoredCode(patches, allow_fences);
    if !code_lint.is_null() {
        return json!({ "ok": false, "kind": "lint", "reason": code_lint });
    }
    let mut content = source.as_str().unwrap_or("").to_string();
    for p in patches_arr {
        let r = crate::agents_dev_apply_section_patch::applyPatchToSource(&json!(content), p);
        match r.as_str() {
            Some(s) => content = s.to_string(),
            None => {
                let op = p.get("op").and_then(Value::as_str).unwrap_or("replace_section");
                return json!({ "ok": false, "kind": "anchor", "reason": format!("{op}: anchor dry-run failed") });
            }
        }
    }
    let empty: Vec<Value> = Vec::new();
    let misses: Vec<Value> = must_contain.as_array().unwrap_or(&empty).iter()
        .filter(|pat| pat.as_str().map_or(true, |p| !content.contains(p))).cloned().collect();
    if !misses.is_empty() {
        return json!({ "ok": false, "kind": "echo", "misses": misses, "content": content });
    }
    json!({ "ok": true, "content": content })
}

/// `export function formatAuthorCorrection(gate)`.
pub fn formatAuthorCorrection(gate: &Value) -> String {
    let kind = gate.get("kind").and_then(Value::as_str).unwrap_or("");
    if kind == "lint" {
        return [
            "═ CODE LINT FAILED (deterministic — fix exactly this) ═".to_string(),
            "Your snippet was refused before any apply attempt:".to_string(),
            format!("• {}", js_string(&gate["reason"])),
            "Return the SAME content decisions but as pure file content in `code` —".to_string(),
            "no markdown fences, no prose, nothing that is not part of the file.".to_string(),
        ].join("\n");
    }
    if kind == "anchor" {
        return [
            "═ ANCHOR DRY-RUN FAILED — your patches do NOT apply to the current source ═".to_string(),
            "The applier could not locate (or found ambiguous) the anchor below when applied".to_string(),
            "against the REAL file content shown in CURRENT SOURCE. Re-author with anchors".to_string(),
            "copied VERBATIM from CURRENT SOURCE (exact whitespace + surrounding lines for".to_string(),
            "uniqueness), or use a single create patch carrying the COMPLETE file.".to_string(),
            "".to_string(),
            format!("• {}", js_string(&gate["reason"])),
        ].join("\n");
    }
    let empty: Vec<Value> = Vec::new();
    let mut out = vec![
        "═ BYTE-EXACT ACCEPTANCE CHECK FAILED (deterministic — fix exactly this) ═".to_string(),
        "The file content your patches produce is MISSING the byte-exact pattern(s) below.".to_string(),
        "Whitespace and punctuation count — the check is mechanical, not semantic. Adjust".to_string(),
        "your replacement text so each pattern appears verbatim:".to_string(),
    ];
    out.extend(gate.get("misses").and_then(Value::as_array).unwrap_or(&empty).iter().map(|m| format!("- must contain exactly: `{}`", js_string(m))));
    out.join("\n")
}

/// `export function scopedFailureFor(file, signal, acceptanceParsed)`.
pub fn scopedFailureFor(file: &str, signal: &Value, acceptance_parsed: &Value) -> Value {
    let empty: Vec<Value> = Vec::new();
    let failing = signal.get("failing").and_then(Value::as_array).unwrap_or(&empty);
    if failing.is_empty() {
        return Value::Null;
    }
    let acceptance = acceptance_parsed.as_array().cloned().unwrap_or_default();
    let mut lines = Vec::new();
    for f in failing {
        let idx = f.get("ac_index").and_then(Value::as_i64).unwrap_or(-1);
        let item = if idx >= 0 { acceptance.get(idx as usize).cloned().unwrap_or(Value::Null) } else { Value::Null };
        let evidence_md = f.get("evidence_md").cloned().unwrap_or(Value::Null);
        let tokens = extractPathTokens(&item, &evidence_md);
        let has = tokens.as_array().map_or(false, |a| a.iter().any(|t| t.as_str() == Some(file)));
        if has {
            let kind = f.get("kind").cloned().unwrap_or(Value::Null);
            let ev = f.get("evidence_md").and_then(Value::as_str).filter(|s| !s.is_empty()).unwrap_or("(no evidence captured)");
            lines.push(format!("── AC#{} ({}) FAILED ──\n{ev}", js_string(&f["ac_index"]), js_string(&kind)));
        }
    }
    if lines.is_empty() {
        return Value::Null;
    }
    let mut out = vec!["A previous attempt FAILED QA on THIS file. Fix exactly this — do not re-emit the prior content verbatim:".to_string(), "".to_string()];
    out.extend(lines);
    json!(out.join("\n"))
}

/// `export function readAuthorPatches(outputKv, file)`.
pub fn readAuthorPatches(output_kv: &Value, file: &str) -> Value {
    let empty: Vec<Value> = Vec::new();
    let rows: Vec<&Value> = output_kv.as_array().unwrap_or(&empty).iter()
        .filter(|o| o.get("parent_output_id").map_or(true, Value::is_null)).collect();
    let cell = |k: &str| -> Value {
        for c in &rows {
            if c.get("field_name").and_then(Value::as_str) == Some(k) {
                for key in ["value_text", "value_number", "value_boolean"] {
                    if let Some(v) = c.get(key) {
                        if !v.is_null() {
                            return v.clone();
                        }
                    }
                }
                return Value::Null;
            }
        }
        Value::Null
    };
    let code = cell("code");
    let code_str = match code.as_str() {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => return json!([]),
    };
    let op = cell("op");
    let op_str = match op.as_str() {
        Some("insert_after") => "insert_after",
        Some("replace_section") => "replace_section",
        _ => "create",
    };
    let mut patch = Map::new();
    patch.insert("file".into(), json!(file));
    patch.insert("op".into(), json!(op_str));
    patch.insert("replacement".into(), json!(code_str));
    if let Some(before) = cell("anchor_before").as_str() {
        if !before.is_empty() {
            patch.insert("anchor_before".into(), json!(before));
        }
    }
    if let Some(after) = cell("anchor_after").as_str() {
        if !after.is_empty() {
            patch.insert("anchor_after".into(), json!(after));
        }
    }
    json!([Value::Object(patch)])
}

// ══ createAuthorEngine — the per-file authoring engine ═══════════════════════════════════════
// PARITY-DEFERRED past the guards/gate machinery: every attempt dispatches through
// `crate::ai_ai_call::aiCall`, which always `Err`s (see that module's doc) — so `authorOneFile`
// here always exhausts its 3-attempt ladder to a `failedFiles` entry. The ladder's shape (attempt
// loop, correction-block threading, echo-enforcement-on-last-microtask-per-file, partial-resume
// reuse) is ported faithfully; only the "gate.ok" success branch is structurally unreachable
// through this deferred boundary (same disclosed shape as book-run.rs's `fac_call`/`step`).
pub struct AuthorEngine<'a> {
    pub db: &'a Connection,
    pub contract: Value,
    pub acceptance_parsed: Value,
    pub signal: Value,
    pub is_reauthor: bool,
    pub bypass_cache: bool,
    pub refine_history: Value,
    pub skill: String,
    last_author_call_id: Value,
}

impl<'a> AuthorEngine<'a> {
    pub fn new(
        db: &'a Connection, contract: Value, acceptance_parsed: Value, signal: Value,
        is_reauthor: bool, bypass_cache: bool, refine_history: Value, skill: &str,
    ) -> Self {
        AuthorEngine {
            db, contract, acceptance_parsed, signal, is_reauthor, bypass_cache, refine_history,
            skill: skill.to_string(), last_author_call_id: Value::Null,
        }
    }

    pub fn getLastAuthorCallId(&self) -> Value {
        self.last_author_call_id.clone()
    }

    // `authorOneFile(def, {...})` — the 3-attempt low/freezed ladder against the deferred aiCall.
    fn author_one_file(&mut self, def: &Value, must_contain: &Value, enforce_echo: bool, source: &Value, plan_files_block: &str, briefing_excerpt: &str) -> Value {
        let file = def.get("file").and_then(Value::as_str).unwrap_or("");
        let gate_must_contain = if enforce_echo { must_contain.clone() } else { json!([]) };
        let use_form = AUTHOR_FORM_ENABLED();
        let must_contain_lines_str = {
            let empty: Vec<Value> = Vec::new();
            let items = must_contain.as_array().unwrap_or(&empty);
            if items.is_empty() {
                "(none)".to_string()
            } else {
                items.iter().map(|p| format!("- `{}`", js_string(p))).collect::<Vec<_>>().join("\n")
            }
        };
        let mut correction = if self.is_reauthor {
            let s = scopedFailureFor(file, &self.signal, &self.acceptance_parsed);
            if s.is_null() { json!("(none)") } else { s }
        } else {
            json!("(none)")
        };
        let mut last_reason = "unknown failure".to_string();
        let op_kind = def.get("op_kind").and_then(Value::as_str).unwrap_or("create").to_string();

        for attempt in 0..3 {
            let mut input_kv = vec![
                json!({ "key": "must_obey", "value": js_string(&self.contract.get("must_obey").cloned().unwrap_or(json!([]))) }),
                json!({ "key": "out_of_scope", "value": js_string(&self.contract.get("out_of_scope").cloned().unwrap_or(json!([]))) }),
                json!({ "key": "file", "value": file }),
                json!({ "key": "op_kind", "value": op_kind.clone() }),
                json!({ "key": "file_spec_md", "value": def.get("file_spec_md").and_then(Value::as_str).filter(|s| !s.is_empty()).unwrap_or("(no spec — author from intent + acceptance)") }),
                json!({ "key": "must_contain_lines", "value": must_contain_lines_str.clone() }),
                json!({ "key": "current_source", "value": source }),
                json!({ "key": "plan_files", "value": plan_files_block }),
                json!({ "key": "briefing_excerpt", "value": briefing_excerpt }),
                json!({ "key": "correction_block", "value": correction.clone() }),
            ];
            if use_form {
                input_kv.push(json!({ "key": "form_skeleton", "value": crate::agents_tl__helpers_author_form::renderAuthorForm(&json!({ "file": file, "op_kind": op_kind.clone() })) }));
            }
            let input = json!({
                "prompt_name": if use_form { AUTHOR_PROMPT_NAME_V3 } else { AUTHOR_PROMPT_NAME },
                "input_kv": input_kv,
                "tier": "low", "temperature": "freezed",
                "bypass_cache": attempt > 0 || self.bypass_cache || self.is_reauthor,
                "strict_feed_check": true,
                "skip_postmortem": attempt < 2,
            });
            match crate::ai_ai_call::aiCall(&input, self.db) {
                Ok(ai_result) => {
                    self.last_author_call_id = ai_result.get("ai_call_id").cloned().unwrap_or(Value::Null);
                    let output_kv = ai_result.get("output_kv").cloned().unwrap_or(json!([]));
                    let patches = readAuthorPatches(&output_kv, file);
                    let gate = gateAuthoredPatches(&patches, source, &gate_must_contain, use_form);
                    if truthy(gate.get("ok")) {
                        return json!({ "ok": true, "patches": patches, "content": gate["content"] });
                    }
                    let kind = gate.get("kind").and_then(Value::as_str).unwrap_or("");
                    last_reason = match kind {
                        "anchor" => format!("anchor dry-run failed: {}", js_string(&gate["reason"])),
                        "lint" => format!("code lint failed: {}", js_string(&gate["reason"])),
                        _ => format!("byte-exact misses: {}", gate.get("misses").and_then(Value::as_array).map_or(String::new(), |a| a.iter().map(|m| format!("`{}`", js_string(m))).collect::<Vec<_>>().join(", "))),
                    };
                    correction = json!(formatAuthorCorrection(&gate));
                }
                Err(e) => {
                    last_reason = format!("ai-call failed: {}", e.chars().take(300).collect::<String>());
                    correction = json!(if use_form {
                        format!("Your previous answer was rejected by the form compiler/validator:\n{}\nReturn ONLY the completed form — all sections present, one fenced code block, no leftover placeholders.", e.chars().take(600).collect::<String>())
                    } else {
                        format!("Your previous response was rejected by the output validator:\n{}\nReturn ONLY the flat JSON shape {{\"op\", \"anchor_before\", \"anchor_after\", \"code\"}} — code only, no prose.", e.chars().take(600).collect::<String>())
                    });
                }
            }
        }
        json!({ "ok": false, "reason": last_reason })
    }

    /// `authorDefs(defs, {planFilesBlock, briefingExcerpt})` — sequential; later microtasks on the
    /// same file see the earlier one's produced content (`fileState`). Partial-resume: a matching
    /// prior `partial_authoring` history entry lets succeeded files skip their AI call.
    pub fn authorDefs(&mut self, defs: &Value, plan_files_block: &str, briefing_excerpt: &str, resolve_source: impl Fn(&str, &std::collections::HashMap<String, String>) -> Option<String>) -> Value {
        let empty: Vec<Value> = Vec::new();
        let defs_arr = defs.as_array().unwrap_or(&empty);
        let hash = planAuthoringHash(defs);
        let refine_history = self.refine_history.as_array().cloned().unwrap_or_default();
        let last_partial = refine_history.iter().rev().find(|e| e.get("kind").and_then(Value::as_str) == Some("partial_authoring")).cloned();
        let resume = last_partial.filter(|p| p.get("plan_hash") == Some(&hash));

        let mut file_state: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        let mut failed_files = Vec::new();
        let mut succeeded = Map::new();

        let mut last_index_for_file: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for (i, d) in defs_arr.iter().enumerate() {
            if let Some(f) = d.get("file").and_then(Value::as_str) {
                last_index_for_file.insert(f.to_string(), i);
            }
        }

        for (i, def) in defs_arr.iter().enumerate() {
            let file = def.get("file").and_then(Value::as_str).unwrap_or("").to_string();
            let mid = def.get("microtask_id").and_then(Value::as_str).unwrap_or("").to_string();
            let enforce_echo = last_index_for_file.get(&file) == Some(&i);

            if let Some(r) = &resume {
                if let Some(reused) = r.get("succeeded").and_then(|s| s.get(&mid)) {
                    if let Some(reused_arr) = reused.as_array() {
                        if !reused_arr.is_empty() {
                            let stamped: Vec<Value> = reused_arr.iter().map(|p| {
                                let mut o = p.as_object().cloned().unwrap_or_default();
                                o.insert("file".into(), json!(file));
                                Value::Object(o)
                            }).collect();
                            let source = resolve_source(&file, &file_state).unwrap_or_default();
                            let mc = if enforce_echo { mustContainLinesFor(&self.acceptance_parsed, &file) } else { json!([]) };
                            let gate = gateAuthoredPatches(&json!(stamped), &json!(source), &mc, AUTHOR_FORM_ENABLED());
                            if truthy(gate.get("ok")) {
                                if let Some(c) = gate.get("content").and_then(Value::as_str) {
                                    file_state.insert(file.clone(), c.to_string());
                                }
                                succeeded.insert(mid.clone(), json!(stamped));
                                continue;
                            }
                        }
                    }
                }
            }

            let source_opt = resolve_source(&file, &file_state);
            let source_v = match &source_opt { Some(s) => json!(s), None => json!("(new file)") };
            let must_contain = mustContainLinesFor(&self.acceptance_parsed, &file);
            let r = self.author_one_file(def, &must_contain, enforce_echo, &source_v, plan_files_block, briefing_excerpt);
            if truthy(r.get("ok")) {
                if let Some(c) = r.get("content").and_then(Value::as_str) {
                    file_state.insert(file.clone(), c.to_string());
                }
                succeeded.insert(mid.clone(), r.get("patches").cloned().unwrap_or(json!([])));
            } else {
                failed_files.push(json!({ "microtask_id": mid, "file": file, "reason": r.get("reason").cloned().unwrap_or(Value::Null) }));
            }
        }

        json!({ "failedFiles": failed_files, "succeeded": Value::Object(succeeded), "plan_hash": hash })
    }

    /// `persistPartialAndThrow(mode, authoring)` — writes the `partial_authoring` history entry
    /// then returns the same `Err` the JS throws (dispatcher-level persistence, not a fixture).
    pub fn persistPartialAndThrow(&self, mode: &str, authoring: &Value) -> Result<(), String> {
        let partial_entry = json!({
            "kind": "partial_authoring",
            "ts": crate::clock::now_iso_nodb(), // frozen clock
            "mode": mode,
            "plan_hash": authoring.get("plan_hash").cloned().unwrap_or(Value::Null),
            "succeeded": authoring.get("succeeded").cloned().unwrap_or(json!({})),
            "failed": authoring.get("failedFiles").cloned().unwrap_or(json!([])),
        });
        let empty: Vec<Value> = Vec::new();
        let mut hist: Vec<Value> = self.refine_history.as_array().unwrap_or(&empty).iter()
            .filter(|e| e.get("kind").and_then(Value::as_str) != Some("partial_authoring")).cloned().collect();
        hist.push(partial_entry);
        let slug = self.contract.get("slug").and_then(Value::as_str).unwrap_or("");
        crate::db_json::run_write(
            self.db,
            &format!("UPDATE contracts SET refine_history_json = ?, updated_at = {} WHERE slug = ?", crate::clock::now_sql()),
            &[json!(serde_json::to_string(&hist).unwrap_or_default()), json!(slug)],
        )?;
        let failed = authoring.get("failedFiles").and_then(Value::as_array).cloned().unwrap_or_default();
        let joined = failed.iter().map(|f| format!("{} ({})", js_string(&f["file"]), js_string(&f["reason"]))).collect::<Vec<_>>().join("; ");
        Err(format!(
            "{}: per-file authoring failed for {} file(s): {}. Plan + succeeded patches persisted (partial_authoring) — the next attempt re-authors ONLY the failed files.",
            self.skill, failed.len(), joined
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── toList / readMicrotaskDefs / buildPlanFromOutput ────────────────────────────────
    #[test]
    fn to_list_heals_array_stringified_and_csv_shapes() {
        assert_eq!(toList(&Value::Null), json!([]));
        assert_eq!(toList(&json!(["a", "b"])), json!(["a", "b"]));
        assert_eq!(toList(&json!(r#"["x","y"]"#)), json!(["x", "y"]));
        assert_eq!(toList(&json!("a, b ,c")), json!(["a", "b", "c"]));
        assert_eq!(toList(&json!("")), json!([]));
        assert_eq!(toList(&json!("  ")), json!([]));
    }

    #[test]
    fn read_microtask_defs_reconstructs_parent_child_rows() {
        let output_kv = json!([
            { "id": 1, "field_name": "microtasks", "parent_output_id": null, "order_index": 0 },
            { "id": 2, "parent_output_id": 1, "field_name": "microtask_id", "value_text": "mt-1" },
            { "id": 3, "parent_output_id": 1, "field_name": "file", "value_text": " src/x.ts " },
            { "id": 4, "parent_output_id": 1, "field_name": "op_kind", "value_text": "modify" },
            { "id": 5, "parent_output_id": 1, "field_name": "acceptance", "value_text": "does the thing" },
            { "id": 6, "parent_output_id": 1, "field_name": "depends_on", "value_text": "a,b" },
        ]);
        let defs = readMicrotaskDefs(&output_kv);
        assert_eq!(defs, json!([{
            "microtask_id": "mt-1", "intent": "", "file": "src/x.ts",
            "file_spec_md": "", "op_kind": "modify",
            "acceptance": ["does the thing"], "depends_on": ["a", "b"],
        }]));
    }

    #[test]
    fn build_plan_from_output_flags_empty_refusal() {
        let plan = buildPlanFromOutput(&json!([]), &json!({ "slug": "c1" }), 0.5);
        assert_eq!(plan["empty_refusal"], json!(true));
        assert_eq!(plan["microtasks"], json!([]));
        assert_eq!(plan["plan_id"], json!("plan-c1-rejected"));
    }

    #[test]
    fn build_plan_from_output_floors_per_task_cost() {
        // JS doc comment's own example: 11 tasks over a 0.5 budget — rounding (0.0455) would
        // overshoot (11*0.0455=0.5005); flooring (0.0454) must not.
        let mut rows: Vec<Value> = Vec::new();
        let mut id = 1i64;
        for i in 0..11 {
            rows.push(json!({ "id": id, "field_name": "microtasks", "parent_output_id": null, "order_index": i }));
            let parent_id = id;
            id += 1;
            rows.push(json!({ "id": id, "parent_output_id": parent_id, "field_name": "file", "value_text": format!("f{i}.ts") }));
            id += 1;
        }
        let output_kv = Value::Array(rows);
        let plan = buildPlanFromOutput(&output_kv, &json!({ "slug": "c2" }), 0.5);
        let mts = plan["microtasks"].as_array().unwrap();
        assert_eq!(mts.len(), 11);
        let total: f64 = mts.iter().map(|m| m["max_cost_usd"].as_f64().unwrap()).sum();
        assert!(total <= 0.5, "total {total} must not exceed budget (floor, not round)");
        assert_eq!(mts[0]["max_cost_usd"], json!(0.0454));
    }

    // ── sanityCheckPath / expandScopeOrReject ────────────────────────────────────────────
    #[test]
    fn sanity_check_path_flags_each_violation_class() {
        assert_eq!(sanityCheckPath(&json!(1)), json!("not a string"));
        assert_eq!(sanityCheckPath(&json!("")), json!("empty"));
        assert_eq!(sanityCheckPath(&json!(" a ")), json!("surrounding whitespace"));
        assert_eq!(sanityCheckPath(&json!("<UNKNOWN_PATHS_TL_REVIEW>")), json!("placeholder pattern"));
        assert_eq!(sanityCheckPath(&json!("src/*.ts")), json!("glob metachars"));
        assert_eq!(sanityCheckPath(&json!("src/$(rm).ts")), json!("shell metachars"));
        assert_eq!(sanityCheckPath(&json!("src/ok.ts")), Value::Null);
    }

    #[test]
    fn expand_scope_unions_new_valid_paths_and_drops_placeholders() {
        let contract = json!({ "slug": "c1", "files_touched": ["<UNKNOWN_PATHS_TL_REVIEW>", "existing.ts"] });
        let plan = json!({ "microtasks": [{ "microtask_id": "m1", "files_touched": ["new.ts"] }] });
        let r = expandScopeOrReject(&plan, &contract, "tl-refine-microtasks").unwrap();
        assert_eq!(r["expanded"], json!(true));
        assert_eq!(r["files_touched"], json!(["existing.ts", "new.ts"]));
        assert_eq!(r["newPathsAdded"], json!(["new.ts"]));
    }

    #[test]
    fn expand_scope_rejects_invalid_paths() {
        let contract = json!({ "slug": "c1", "files_touched": [] });
        let plan = json!({ "microtasks": [{ "microtask_id": "m1", "files_touched": ["src/*.ts"] }] });
        let e = expandScopeOrReject(&plan, &contract, "tl-refine-microtasks").unwrap_err();
        assert!(e.contains("invalid paths"), "{e}");
        assert!(e.contains("glob metachars"), "{e}");
    }

    #[test]
    fn expand_scope_rejects_over_path_cap() {
        let files: Vec<String> = (0..25).map(|i| format!("f{i}.ts")).collect();
        let contract = json!({ "slug": "c1", "files_touched": files });
        let plan = json!({ "microtasks": [] });
        let e = expandScopeOrReject(&plan, &contract, "tl-refine-microtasks").unwrap_err();
        assert!(e.contains("exceeds cap"), "{e}");
    }

    // ── coverageGaps ──────────────────────────────────────────────────────────────────────
    #[test]
    fn coverage_gaps_flags_net_new_unpatched_ungenerated_files() {
        let files_touched = json!(["a.ts", "b.ts", "node_modules/x", "existing.ts"]);
        let microtasks = json!([{ "patches": [{ "file": "a.ts" }] }]);
        let generated = json!(["node_modules/"]);
        let gaps = coverageGaps(&files_touched, &microtasks, &generated, |f| f == "existing.ts");
        assert_eq!(gaps, json!(["b.ts"]));
    }

    // ── sliced-retry pure helpers ─────────────────────────────────────────────────────────
    #[test]
    fn extract_path_tokens_from_structured_and_evidence() {
        let ac = json!({ "path": "src/a.ts", "paths": ["src/b.ts"], "cmd": "node src/c.mjs" });
        let toks = extractPathTokens(&ac, &json!("touch src/d.js"));
        let arr: Vec<String> = toks.as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
        assert!(arr.contains(&"src/a.ts".to_string()));
        assert!(arr.contains(&"src/b.ts".to_string()));
        assert!(arr.contains(&"src/c.mjs".to_string()));
        assert!(arr.contains(&"src/d.js".to_string()));
    }

    #[test]
    fn implicate_microtasks_maps_failing_acs_via_file_overlap() {
        let failing = json!([{ "ac_index": 0, "evidence_md": "" }]);
        let acceptance = json!([{ "path": "src/a.ts" }]);
        let microtasks = json!([{ "microtask_id": "m1", "files_touched": ["src/a.ts"] }]);
        let r = implicateMicrotasks(&failing, &acceptance, &microtasks);
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["implicated_ids"], json!(["m1"]));
    }

    #[test]
    fn implicate_microtasks_unmapped_ac_is_not_ok() {
        let failing = json!([{ "ac_index": 0, "evidence_md": "no path here" }]);
        let acceptance = json!([{ "kind": "command" }]);
        let microtasks = json!([{ "microtask_id": "m1", "files_touched": ["src/a.ts"] }]);
        let r = implicateMicrotasks(&failing, &acceptance, &microtasks);
        assert_eq!(r["ok"], json!(false));
        assert_eq!(r["reason"], json!("unmapped-failing-ac"));
    }

    #[test]
    fn decide_retry_mode_full_on_build_gate_red() {
        let r = decideRetryMode(&json!({ "buildGateRed": true }));
        assert_eq!(r["mode"], json!("full"));
        assert_eq!(r["why"], json!("build-gate-red"));
    }

    #[test]
    fn decide_retry_mode_sliced_on_minority_implicated() {
        let opts = json!({
            "buildGateRed": false, "qaRun": { "id": 1 },
            "failingAcs": [{ "ac_index": 0 }], "totalAcs": 4,
            "implication": { "ok": true, "implicated_ids": ["m1"] },
            "planSize": 4, "refineHistory": [],
        });
        let r = decideRetryMode(&opts);
        assert_eq!(r["mode"], json!("sliced"));
        assert_eq!(r["replaced_ids"], json!(["m1"]));
    }

    #[test]
    fn decide_retry_mode_full_when_sliced_already_failed_same_acs() {
        let opts = json!({
            "buildGateRed": false, "qaRun": { "id": 1 },
            "failingAcs": [{ "ac_index": 0 }], "totalAcs": 4,
            "implication": { "ok": true, "implicated_ids": ["m1"] },
            "planSize": 4,
            "refineHistory": [{ "mode": "sliced", "failing_ac_indices": [0] }],
        });
        let r = decideRetryMode(&opts);
        assert_eq!(r["mode"], json!("full"));
        assert_eq!(r["why"], json!("sliced-already-failed-same-acs"));
    }

    #[test]
    fn merge_sliced_plan_keeps_and_replaces_in_order() {
        let prior = json!([
            { "microtask_id": "m1", "files_touched": ["a.ts"] },
            { "microtask_id": "m2", "files_touched": ["b.ts"] },
        ]);
        let kept_ids = json!(["m1"]);
        let replacements = json!([{ "microtask_id": "m3", "patches": [{ "file": "b.ts" }] }]);
        let r = mergeSlicedPlan(&prior, &kept_ids, &replacements, &json!("sliced_retry"));
        assert_eq!(r["ok"], json!(true));
        let merged = r["merged"].as_array().unwrap();
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0]["microtask_id"], json!("m1"));
        assert_eq!(merged[1]["microtask_id"], json!("m3"));
        assert_eq!(merged[1]["origin"], json!("sliced_retry"));
    }

    #[test]
    fn merge_sliced_plan_voids_when_replacement_touches_kept_only_file() {
        let prior = json!([
            { "microtask_id": "m1", "files_touched": ["a.ts"] },
            { "microtask_id": "m2", "files_touched": ["b.ts"] },
        ]);
        let kept_ids = json!(["m1"]);
        let replacements = json!([{ "microtask_id": "m3", "patches": [{ "file": "a.ts" }] }]);
        let r = mergeSlicedPlan(&prior, &kept_ids, &replacements, &json!("sliced_retry"));
        assert_eq!(r["ok"], json!(false));
    }

    #[test]
    fn prior_patches_block_lists_each_patch_line() {
        let contract = json!({ "microtasks_json": r#"[{"patches":[{"file":"a.ts","op":"create"},{"file":"b.ts","op":"insert_after","anchor_before":"const x = 1;"}]}]"# });
        let block = priorPatchesBlock(&contract);
        assert!(block.contains("• a.ts [create]"));
        assert!(block.contains("• b.ts [insert_after] after \"const x = 1;\""));
    }

    #[test]
    fn prior_patches_block_defaults_when_none_recorded() {
        assert_eq!(priorPatchesBlock(&json!({})), "(none recorded)");
    }

    // ── plan/code lints ───────────────────────────────────────────────────────────────────
    #[test]
    fn lint_plan_defs_flags_vague_spec_and_missing_deps() {
        let defs = json!([
            { "microtask_id": "m1", "file": "src/x.ts", "file_spec_md": "configure it as appropriate" },
            { "microtask_id": "m2", "file": "package.json", "file_spec_md": "add react and vite, scripts included" },
            { "microtask_id": "m3", "file": "src/y.ts", "file_spec_md": "export function y() {}" },
        ]);
        let violations = lintPlanDefs(&defs).as_array().unwrap().clone();
        assert!(violations.iter().any(|v| v["microtask_id"] == json!("m1") && v["problem"].as_str().unwrap().contains("vague phrase")));
        assert!(violations.iter().any(|v| v["microtask_id"] == json!("m2") && v["problem"].as_str().unwrap().contains("versioned dependency")));
        assert!(!violations.iter().any(|v| v["microtask_id"] == json!("m3")));
    }

    #[test]
    fn format_plan_lint_block_lists_every_violation() {
        let violations = json!([{ "microtask_id": "m1", "file": "x.ts", "problem": "no export named" }]);
        let block = formatPlanLintBlock(&violations);
        assert!(block.contains("PLAN LINT FAILED"));
        assert!(block.contains("• [m1] x.ts: no export named"));
    }

    #[test]
    fn lint_authored_code_flags_fence_and_preamble() {
        assert!(lintAuthoredCode(&json!([{ "replacement": "```js\ncode\n```" }]), false).as_str().unwrap().contains("markdown fence"));
        assert!(lintAuthoredCode(&json!([{ "replacement": "Here is the file:\ncode" }]), false).as_str().unwrap().contains("conversational prose"));
        assert_eq!(lintAuthoredCode(&json!([{ "replacement": "export const x = 1;" }]), false), Value::Null);
        // allow_fences=true (v3 form mode) tolerates fences.
        assert_eq!(lintAuthoredCode(&json!([{ "replacement": "```js\ncode\n```" }]), true), Value::Null);
    }

    // ── must-contain / hash / gate ────────────────────────────────────────────────────────
    #[test]
    fn must_contain_lines_for_filters_by_file() {
        let acceptance = json!([
            { "kind": "grep-contains", "path": "a.ts", "pattern": "export const x" },
            { "kind": "grep-contains", "path": "b.ts", "pattern": "export const y" },
        ]);
        assert_eq!(mustContainLinesFor(&acceptance, "a.ts"), json!(["export const x"]));
    }

    #[test]
    fn plan_authoring_hash_is_stable_and_order_sensitive() {
        let defs1 = json!([{ "microtask_id": "m1", "file": "a.ts", "file_spec_md": "spec" }]);
        let defs2 = json!([{ "microtask_id": "m1", "file": "a.ts", "file_spec_md": "spec" }]);
        assert_eq!(planAuthoringHash(&defs1), planAuthoringHash(&defs2));
        let defs3 = json!([{ "microtask_id": "m1", "file": "a.ts", "file_spec_md": "different" }]);
        assert_ne!(planAuthoringHash(&defs1), planAuthoringHash(&defs3));
    }

    #[test]
    fn gate_authored_patches_create_succeeds_and_checks_must_contain() {
        let patches = json!([{ "op": "create", "replacement": "export const x = 1;" }]);
        let gate = gateAuthoredPatches(&patches, &json!(""), &json!(["export const x"]), false);
        assert_eq!(gate["ok"], json!(true));
        assert_eq!(gate["content"], json!("export const x = 1;"));

        let gate2 = gateAuthoredPatches(&patches, &json!(""), &json!(["missing pattern"]), false);
        assert_eq!(gate2["ok"], json!(false));
        assert_eq!(gate2["kind"], json!("echo"));
    }

    #[test]
    fn gate_authored_patches_no_patches_fails_anchor_kind() {
        let gate = gateAuthoredPatches(&json!([]), &json!(""), &json!([]), false);
        assert_eq!(gate["ok"], json!(false));
        assert_eq!(gate["kind"], json!("anchor"));
    }

    #[test]
    fn format_author_correction_covers_all_three_gate_kinds() {
        assert!(formatAuthorCorrection(&json!({ "kind": "lint", "reason": "bad" })).contains("CODE LINT FAILED"));
        assert!(formatAuthorCorrection(&json!({ "kind": "anchor", "reason": "bad" })).contains("ANCHOR DRY-RUN FAILED"));
        assert!(formatAuthorCorrection(&json!({ "kind": "echo", "misses": ["x"] })).contains("BYTE-EXACT ACCEPTANCE CHECK FAILED"));
    }

    #[test]
    fn scoped_failure_for_only_returns_when_file_implicated() {
        let signal = json!({ "failing": [{ "ac_index": 0, "kind": "command", "evidence_md": "boom" }] });
        let acceptance = json!([{ "path": "a.ts" }]);
        let r = scopedFailureFor("a.ts", &signal, &acceptance);
        assert!(r.as_str().unwrap().contains("FAILED QA on THIS file"));
        let r2 = scopedFailureFor("b.ts", &signal, &acceptance);
        assert_eq!(r2, Value::Null);
    }

    #[test]
    fn read_author_patches_builds_flat_patch_from_flat_output_kv() {
        let output_kv = json!([
            { "field_name": "op", "value_text": "insert_after", "parent_output_id": null },
            { "field_name": "anchor_before", "value_text": "const a = 1;", "parent_output_id": null },
            { "field_name": "code", "value_text": "const b = 2;", "parent_output_id": null },
        ]);
        let patches = readAuthorPatches(&output_kv, "x.ts");
        assert_eq!(patches, json!([{ "file": "x.ts", "op": "insert_after", "replacement": "const b = 2;", "anchor_before": "const a = 1;" }]));
    }

    #[test]
    fn read_author_patches_empty_code_returns_empty() {
        let output_kv = json!([{ "field_name": "op", "value_text": "create", "parent_output_id": null }]);
        assert_eq!(readAuthorPatches(&output_kv, "x.ts"), json!([]));
    }

    // ── grepAcEchoGaps ────────────────────────────────────────────────────────────────────
    #[test]
    fn grep_ac_echo_gaps_flags_missing_byte_exact_pattern_in_created_file() {
        let acceptance = json!([{ "kind": "grep-contains", "path": "a.ts", "pattern": "\"strict\": true" }]);
        let microtasks = json!([{ "patches": [{ "file": "a.ts", "op": "create", "replacement": "{\"strict\":true}" }] }]);
        let gaps = grepAcEchoGaps(&acceptance, &microtasks);
        assert_eq!(gaps, json!([{ "file": "a.ts", "pattern": "\"strict\": true" }]));
    }

    #[test]
    fn grep_ac_echo_gaps_clean_when_pattern_present() {
        let acceptance = json!([{ "kind": "grep-contains", "path": "a.ts", "pattern": "hello" }]);
        let microtasks = json!([{ "patches": [{ "file": "a.ts", "op": "create", "replacement": "hello world" }] }]);
        assert_eq!(grepAcEchoGaps(&acceptance, &microtasks), json!([]));
    }

    // ── ensurePlanPrompts (db write via promptFind/promptAdd twins) ─────────────────────────
    fn brain_db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain").unwrap();
        c.execute_batch(
            "CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL CHECK (default_tier IN ('low','mid','high')), default_temperature TEXT NOT NULL CHECK (default_temperature IN ('freezed','cold','warm','hot','hell')), max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL CHECK (field_type IN ('string','number','boolean','enum','array_of_string','array_of_object')), enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL CHECK (field_type IN ('string','number','boolean','enum','array_of_string','array_of_object')), enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );"
        ).unwrap();
        c
    }

    #[test]
    fn ensure_plan_prompts_registers_all_three_templates_idempotently() {
        let c = brain_db();
        ensurePlanPrompts(&c).unwrap();
        let count: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 3);
        // Re-running with the SAME template text does not bump versions (ensurePromptCurrent no-ops).
        ensurePlanPrompts(&c).unwrap();
        let count2: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates", [], |r| r.get(0)).unwrap();
        assert_eq!(count2, 3);
    }

    // ── failure-context assembly (db reads on a partial DB — tables missing → Null/empty) ──
    #[test]
    fn build_gate_failure_block_null_on_missing_table() {
        let c = Connection::open_in_memory().unwrap();
        let r = buildGateFailureBlock(&c, &json!({ "slug": "c1" }));
        assert_eq!(r, Value::Null);
    }

    #[test]
    fn build_gate_failure_block_surfaces_real_evidence() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE build_gate_runs (id INTEGER PRIMARY KEY, contract_slug TEXT, passed INTEGER, skipped INTEGER, build_cmd TEXT, evidence_md TEXT)").unwrap();
        c.execute("INSERT INTO build_gate_runs (contract_slug, passed, skipped, build_cmd, evidence_md) VALUES ('c1', 0, 0, 'npm run build', 'TS2345: boom')", []).unwrap();
        let r = buildGateFailureBlock(&c, &json!({ "slug": "c1", "microtasks_json": "[]" }));
        let s = r.as_str().unwrap();
        assert!(s.contains("npm run build"));
        assert!(s.contains("TS2345: boom"));
    }

    #[test]
    fn gather_failure_signal_and_qa_failure_context_assemble_ac_evidence() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE qa_runs (id INTEGER PRIMARY KEY, contract_slug TEXT, all_passed INTEGER, feedback_md TEXT, acceptance_fail INTEGER, acceptance_pass INTEGER);
             CREATE TABLE qa_run_results (id INTEGER PRIMARY KEY, qa_run_id INTEGER, ac_index INTEGER, kind TEXT, evidence_md TEXT, passed INTEGER);"
        ).unwrap();
        c.execute("INSERT INTO qa_runs (id, contract_slug, all_passed, feedback_md, acceptance_fail, acceptance_pass) VALUES (1, 'c1', 0, 'still broken', 1, 2)", []).unwrap();
        c.execute("INSERT INTO qa_run_results (qa_run_id, ac_index, kind, evidence_md, passed) VALUES (1, 0, 'command', 'exit 1', 0)", []).unwrap();
        let contract = json!({ "slug": "c1", "microtasks_json": "[]" });
        let signal = gatherFailureSignal(&c, &contract);
        assert_eq!(signal["run"]["id"], json!(1));
        assert_eq!(signal["failing"].as_array().unwrap().len(), 1);
        let ctx = buildQaFailureContext(&c, &contract, &Value::Null);
        let s = ctx.as_str().unwrap();
        assert!(s.contains("AC#0 (command) FAILED"));
        assert!(s.contains("exit 1"));
        assert!(s.contains("still broken"));
    }

    #[test]
    fn build_qa_failure_context_null_when_no_signal_at_all() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE qa_runs (id INTEGER PRIMARY KEY, contract_slug TEXT, all_passed INTEGER, feedback_md TEXT, acceptance_fail INTEGER, acceptance_pass INTEGER); CREATE TABLE qa_run_results (id INTEGER PRIMARY KEY)").unwrap();
        let ctx = buildQaFailureContext(&c, &json!({ "slug": "nope" }), &Value::Null);
        assert_eq!(ctx, Value::Null);
    }

    // ── AuthorEngine (deferred aiCall → always exhausts to failedFiles) ─────────────────────
    #[test]
    fn author_defs_exhausts_to_failed_files_via_deferred_ai_call() {
        let c = Connection::open_in_memory().unwrap();
        let mut engine = AuthorEngine::new(
            &c, json!({ "slug": "c1", "must_obey": [], "out_of_scope": [] }),
            json!([]), json!({ "failing": [] }), false, false, json!([]), "tl-author-snippets",
        );
        let defs = json!([{ "microtask_id": "m1", "file": "src/x.ts", "op_kind": "create", "file_spec_md": "spec" }]);
        let result = engine.authorDefs(&defs, "", "", |_file, _state| None);
        let failed = result["failedFiles"].as_array().unwrap();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0]["file"], json!("src/x.ts"));
        assert!(engine.getLastAuthorCallId().is_null());
    }

    #[test]
    fn persist_partial_and_throw_writes_history_and_returns_err() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE contracts (slug TEXT PRIMARY KEY, refine_history_json TEXT, updated_at TEXT)").unwrap();
        c.execute("INSERT INTO contracts (slug, refine_history_json) VALUES ('c1', '[]')", []).unwrap();
        let engine = AuthorEngine::new(&c, json!({ "slug": "c1" }), json!([]), json!({}), false, false, json!([]), "tl-author-snippets");
        let authoring = json!({ "plan_hash": "abc", "succeeded": {}, "failedFiles": [{ "microtask_id": "m1", "file": "x.ts", "reason": "boom" }] });
        let e = engine.persistPartialAndThrow("full", &authoring).unwrap_err();
        assert!(e.contains("per-file authoring failed for 1 file(s)"));
        assert!(e.contains("x.ts (boom)"));
        let stored: String = c.query_row("SELECT refine_history_json FROM contracts WHERE slug = 'c1'", [], |r| r.get(0)).unwrap();
        assert!(stored.contains("partial_authoring"));
    }
}

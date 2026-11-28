//! snippet-author.rs — twin of skills/blueprint/snippet-author.mjs (db lane). NAMES ARE CONTRACT:
//! default `snippetAuthor`. DEV authors code per BLOCK (pipeline v2): loads the active blueprint,
//! ensures the two authoring prompts exist, gathers re-author feedback (judgment court verdicts /
//! build-gate / test-gate evidence), builds per-file grounding via contextBundle, groups leaves
//! into whole-file vs per-leaf authoring, dispatches ONE ai-call per file/leaf, lints + persists
//! the result, and (when the blueprint carries planned_nodes) verifies the authored files against
//! the design stub.
//!
//! PARITY-DEFERRED: every authoring attempt calls `crate::ai_ai_call::aiCall`, which (per its own
//! module doc) always `Err`s — the provider dispatch is a live network call, non-deterministic and
//! unfixturable in principle. JS wraps every `aiCall` call in try/catch that degrades to a TODO
//! stub on ANY failure; ported here identically — the whole-file and per-leaf call sites call
//! aiCall via a `match`, and the `Err` arm collapses to `None`, exactly mirroring the JS catch
//! block that sets `snippet = null`. Since aiCall Errs unconditionally and deterministically (no
//! retry would ever succeed differently), the JS 3-attempt "rotate model" ladder collapses to a
//! single attempt here — same convention as tl-author-snippets.rs's AuthorEngine (its ladder also
//! always exhausts to the same outcome against this stub). The response-shaping that would run on
//! a REAL aiCall success (`parseLoose`/`pickSnippet`/`splitCodeArtifact`/`acceptAuthoredCode`) is
//! still ported in full (`whole_file_from_response`/`leaf_from_response` below) and reachable
//! structurally the moment aiCall's dispatch is un-stubbed — `parseLoose`/`pickSnippet` are pure
//! and exercised directly by the native tests. The truncation-guard / `aiOutcomeReport` /
//! `contextBundlePersist` telemetry calls that only fire on a real aiCall SUCCESS are not wired at
//! the call sites: they would be unreachable dead code with no fixture path here (aiCall never
//! returns `Ok`), so they're omitted per MINIMUM_VIABLE_CHANGE rather than faked for coverage.
//!
//! Everything AROUND the dispatch is ported faithfully and exercised by the native tests below:
//! blueprint/contract reads, `normalizeBlueprint`/`flattenBlocks` (already-converted twins),
//! prompt-ensure (`promptFind`/`promptAdd`), the judgment-court / build-gate / test-gate
//! `refine_feedback` assembly, `contextBundle` per-file grounding (itself real — only ITS internal
//! rag/research gatherers touch the deferred embed boundary, and they degrade gracefully per their
//! own module docs), the whole-file-vs-per-leaf grouping (`ANTILOKI_WHOLE_FILE_AUTHOR` env gate),
//! the design-bound CONST fast path (never calls AI — byte-exact literal emission), `snippetLint` +
//! its `snippet_lint_runs` persistence, and `verifyAgainstStub`.
//!
//! `err.*`/`AntilokiError` (engine/error.mjs) are PARITY-DEFERRED at the class level (every other
//! twin's convention) — collapsed to `Err("snippet-author: <cause>".into())`.
//! `context.provider_fn` has no Rust counterpart (same as tl-author-snippets.rs/tl-plan-contract.rs
//! — it only threads into the deferred aiCall dispatch, which never inspects it here).
//! `console.log` gate-marker lines use the frozen-clock ISO stamp `"2026-01-01T00:00:00.000Z"`
//! (crossing playbook convention — `Date.now()`/`toISOString()` has no fixture-stable Rust
//! equivalent) instead of a live timestamp.
// QUALITY: mixed unit — full deterministic surface (blueprint load/normalize, prompt-ensure,
// judgment/build/test-gate feedback, contextBundle grounding, whole-file grouping, const fast
// path, lint+persist, verifyAgainstStub) ported + fixture-tested; the AI-authoring leaf is
// PARITY-DEFERRED (aiCall always Errs → deterministic TODO-stub fallback, itself tested).

use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
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
} // = JS String(v ?? '')
fn js_len(s: &str) -> usize { s.encode_utf16().count() } // = JS .length
fn js_slice(s: &str, n: usize) -> String { s.chars().take(n).collect() } // = JS .slice(0, n) (char approx)
fn js_slice_tail(s: &str, n: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= n { s.to_string() } else { chars[chars.len() - n..].iter().collect() }
} // = JS .slice(-n)
fn num_str(v: Option<&Value>) -> String {
    match v {
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::String(s)) => s.clone(),
        Some(o) => o.to_string(),
        None => "undefined".to_string(),
    }
} // = JS `${v}` template interpolation

// ── prompt bodies (byte-identical to the JS module constants) ──────────────────────────────
const PROMPT: &str = "blueprint.snippet.author.v3";
const SYSTEM: &str = "You are a focused code generator. You write ONE code block from its intent + its typed inputs and outputs. Your entire reply is raw code — never JSON, never prose.";
const TEMPLATE: &str = r##"Generate the code for ONE block (a block of a file).

FILE: {{path}}
FILE PURPOSE: {{file_intent}}
BLOCK type: {{type}}    signature: {{signature}}
INTENT: {{intent}}
INPUTS (data entering this block, from its bindings): {{inputs}}
OUTPUTS (data this block must produce): {{outputs}}
DECIDED NAMES (the naming authority already chose these — use them EXACTLY, never invent variants): {{decided_names}}
SIBLING BLOCKS IN THIS FILE (authored SEPARATELY — they already exist in this module): {{siblings}}

{{codebase_context}}

{{file_stub}}

{{refine_feedback}}

RULES:
- If a SIGNATURE is given above it is FIXED — it comes from the design stub the whole architecture
  is parsed from. Implement the BODY for the intent; keep the EXACT name, parameters, and return
  type (a changed signature breaks the planned graph and fails the verify gate).
- If NO signature is given, this block is the file's whole module: write the complete, working
  content the FILE PURPOSE describes (all imports, declarations, and exports).
- If a DESIGN STUB is shown, define the SAME top-level symbols with the SAME signatures — fill the
  TODO bodies with the real implementation.
- Constants declared in the DESIGN STUB are IMMUTABLE design decisions (storage keys, routes,
  labels): reproduce each one byte-for-byte — never rename it, never change its value.
- Use ONLY the stack shown in the context above — never import a framework, store, or library the
  stack does not list.
- STATE HYDRATION (r34): when the intent says state initializes FROM storage (
  localStorage/sessionStorage) "before initial state", use the LAZY INITIALIZER —
  useState(() => read(...)) — NEVER useState(default) + useEffect-overwrite (that
  renders the default first and was judged RED three rounds straight).
- NEVER leave a TODO/placeholder body: if you cannot finish a block, the lint REDs
  it — emit the complete implementation.
- NEVER declare, redefine, or re-export a SIBLING BLOCK — write ONLY your own block and reference
  siblings directly (same module scope). Declaring a sibling duplicates it at assembly and fails
  the lint ('declared 2×' — the exact loop that burned r28's whole reauthor budget).
- If a PREVIOUS FAILURE is shown, your code MUST fix exactly what it describes — and it OVERRIDES
  EVERY rule above, DECIDED NAMES and stub constants included. If the failure says a key, name, or
  text is wrong, the failure's required value WINS: the judge enforces the CONTRACT, and the
  contract outranks the design. Reproducing a rejected value because a stub or decided name carries
  it is the exact loop that burns the reauthor budget.

Your ENTIRE reply is this block's code (for a whole-module block, the file's full content;
otherwise no surrounding scaffolding — just this block). Put the import lines this block needs
at the TOP. Output ONLY raw code — no prose, no fences, no JSON, no field names."##;

const FILE_PROMPT: &str = "blueprint.file.author.v2";
const FILE_TEMPLATE: &str = r##"Generate the COMPLETE source for ONE FILE.

FILE: {{path}}
FILE PURPOSE: {{file_intent}}

THIS FILE MUST DECLARE EXACTLY THESE TOP-LEVEL SYMBOLS — each ONCE, none omitted, none duplicated:
{{symbols}}

DECIDED NAMES (the naming authority chose these — use verbatim, never invent variants): {{decided_names}}

{{codebase_context}}

{{file_stub}}

{{refine_feedback}}

RULES:
- Emit the ENTIRE file: imports at the top, then every symbol above declared EXACTLY ONCE,
  in natural order (a hook/helper before the component that consumes it). This is ONE cohesive
  file — never fragments, never the same symbol twice.
- A symbol references its siblings DIRECTLY (same module scope). Declare each only once; do not
  re-declare a symbol just because another one uses it.
- A symbol shown as `= <literal>` is a DECIDED constant: reproduce it byte-for-byte.
- If the context shows this file ALREADY EXISTS, emit the COMPLETE merged file (existing content
  PRESERVED + your additions) — never a diff, never a fragment that assumes preceding lines.
- STATE HYDRATION: storage-initialized state uses the lazy initializer useState(() => read(...)),
  never useState(default) + useEffect-overwrite.
- Use ONLY the stack shown in the context — never import a library it does not list.
- Never leave a TODO/placeholder body.
- If a PREVIOUS FAILURE is shown it OVERRIDES every rule above — fix exactly what it describes.

Output ONLY the raw file source — no prose, no fences, no JSON, no field names.
CRITICAL: your VERY FIRST character must be the first character of the file (an `import`, a
comment, or a declaration). Do NOT write "Analyze the request", a numbered plan, a "Goal:", or
ANY reasoning — a single line of analysis wastes the token budget and the reply is discarded as
truncated. Emit the code immediately."##;

fn output_fields() -> Value { json!([{ "field_name": "snippet_json", "field_type": "string" }]) }

// ── prompt ensure (idempotent register-or-skip, mirrors ensureSnippetPrompt/ensureFilePrompt) ──
fn ensure_snippet_prompt(db: &Connection) -> Result<(), String> {
    let existing = crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT }), db).ok();
    let up_to_date = existing.as_ref().map_or(false, |e| {
        e.get("template_md").and_then(Value::as_str) == Some(TEMPLATE)
            && e.get("max_output_tokens").and_then(Value::as_i64) == Some(4000)
    });
    if up_to_date { return Ok(()); }
    crate::ai_prompt_add::promptAdd(&json!({
        "name": PROMPT, "template_md": TEMPLATE, "system_md": SYSTEM,
        "default_tier": "low", "default_temperature": "cold", "phase_flow": "single_only",
        "max_output_tokens": 4000, "output_fields": output_fields(),
    }), db)?;
    Ok(())
}
fn ensure_file_prompt(db: &Connection) -> Result<(), String> {
    let existing = crate::ai_prompt_find::promptFind(&json!({ "name": FILE_PROMPT }), db).ok();
    let up_to_date = existing.as_ref().map_or(false, |e| {
        e.get("template_md").and_then(Value::as_str) == Some(FILE_TEMPLATE)
            && e.get("max_output_tokens").and_then(Value::as_i64) == Some(4000)
    });
    if up_to_date { return Ok(()); }
    crate::ai_prompt_add::promptAdd(&json!({
        "name": FILE_PROMPT, "template_md": FILE_TEMPLATE, "system_md": SYSTEM,
        "default_tier": "low", "default_temperature": "cold", "phase_flow": "single_only",
        "max_output_tokens": 4000, "output_fields": output_fields(),
    }), db)?;
    Ok(())
}

// ── small db-backed lookups ─────────────────────────────────────────────────────────────────
// JS `db.prepare(...).get(slug)?.card_slug ?? slug` — `??` only falls back on null/undefined.
fn card_slug_for(db: &Connection, slug_v: &Value, fallback: &str) -> String {
    let rows = crate::db_json::query_rows(db, "SELECT card_slug FROM contracts WHERE slug = ?", &[slug_v.clone()])
        .unwrap_or(json!([]));
    match rows.as_array().and_then(|a| a.first()).and_then(|r| r.get("card_slug")).cloned() {
        None | Some(Value::Null) => fallback.to_string(),
        Some(v) => str_qq(Some(&v)),
    }
}

// FEAT-NAMING-AUTHORITY: the contract's decided identifier set.
fn decided_names_for(db: &Connection, slug_v: &Value) -> String {
    (|| -> Option<String> {
        let rows = crate::db_json::query_rows(db, "SELECT naming_json FROM contracts WHERE slug = ?", &[slug_v.clone()]).ok()?;
        let nj = rows.as_array()?.first()?.get("naming_json").cloned();
        let nj_str = match nj {
            None | Some(Value::Null) => "{}".to_string(),
            Some(Value::String(s)) => s,
            Some(other) => other.to_string(),
        };
        let n: Value = serde_json::from_str(&nj_str).ok()?;
        if n.is_object() && !n.as_object().unwrap().is_empty() { serde_json::to_string(&n).ok() } else { None }
    })().unwrap_or_else(|| "(none decided)".to_string())
}

// FIX-V2-REAUTHOR-FEEDBACK + FIX-V2-BUILD-GATE-FEEDBACK + TEST-GATE FEEDBACK: the three sources
// a re-author must see WHY the last attempt was judged/built/tested RED. Ported as one function
// (independently unit-testable) rather than inline — the JS source keeps each block inline but
// none of the three depend on each other beyond string concatenation order.
fn refine_feedback_for(db: &Connection, slug_v: &Value, slug: &str, version: i64) -> String {
    let mut refine_feedback = String::new();

    let judgment_block = (|| -> Option<String> {
        let card_slug = card_slug_for(db, slug_v, slug);
        let scope_ref = format!("snippets-v{}", version);
        let rows = crate::db_json::query_rows(db,
            "SELECT judgment_id, goal_line, verdict, updated_at FROM judgments.judgment_cases WHERE card_slug = ? AND scope_kind = 'blueprint' AND scope_ref = ? LIMIT 1",
            &[json!(card_slug), json!(scope_ref)]).ok()?;
        let jc = rows.as_array()?.first()?.clone();
        if jc.get("verdict").and_then(Value::as_str) != Some("fail") { return None; }
        let judgment_id = jc.get("judgment_id").cloned().unwrap_or(Value::Null);
        let items = crate::db_json::query_rows(db,
            "SELECT statement, judge_why FROM judgments.judgment_items WHERE judgment_id = ? AND side = 'prosecutor' AND judge_value = 1",
            &[judgment_id]).ok()?;
        let sustained = items.as_array().cloned().unwrap_or_default();
        // DEFECTS FIRST, acceptance context LAST (r19): whole line kept per item (capped), goal
        // rides last and is the only piece truncated by the outer 3400-char cap.
        let defect_lines: Vec<String> = sustained.iter().take(8).map(|s| {
            let stmt = str_qq(s.get("statement"));
            let why = match s.get("judge_why") { Some(v) if truthy(Some(v)) => format!(" ({})", str_qq(Some(v))), _ => String::new() };
            let line = format!("- {}{}", stmt, why);
            if js_len(&line) > 400 { format!("{}…", js_slice(&line, 400)) } else { line }
        }).collect();
        let goal = str_qq(jc.get("goal_line")).trim().to_string();
        let lintish = defect_lines.is_empty() && !goal.is_empty();
        let goal_line = if lintish {
            format!(
                "- DETERMINISTIC GATE FAILURE (no court ran — this IS the defect; fix it literally): {}",
                if js_len(&goal) > 700 { format!("{}…", js_slice(&goal, 700)) } else { goal.clone() }
            )
        } else if !goal.is_empty() {
            format!("judged against: {}", if js_len(&goal) > 900 { format!("{}…", js_slice(&goal, 900)) } else { goal.clone() })
        } else {
            String::new()
        };
        let mut lines = defect_lines;
        if !goal_line.is_empty() { lines.push(goal_line); }
        if lines.is_empty() { return None; }
        let updated_at = match jc.get("updated_at") { Some(v) if truthy(Some(v)) => str_qq(Some(v)), _ => "unknown".to_string() };
        let block = format!("=== PREVIOUS FAILURE (judged {}) — your last code was judged RED; fix exactly this ===\n{}", updated_at, lines.join("\n"));
        Some(js_slice(&block, 3400))
    })();
    if let Some(b) = judgment_block { refine_feedback = b; }

    let build_block = (|| -> Option<String> {
        let rows = crate::db_json::query_rows(db, "SELECT id, passed, evidence_md FROM build_gate_runs WHERE contract_slug = ? ORDER BY id DESC LIMIT 1", &[slug_v.clone()]).ok()?;
        let bg = rows.as_array()?.first()?.clone();
        if bg.get("passed").and_then(Value::as_i64) != Some(0) { return None; }
        let evidence = bg.get("evidence_md").cloned().unwrap_or(Value::Null);
        if !truthy(Some(&evidence)) { return None; }
        // ROUND SALT: the prior-author-call count grows on every call (cached included), so every
        // retry is a genuinely new prompt by construction — recomputed, never cached across calls.
        let round = (|| -> Option<i64> {
            let card_slug2 = card_slug_for(db, slug_v, slug);
            let r = crate::db_json::query_rows(db,
                "SELECT COUNT(*) AS n FROM brain.ai_calls a JOIN brain.prompt_templates p ON p.id = a.prompt_template_id WHERE p.name LIKE 'blueprint.snippet.author%' AND a.session_id LIKE 'card-' || ? || '-%'",
                &[json!(card_slug2)]).ok()?;
            r.as_array()?.first()?.get("n").and_then(Value::as_i64)
        })().unwrap_or(0);
        let ev = str_qq(Some(&evidence));
        let ev_tail = if js_len(&ev) > 2200 { format!("…{}", js_slice_tail(&ev, 2200)) } else { ev };
        Some(format!(
            "=== PREVIOUS BUILD FAILED (gate run #{} · re-author round {}) — the code must fix this real compiler/build output ===\n{}",
            num_str(bg.get("id")), round + 1, ev_tail
        ))
    })();
    if let Some(b) = build_block {
        refine_feedback = if refine_feedback.is_empty() { b } else { format!("{refine_feedback}\n\n{b}") };
    }

    let test_block = (|| -> Option<String> {
        let rows = crate::db_json::query_rows(db, "SELECT id, passed, evidence_md FROM test_gate_runs WHERE contract_slug = ? ORDER BY id DESC LIMIT 1", &[slug_v.clone()]).ok()?;
        let tg = rows.as_array()?.first()?.clone();
        if tg.get("passed").and_then(Value::as_i64) != Some(0) { return None; }
        let evidence = tg.get("evidence_md").cloned().unwrap_or(Value::Null);
        if !truthy(Some(&evidence)) { return None; }
        let ev = str_qq(Some(&evidence));
        let ev_tail = if js_len(&ev) > 2200 { format!("…{}", js_slice_tail(&ev, 2200)) } else { ev };
        Some(format!(
            "=== PREVIOUS TEST RUN FAILED (test gate run #{}) — the code must make these tests pass ===\n{}",
            num_str(tg.get("id")), ev_tail
        ))
    })();
    if let Some(b) = test_block {
        refine_feedback = if refine_feedback.is_empty() { b } else { format!("{refine_feedback}\n\n{b}") };
    }

    refine_feedback
}

// ── lint snippets: deterministic + VISIBLE (self-lint on top of snippetLint's shape gate) ──────
fn merge_conflict_re() -> &'static regex::Regex {
    static R: OnceLock<regex::Regex> = OnceLock::new();
    R.get_or_init(|| regex::Regex::new(r"(?m)^(<{7}|={7}|>{7})").unwrap())
}
fn fence_start_re() -> &'static regex::Regex {
    static R: OnceLock<regex::Regex> = OnceLock::new();
    R.get_or_init(|| regex::Regex::new(r"(?m)^```").unwrap())
}
fn todo_word_re() -> &'static regex::Regex {
    static R: OnceLock<regex::Regex> = OnceLock::new();
    R.get_or_init(|| regex::Regex::new(r"\bTODO\b").unwrap())
}
fn lint_snippet(code: &Value, path: &Value) -> Option<String> {
    let s = str_qq(Some(code));
    let mut v: Vec<&str> = Vec::new();
    if merge_conflict_re().is_match(&s) { v.push("merge-conflict markers"); }
    if fence_start_re().is_match(&s) { v.push("markdown code fence inside source"); }
    if s.trim().is_empty() { v.push("empty file"); }
    if todo_word_re().is_match(&s) && js_len(&s) < 400 { v.push("TODO-only body"); }
    if v.is_empty() { None } else { Some(format!("{}: {}", str_qq(Some(path)), v.join(", "))) }
}
fn lint_log(db: &Connection, slug_v: &Value, slug: &str, violations: &[String]) {
    let ts = crate::clock::now_iso_nodb();
    let sess = format!("card-{}", card_slug_for(db, slug_v, slug));
    if violations.is_empty() {
        println!("[gate lint.snippets] {ts} session={sess} clean");
    } else {
        println!("[gate lint.snippets] {ts} session={sess} VIOLATIONS: {}", violations.join(" · "));
    }
}

// ── response shaping over a REAL aiCall success (structurally reachable, never exercised here —
// aiCall's twin always Errs; see module doc). Pure — directly unit-tested below. ─────────────────
fn fenced_json_re() -> &'static regex::Regex {
    static R: OnceLock<regex::Regex> = OnceLock::new();
    R.get_or_init(|| regex::Regex::new(r"(?is)```(?:json)?\s*([\s\S]*?)```").unwrap())
}
fn brace_re() -> &'static regex::Regex {
    static R: OnceLock<regex::Regex> = OnceLock::new();
    R.get_or_init(|| regex::Regex::new(r"(?s)\{[\s\S]*\}").unwrap())
}
fn parse_loose(s: &Value) -> Value {
    if s.is_null() { return Value::Null; }
    let str_v = str_qq(Some(s));
    let fenced = fenced_json_re().captures(&str_v).map(|c| c.get(1).map(|m| m.as_str().to_string()).unwrap_or_default());
    let braced = brace_re().find(&str_v).map(|m| m.as_str().to_string());
    for c in [fenced, Some(str_v.clone()), braced] {
        if let Some(cs) = c {
            if let Ok(v) = serde_json::from_str::<Value>(cs.trim()) { return v; }
        }
    }
    Value::Null
}
fn pick_snippet(r: &Value) -> Value {
    if let Some(a) = r.get("output_kv").and_then(Value::as_array) {
        for o in a {
            if o.get("field_name").and_then(Value::as_str) == Some("snippet_json")
                && o.get("parent_output_id").map_or(true, Value::is_null)
            {
                return o.get("value_text").cloned().unwrap_or(Value::Null);
            }
        }
    }
    Value::Null
}
// WHOLE-FILE runAuthorCall's accept branch: the reply IS the complete file, kept VERBATIM.
fn whole_file_from_response(res: &Value, db: &Connection) -> Option<Value> {
    let raw_out = pick_snippet(res);
    let parsed = parse_loose(&raw_out);
    let code = if let Some(pc) = parsed.get("code").and_then(Value::as_str).filter(|c| !c.trim().is_empty()) {
        let imps: Vec<String> = parsed.get("imports").and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|i| { let t = str_qq(Some(i)).trim().to_string(); if t.is_empty() { None } else { Some(t) } }).collect())
            .unwrap_or_default();
        if imps.is_empty() { pc.to_string() } else { format!("{}\n\n{}", imps.join("\n"), pc) }
    } else {
        str_qq(Some(&crate::blueprint__helpers_code_checks::extractRawCode(&raw_out)))
    };
    let accept = crate::blueprint__helpers_author_accept::acceptAuthoredCode(db, &json!({
        "code": code, "path": Value::Null, "ai_call_id": res.get("ai_call_id").cloned().unwrap_or(Value::Null),
    }));
    if accept.get("ok").and_then(Value::as_bool) == Some(true) { Some(json!({ "code": code, "imports": [] })) } else { None }
}
// PER-LEAF response shaping: legacy JSON reply, else deterministic split + degenerate-source guard.
fn leaf_from_response(res: &Value) -> Option<Value> {
    let raw_out = pick_snippet(res);
    let parsed = parse_loose(&raw_out);
    if let Some(pc) = parsed.get("code").and_then(Value::as_str).filter(|c| !c.trim().is_empty()) {
        let imports = parsed.get("imports").and_then(Value::as_array).cloned().unwrap_or_default();
        let exports = parsed.get("exports").and_then(Value::as_array).cloned().unwrap_or_default();
        return Some(json!({ "code": pc, "imports": imports, "exports": exports }));
    }
    let art = crate::blueprint__helpers_code_checks::splitCodeArtifact(&raw_out);
    let code = art.get("code").and_then(Value::as_str).unwrap_or("").to_string();
    let imports_joined = art.get("imports").and_then(Value::as_array)
        .map(|a| a.iter().map(|v| str_qq(Some(v))).collect::<Vec<_>>().join("\n")).unwrap_or_default();
    let combined = format!("{imports_joined}\n{code}");
    if !code.trim().is_empty() && !truthy(Some(&crate::blueprint__helpers_code_checks::isDegenerateSource(&json!(combined)))) {
        Some(art)
    } else {
        None
    }
}

// ── shape helpers ────────────────────────────────────────────────────────────────────────────
fn is_leaf(e: &Value) -> bool {
    match e.get("children") { Some(Value::Array(a)) => a.is_empty(), _ => true }
}
fn is_authorable_const(block: &Value) -> bool {
    block.get("type").and_then(Value::as_str) == Some("const") && str_qq(block.get("signature")).trim().starts_with('=')
}
fn bindings_field(bindings: &Value, key: &str, id: &str, field: &str) -> Value {
    let out: Vec<Value> = bindings.as_array().cloned().unwrap_or_default().iter()
        .filter(|b| str_qq(b.get(key)) == id)
        .filter_map(|b| b.get(field).cloned())
        .filter(|v| !v.is_null())
        .collect();
    Value::Array(out)
}
fn find_file_block(blocks_arr: &[Value], file_id: &str) -> Value {
    blocks_arr.iter().find(|b| str_qq(b.get("id")) == file_id || str_qq(b.get("path")) == file_id).cloned().unwrap_or(Value::Null)
}
fn ctx_for_file(
    file_id: &Value, file_block: &Value, stub_files: &[Value],
    cache: &mut HashMap<String, (String, String)>, db: &Connection,
) -> (String, String) {
    let key = str_qq(Some(file_id));
    if let Some(v) = cache.get(&key) { return v.clone(); }
    let mut grounding = String::new();
    let intent = file_block.get("intent").cloned().unwrap_or(Value::Null);
    let task = format!("{}\n{}", key, str_qq(Some(&intent)));
    let paths = if key.is_empty() { json!([]) } else { json!([key.clone()]) };
    // grounding is ADDITIVE — a gather failure degrades to the old ungrounded prompt (matches JS
    // try/catch around the contextBundle call).
    if let Ok(bundle) = crate::codebase_context_bundle::contextBundle(&json!({
        "agent": "dev", "phase": "author", "task": task, "paths": paths, "budget": 4000
    }), db) {
        if let Some(md) = bundle.get("bundle_md").and_then(Value::as_str) {
            if !md.is_empty() {
                grounding = format!("{md}\n\n⚠ The above is the REAL stack + existing code. Write code that fits it.");
            }
        }
    }
    let stub_src = stub_files.iter().find(|s| str_qq(s.get("path")) == key).and_then(|s| s.get("source").cloned());
    let stub = match stub_src {
        Some(v) => format!("=== DESIGN STUB (the planned signatures for this file) ===\n{}", js_slice(&str_qq(Some(&v)), 4000)),
        None => String::new(),
    };
    cache.insert(key, (grounding.clone(), stub.clone()));
    (grounding, stub)
}
fn persist_snippet(db: &Connection, blueprint_id: &Value, block_id: &str, code: &str, imports: &Value, exports: &Value, authored_by: &str) -> Result<(), String> {
    crate::db_json::run_write(db,
        &format!("INSERT INTO blueprint_snippets (blueprint_id, block_id, code, imports_json, exports_json, authored_by) VALUES (?, ?, ?, ?, ?, ?) \
         ON CONFLICT(blueprint_id, block_id) DO UPDATE SET code=excluded.code, imports_json=excluded.imports_json, exports_json=excluded.exports_json, authored_at={}", crate::clock::now_sql()),
        &[
            blueprint_id.clone(), json!(block_id), json!(code),
            json!(serde_json::to_string(imports).unwrap_or_else(|_| "[]".into())),
            json!(serde_json::to_string(exports).unwrap_or_else(|_| "[]".into())),
            json!(authored_by),
        ],
    )?;
    Ok(())
}

#[allow(non_snake_case)]
pub fn snippetAuthor(input: &Value, db: &Connection) -> Result<Value, String> {
    let slug_v = input.get("contract_slug").cloned().unwrap_or(Value::Null);
    if !truthy(Some(&slug_v)) { return Err("snippet-author: contract_slug is required".into()); }
    let slug = str_qq(Some(&slug_v));

    println!("[gate snippet.author] {} session=card-{} contract={}", crate::clock::now_iso_nodb(), card_slug_for(db, &slug_v, &slug), slug);

    let bp_rows = crate::db_json::query_rows(db,
        "SELECT id, version, blueprint_json FROM blueprints WHERE contract_slug = ? AND superseded_by IS NULL ORDER BY version DESC, id DESC LIMIT 1",
        &[slug_v.clone()])?;
    let bp_row = match bp_rows.as_array().and_then(|a| a.first()) {
        Some(r) => r.clone(),
        None => return Err(format!("snippet-author: blueprint '{slug}' not found")),
    };
    let bp_id = bp_row.get("id").cloned().unwrap_or(Value::Null);
    let version = bp_row.get("version").and_then(Value::as_i64).unwrap_or(1);

    let raw_bp = crate::engine_json_utils::loadJson(&bp_row.get("blueprint_json").cloned().unwrap_or(Value::Null), &json!({}));
    let bp = crate::blueprint_block_shape::normalizeBlueprint(&raw_bp);
    let planned_nodes = raw_bp.get("planned_nodes").filter(|v| v.is_array()).cloned().unwrap_or(json!([]));
    let blocks = bp.get("blocks").cloned().unwrap_or(json!([]));
    let bindings = bp.get("bindings").cloned().unwrap_or(json!([]));
    let blocks_flat = crate::blueprint_block_shape::flattenBlocks(&blocks);
    let blocks_flat_obj = blocks_flat.as_object().cloned().unwrap_or_default();
    let blocks_arr: Vec<Value> = blocks.as_array().cloned().unwrap_or_default();

    ensure_snippet_prompt(db)?;
    ensure_file_prompt(db)?;

    let decided_names = decided_names_for(db, &slug_v);
    let refine_feedback = refine_feedback_for(db, &slug_v, &slug, version);

    let stub_files: Vec<Value> = raw_bp.get("stub_files").and_then(Value::as_array).cloned().unwrap_or_default();
    let mut file_ctx_cache: HashMap<String, (String, String)> = HashMap::new();

    let mut authored: Vec<Value> = Vec::new();
    let mut expected: Vec<Value> = Vec::new();

    // ── WHOLE-FILE AUTHORING PRE-PASS (r40) ────────────────────────────────────────────────
    let whole_file_on = std::env::var("ANTILOKI_WHOLE_FILE_AUTHOR").map(|v| v != "off").unwrap_or(true);
    let mut leaves_by_file_order: Vec<String> = Vec::new();
    let mut leaves_by_file: HashMap<String, Vec<(String, Value)>> = HashMap::new();
    for (id, entry) in blocks_flat_obj.iter() {
        let block = entry.get("block").cloned().unwrap_or(Value::Null);
        if !is_leaf(&block) { continue; }
        let file_id_v = entry.get("fileId").cloned().unwrap_or(Value::Null);
        let k = if file_id_v.is_null() { id.clone() } else { str_qq(Some(&file_id_v)) };
        if !leaves_by_file.contains_key(&k) {
            leaves_by_file_order.push(k.clone());
            leaves_by_file.insert(k.clone(), Vec::new());
        }
        leaves_by_file.get_mut(&k).unwrap().push((id.clone(), block));
    }
    let mut whole_file_order: Vec<String> = Vec::new();
    let mut whole_file_ids: HashSet<String> = HashSet::new();
    if whole_file_on {
        for file_id in &leaves_by_file_order {
            let leaves = &leaves_by_file[file_id];
            let authorable = leaves.iter().filter(|(_, b)| !is_authorable_const(b)).count();
            let consts = leaves.iter().filter(|(_, b)| is_authorable_const(b)).count();
            if authorable >= 2 || (authorable == 1 && consts == 0) {
                whole_file_order.push(file_id.clone());
                whole_file_ids.insert(file_id.clone());
            }
        }
    }

    for file_id in &whole_file_order {
        let file_block = find_file_block(&blocks_arr, file_id);
        let leaves = &leaves_by_file[file_id];
        let symbols: Vec<String> = leaves.iter().map(|(id, b)| {
            if is_authorable_const(b) {
                let name = str_qq(b.get("name"));
                let exported = truthy(b.get("exported"));
                format!("- {} (const): declare exactly `{}const {} {}`", name, if exported { "export " } else { "" }, name, str_qq(b.get("signature")).trim())
            } else {
                let name = b.get("name").filter(|v| !v.is_null()).map(|v| str_qq(Some(v))).unwrap_or_else(|| id.clone());
                let btype = b.get("type").and_then(Value::as_str).unwrap_or("function");
                let exported = truthy(b.get("exported"));
                let intent = match b.get("intent") { Some(v) if !v.is_null() => str_qq(Some(v)), _ => "(see file purpose)".to_string() };
                let sig = str_qq(b.get("signature")).trim().to_string();
                let sig_part = if sig.is_empty() { String::new() } else { format!(" [signature: {sig}]") };
                format!("- {} ({}{}): {}{}", name, btype, if exported { ", exported" } else { "" }, intent, sig_part)
            }
        }).collect();
        let symbols_str = symbols.join("\n");

        let (grounding, stub) = ctx_for_file(&json!(file_id.clone()), &file_block, &stub_files, &mut file_ctx_cache, db);
        let kv = json!([
            { "key": "path", "value": file_id },
            { "key": "file_intent", "value": file_block.get("intent").cloned().unwrap_or(json!("")) },
            { "key": "symbols", "value": symbols_str },
            { "key": "decided_names", "value": decided_names },
            { "key": "codebase_context", "value": grounding },
            { "key": "file_stub", "value": stub },
            { "key": "refine_feedback", "value": refine_feedback },
        ]);
        let ai_res = crate::ai_ai_call::aiCall(&json!({
            "prompt_name": FILE_PROMPT, "input_kv": kv, "contract_slug": slug,
            "tier": "low", "temperature": "cold", "phase_flow": "single_only", "structured_output": false,
        }), db);
        let snippet = match ai_res { Ok(res) => whole_file_from_response(&res, db), Err(_) => None };
        let rec = snippet.unwrap_or_else(|| json!({ "code": format!("// TODO({file_id}): author this file"), "imports": [], "exports": [] }));
        let code = rec.get("code").cloned().unwrap_or(json!(""));
        let imports = rec.get("imports").cloned().unwrap_or(json!([]));
        let exports = rec.get("exports").cloned().unwrap_or(json!([]));
        authored.push(json!({ "block_id": file_id, "code": code, "imports": imports, "exports": exports }));
        expected.push(json!(file_id));
        let authored_by = input.get("authored_by").filter(|v| !v.is_null()).cloned().unwrap_or(json!("dev:auto-file"));
        persist_snippet(db, &bp_id, file_id, &str_qq(Some(&code)), &imports, &exports, &str_qq(Some(&authored_by)))?;
    }

    // ── PER-LEAF LOOP (single-leaf files; unchanged) ───────────────────────────────────────
    for (id, entry) in blocks_flat_obj.iter() {
        let block = entry.get("block").cloned().unwrap_or(Value::Null);
        if !is_leaf(&block) { continue; }
        let file_id_v = entry.get("fileId").cloned().unwrap_or(Value::Null);
        let file_id_key = if file_id_v.is_null() { String::new() } else { str_qq(Some(&file_id_v)) };
        if whole_file_ids.contains(&file_id_key) { continue; }
        expected.push(json!(id.clone()));

        let inputs = bindings_field(&bindings, "to", id, "input");
        let outputs = bindings_field(&bindings, "from", id, "output");
        let file_block = blocks_arr.iter().find(|b| b.get("id").cloned().unwrap_or(Value::Null) == file_id_v).cloned().unwrap_or(Value::Null);

        // FEAT-DESIGN-BINDS-IDENTIFIERS: a design-bound CONSTANT is emitted deterministically —
        // byte-for-byte from the design, never re-asked of a model.
        if block.get("type").and_then(Value::as_str) == Some("const")
            && truthy(block.get("name"))
            && str_qq(block.get("signature")).trim().starts_with('=')
        {
            let exported = truthy(block.get("exported"));
            let name = str_qq(block.get("name"));
            let sig = str_qq(block.get("signature")).trim().to_string();
            let code = format!("{}const {} {};", if exported { "export " } else { "" }, name, sig);
            let exports = if exported { json!([name]) } else { json!([]) };
            authored.push(json!({ "block_id": id, "code": code, "imports": [], "exports": exports }));
            persist_snippet(db, &bp_id, id, &code, &json!([]), &exports, "design:const")?;
            continue;
        }

        let (grounding, stub) = ctx_for_file(&file_id_v, &file_block, &stub_files, &mut file_ctx_cache, db);
        let siblings: Vec<String> = blocks_flat_obj.iter()
            .filter(|(sid, e)| e.get("fileId").cloned().unwrap_or(Value::Null) == file_id_v && sid.as_str() != id.as_str() && is_leaf(&e.get("block").cloned().unwrap_or(Value::Null)))
            .filter_map(|(_, e)| e.get("block").and_then(|b| b.get("name")).and_then(Value::as_str).map(str::to_string))
            .collect();
        let siblings_str = if siblings.is_empty() { "(none — this block owns the whole file)".to_string() } else { siblings.join(", ") };

        let kv = json!([
            { "key": "path", "value": file_id_key },
            { "key": "file_intent", "value": file_block.get("intent").cloned().unwrap_or(json!("")) },
            { "key": "type", "value": block.get("type").cloned().unwrap_or(json!("")) },
            { "key": "signature", "value": block.get("signature").cloned().unwrap_or(json!("")) },
            { "key": "intent", "value": block.get("intent").cloned().unwrap_or(json!("")) },
            { "key": "inputs", "value": serde_json::to_string(&inputs).unwrap_or_default() },
            { "key": "outputs", "value": serde_json::to_string(&outputs).unwrap_or_default() },
            { "key": "decided_names", "value": decided_names },
            { "key": "siblings", "value": siblings_str },
            { "key": "codebase_context", "value": grounding },
            { "key": "file_stub", "value": stub },
            { "key": "refine_feedback", "value": refine_feedback },
        ]);
        let ai_res = crate::ai_ai_call::aiCall(&json!({
            "prompt_name": PROMPT, "input_kv": kv, "contract_slug": slug,
            "tier": "low", "temperature": "cold", "phase_flow": "single_only", "structured_output": false,
        }), db);
        let snippet = match ai_res { Ok(res) => leaf_from_response(&res), Err(_) => None };
        let rec = match snippet {
            Some(s) => json!({ "block_id": id, "code": s.get("code").cloned().unwrap_or(json!("")), "imports": s.get("imports").cloned().unwrap_or(json!([])), "exports": s.get("exports").cloned().unwrap_or(json!([])) }),
            None => {
                let intent_txt = match block.get("intent") { None | Some(Value::Null) => "author this block".to_string(), Some(v) => str_qq(Some(v)) };
                json!({ "block_id": id, "code": format!("// TODO({id}): {intent_txt}"), "imports": [], "exports": [] })
            }
        };
        authored.push(rec.clone());
        let authored_by = input.get("authored_by").filter(|v| !v.is_null()).cloned().unwrap_or(json!("dev:auto"));
        persist_snippet(
            db, &bp_id, id,
            &str_qq(rec.get("code")), rec.get("imports").unwrap_or(&json!([])), rec.get("exports").unwrap_or(&json!([])),
            &str_qq(Some(&authored_by)),
        )?;
    }

    let lint = crate::blueprint_snippet_lint::snippetLint(&json!({ "snippets": &authored, "block_ids": &expected }), db)?;
    // persist + log the verdict (non-blocking on error, matches JS's outer try/catch).
    (|| -> Result<(), String> {
        let per_file: Vec<String> = authored.iter().filter_map(|s| lint_snippet(s.get("code").unwrap_or(&Value::Null), s.get("block_id").unwrap_or(&Value::Null))).collect();
        let issues: Vec<String> = lint.get("issues").and_then(Value::as_array)
            .map(|a| a.iter().map(|x| match x { Value::String(s) => s.clone(), o => o.to_string() }).collect()).unwrap_or_default();
        // JS: `Array.isArray(lint?.missing) ? lint.missing : []` — `missing` is a COUNT (int), so
        // this branch is always [] in practice; the generic and_then(as_array) mirrors that.
        let missing_list: Vec<String> = lint.get("missing").and_then(Value::as_array)
            .map(|a| a.iter().map(|m| format!("missing block: {}", match m { Value::String(s) => s.clone(), o => o.to_string() })).collect()).unwrap_or_default();
        let mut all: Vec<String> = Vec::new();
        all.extend(issues);
        all.extend(missing_list);
        all.extend(per_file);
        lint_log(db, &slug_v, &slug, &all);
        let passed = (lint.get("passed").and_then(Value::as_bool) == Some(true)) && all.is_empty();
        let issues_json = serde_json::to_string(&all).map_err(|e| e.to_string())?;
        let missing_json = serde_json::to_string(&lint.get("missing").cloned().unwrap_or(json!([]))).map_err(|e| e.to_string())?;
        crate::db_json::run_write(db,
            "INSERT INTO snippet_lint_runs (contract_slug, blueprint_id, passed, issues_json, missing_json) VALUES (?, ?, ?, ?, ?)",
            &[slug_v.clone(), bp_id.clone(), json!(if passed { 1 } else { 0 }), json!(issues_json), json!(missing_json)])?;
        Ok(())
    })().unwrap_or_else(|e| println!("[gate lint.snippets] persist/log error (non-blocking): {e}"));

    // P3: verify authored files against the design stub — only when the design carried stubs.
    let mut authored_map_obj: serde_json::Map<String, Value> = serde_json::Map::new();
    for s in &authored {
        let bid = str_qq(s.get("block_id"));
        authored_map_obj.insert(bid, json!({ "code": s.get("code").cloned().unwrap_or(json!("")), "imports": s.get("imports").cloned().unwrap_or(json!([])), "exports": s.get("exports").cloned().unwrap_or(json!([])) }));
    }
    let authored_map: Value = Value::Object(authored_map_obj);
    let verify = if planned_nodes.as_array().map(|a| !a.is_empty()).unwrap_or(false) {
        crate::blueprint__helpers_stub_verify::verifyAgainstStub(&blocks, &authored_map, &planned_nodes)
    } else {
        Value::Null
    };

    Ok(json!({
        "contract_slug": slug,
        "blueprint_id": bp_id,
        "authored": authored.len() as i64,
        "expected": expected.len() as i64,
        "lint": lint,
        "verify": verify,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS brain; ATTACH ':memory:' AS judgments; ATTACH ':memory:' AS memory;",
        ).unwrap();
        c.execute_batch(
            "CREATE TABLE contracts (slug TEXT PRIMARY KEY, card_slug TEXT, naming_json TEXT);
             CREATE TABLE blueprints (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT, version INTEGER, blueprint_json TEXT, superseded_by INTEGER);
             CREATE TABLE blueprint_snippets (id INTEGER PRIMARY KEY AUTOINCREMENT, blueprint_id INTEGER, block_id TEXT, code TEXT, imports_json TEXT, exports_json TEXT, authored_by TEXT, authored_at TEXT, UNIQUE(blueprint_id, block_id));
             CREATE TABLE build_gate_runs (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT, passed INTEGER, evidence_md TEXT);
             CREATE TABLE test_gate_runs (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT, passed INTEGER, evidence_md TEXT);
             CREATE TABLE snippet_lint_runs (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT, blueprint_id INTEGER, passed INTEGER, issues_json TEXT, missing_json TEXT);
             CREATE TABLE judgments.judgment_cases (judgment_id TEXT PRIMARY KEY, card_slug TEXT, scope_kind TEXT, scope_ref TEXT, goal_line TEXT, verdict TEXT, updated_at TEXT);
             CREATE TABLE judgments.judgment_items (id INTEGER PRIMARY KEY AUTOINCREMENT, judgment_id TEXT, side TEXT, statement TEXT, judge_why TEXT, judge_value INTEGER);
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.ai_calls (id INTEGER PRIMARY KEY AUTOINCREMENT, prompt_template_id INTEGER, session_id TEXT, tokens_out INTEGER, cached INTEGER DEFAULT 0);
             -- codebase-context-bundle deps (kept empty; grounding degrades to '' on any read miss)
             CREATE TABLE workspaces (id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE, root_path TEXT NOT NULL, vcs TEXT NOT NULL DEFAULT 'git', is_active INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00');
             CREATE TABLE code_nodes (id TEXT PRIMARY KEY, origin TEXT, path TEXT, type TEXT, name TEXT, signature TEXT, intent TEXT, parent_id TEXT);
             CREATE TABLE code_edges (id INTEGER PRIMARY KEY AUTOINCREMENT, origin TEXT, from_node TEXT, to_node TEXT, type TEXT, kind TEXT);
             CREATE TABLE code_decorators (origin TEXT, path TEXT, name TEXT, kind TEXT, signature TEXT, summary TEXT, line INTEGER);
             CREATE TABLE code_entry_points (id INTEGER PRIMARY KEY AUTOINCREMENT, origin TEXT, kind TEXT, subtype TEXT, path TEXT, label TEXT, line INTEGER, framework TEXT, confidence REAL DEFAULT 0.7);
             CREATE TABLE code_data_stories (id INTEGER PRIMARY KEY, origin TEXT, name TEXT, description TEXT, hops_json TEXT);
             CREATE TABLE code_digests (id INTEGER PRIMARY KEY, origin TEXT, narrative_md TEXT);
             CREATE TABLE memory.memories (id INTEGER PRIMARY KEY, type TEXT, title TEXT, guidance TEXT, validated INTEGER, superseded_by INTEGER);",
        ).unwrap();
        c.execute("INSERT INTO workspaces (name, root_path, is_active) VALUES ('w', '.', 1)", []).unwrap();
        c
    }

    fn a_blueprint(c: &Connection, slug: &str, blueprint_json: &str) -> i64 {
        c.execute("INSERT INTO contracts (slug, card_slug) VALUES (?, ?)", rusqlite::params![slug, slug]).unwrap();
        c.execute("INSERT INTO blueprints (contract_slug, version, blueprint_json) VALUES (?, 1, ?)", rusqlite::params![slug, blueprint_json]).unwrap();
        c.last_insert_rowid()
    }

    #[test]
    fn contract_slug_is_required() {
        let c = setup();
        let e = snippetAuthor(&json!({}), &c).unwrap_err();
        assert_eq!(e, "snippet-author: contract_slug is required");
    }

    #[test]
    fn missing_blueprint_errors() {
        let c = setup();
        let e = snippetAuthor(&json!({ "contract_slug": "ghost" }), &c).unwrap_err();
        assert_eq!(e, "snippet-author: blueprint 'ghost' not found");
    }

    #[test]
    fn design_bound_const_is_emitted_byte_for_byte_without_calling_ai() {
        let c = setup();
        let bp = json!({
            "blocks": [{ "id": "f.js", "type": "file", "path": "f.js", "intent": "the file",
                "children": [{ "id": "f.js#K", "type": "const", "name": "STORAGE_KEY", "exported": true, "signature": "= 'antiloki:x'", "children": [] }] }],
            "bindings": [],
        });
        a_blueprint(&c, "CT-1", &bp.to_string());
        let r = snippetAuthor(&json!({ "contract_slug": "CT-1" }), &c).unwrap();
        assert_eq!(r["authored"], json!(1));
        assert_eq!(r["expected"], json!(1));
        let code: String = c.query_row("SELECT code FROM blueprint_snippets WHERE block_id = 'f.js#K'", [], |row| row.get(0)).unwrap();
        assert_eq!(code, "export const STORAGE_KEY = 'antiloki:x';");
        let authored_by: String = c.query_row("SELECT authored_by FROM blueprint_snippets WHERE block_id = 'f.js#K'", [], |row| row.get(0)).unwrap();
        assert_eq!(authored_by, "design:const");
    }

    #[test]
    fn single_function_leaf_falls_back_to_a_todo_stub_since_aicall_is_deferred() {
        // reader side of ANTILOKI_WHOLE_FILE_AUTHOR — see the sibling test below.
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        let c = setup();
        let bp = json!({
            "blocks": [{ "id": "f.js", "type": "file", "path": "f.js", "intent": "the file",
                "children": [{ "id": "f.js#fn", "type": "function", "name": "add", "intent": "adds two numbers", "signature": "(a, b)", "exported": true, "children": [] }] }],
            "bindings": [],
        });
        a_blueprint(&c, "CT-2", &bp.to_string());
        let r = snippetAuthor(&json!({ "contract_slug": "CT-2" }), &c).unwrap();
        assert_eq!(r["authored"], json!(1));
        assert_eq!(r["expected"], json!(1));
        // A single authorable leaf with NO design-bound const falls into whole-file authoring
        // (JS snippet-author.mjs lines 332-338, the r41 barrel rule), so the row lands under the
        // FILE block_id ("f.js"), not the leaf ("f.js#fn"), with authored_by "dev:auto-file".
        // Verified against real JS: authorable.length===1 && consts.length===0 => wholeFileIds
        // includes "f.js"; the per-leaf loop then skips "f.js#fn" via `wholeFileIds.has(fileId)`.
        let code: String = c.query_row("SELECT code FROM blueprint_snippets WHERE block_id = 'f.js'", [], |row| row.get(0)).unwrap();
        assert_eq!(code, "// TODO(f.js): author this file");
        let authored_by: String = c.query_row("SELECT authored_by FROM blueprint_snippets WHERE block_id = 'f.js'", [], |row| row.get(0)).unwrap();
        assert_eq!(authored_by, "dev:auto-file");
        // lint marks the TODO-only body a violation but the run is not blocked by it.
        let passed: i64 = c.query_row("SELECT passed FROM snippet_lint_runs WHERE contract_slug = 'CT-2'", [], |row| row.get(0)).unwrap();
        assert_eq!(passed, 0);
        let issues: String = c.query_row("SELECT issues_json FROM snippet_lint_runs WHERE contract_slug = 'CT-2'", [], |row| row.get(0)).unwrap();
        assert!(issues.contains("TODO-only body"), "{issues}");
    }

    #[test]
    fn two_authorable_leaves_in_one_file_author_as_a_single_whole_file_unit() {
        // reader side of ANTILOKI_WHOLE_FILE_AUTHOR: asserts the DEFAULT 'on' behavior, so it must
        // not run inside the sibling test's 'off' window. Guarding only the writer is not enough.
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        let c = setup();
        let bp = json!({
            "blocks": [{ "id": "f.js", "type": "file", "path": "f.js", "intent": "the file", "children": [
                { "id": "f.js#a", "type": "function", "name": "a", "children": [] },
                { "id": "f.js#b", "type": "function", "name": "b", "children": [] },
            ] }],
            "bindings": [],
        });
        a_blueprint(&c, "CT-3", &bp.to_string());
        let r = snippetAuthor(&json!({ "contract_slug": "CT-3" }), &c).unwrap();
        // whole-file: ONE authored/expected entry keyed by the file, not one per leaf.
        assert_eq!(r["authored"], json!(1));
        assert_eq!(r["expected"], json!(1));
        let code: String = c.query_row("SELECT code FROM blueprint_snippets WHERE block_id = 'f.js'", [], |row| row.get(0)).unwrap();
        assert_eq!(code, "// TODO(f.js): author this file");
        let n: i64 = c.query_row("SELECT COUNT(*) FROM blueprint_snippets WHERE block_id IN ('f.js#a','f.js#b')", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 0, "per-leaf rows must NOT be written when the file authors as a whole unit");
    }

    #[test]
    fn whole_file_authoring_can_be_disabled_via_env_flag() {
        // ENV_LOCK, not "own db/state": the private fixture was never the hazard. The two tests
        // above read this same process-global var (via snippetAuthor) and assert the DEFAULT 'on'
        // behavior, so this 'off' window flips them whenever the threads interleave.
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        std::env::set_var("ANTILOKI_WHOLE_FILE_AUTHOR", "off");
        let c = setup();
        let bp = json!({
            "blocks": [{ "id": "f.js", "type": "file", "path": "f.js", "children": [
                { "id": "f.js#a", "type": "function", "name": "a", "children": [] },
                { "id": "f.js#b", "type": "function", "name": "b", "children": [] },
            ] }],
            "bindings": [],
        });
        a_blueprint(&c, "CT-4", &bp.to_string());
        let r = snippetAuthor(&json!({ "contract_slug": "CT-4" }), &c).unwrap();
        std::env::remove_var("ANTILOKI_WHOLE_FILE_AUTHOR");
        assert_eq!(r["authored"], json!(2));
        assert_eq!(r["expected"], json!(2));
    }

    #[test]
    fn verify_against_stub_is_null_without_planned_nodes_and_populated_with_them() {
        let c = setup();
        let bp_none = json!({ "blocks": [{ "id": "f.js", "type": "file", "path": "f.js", "children": [
            { "id": "f.js#a", "type": "function", "name": "a", "children": [] } ] }], "bindings": [] });
        a_blueprint(&c, "CT-5", &bp_none.to_string());
        let r = snippetAuthor(&json!({ "contract_slug": "CT-5" }), &c).unwrap();
        assert_eq!(r["verify"], Value::Null);

        let c2 = setup();
        let bp_planned = json!({
            "blocks": [{ "id": "f.js", "type": "file", "path": "f.js", "children": [
                { "id": "f.js#a", "type": "function", "name": "a", "children": [] } ] }],
            "bindings": [],
            "planned_nodes": [{ "type": "function", "path": "f.js", "name": "a", "signature": "()" }],
        });
        a_blueprint(&c2, "CT-6", &bp_planned.to_string());
        let r2 = snippetAuthor(&json!({ "contract_slug": "CT-6" }), &c2).unwrap();
        // TODO stub `// TODO(f.js#a): author this block` does not parse as a function `a` →
        // composed source doesn't satisfy the planned signature → drift, not a hard fail.
        assert_ne!(r2["verify"], Value::Null);
        assert_eq!(r2["verify"]["ok"], json!(true));
    }

    #[test]
    fn refine_feedback_for_assembles_judgment_then_build_then_test_blocks_in_order() {
        let c = setup();
        c.execute("INSERT INTO contracts (slug, card_slug) VALUES ('CT-7', 'CARD-7')", []).unwrap();
        c.execute(
            "INSERT INTO judgments.judgment_cases (judgment_id, card_slug, scope_kind, scope_ref, goal_line, verdict, updated_at) VALUES ('j1', 'CARD-7', 'blueprint', 'snippets-v1', 'must render X', 'fail', '2026-01-01 00:00:00')",
            [],
        ).unwrap();
        c.execute(
            "INSERT INTO judgments.judgment_items (judgment_id, side, statement, judge_why, judge_value) VALUES ('j1', 'prosecutor', 'missing export', 'no export found', 1)",
            [],
        ).unwrap();
        c.execute("INSERT INTO build_gate_runs (contract_slug, passed, evidence_md) VALUES ('CT-7', 0, 'compile error: missing semicolon')", []).unwrap();
        c.execute("INSERT INTO test_gate_runs (contract_slug, passed, evidence_md) VALUES ('CT-7', 0, 'FAIL: expected 2 got 1')", []).unwrap();

        let fb = refine_feedback_for(&c, &json!("CT-7"), "CT-7", 1);
        assert!(fb.contains("PREVIOUS FAILURE"), "{fb}");
        assert!(fb.contains("missing export"), "{fb}");
        assert!(fb.contains("PREVIOUS BUILD FAILED"), "{fb}");
        assert!(fb.contains("missing semicolon"), "{fb}");
        assert!(fb.contains("PREVIOUS TEST RUN FAILED"), "{fb}");
        assert!(fb.contains("expected 2 got 1"), "{fb}");
        let judgment_pos = fb.find("PREVIOUS FAILURE").unwrap();
        let build_pos = fb.find("PREVIOUS BUILD FAILED").unwrap();
        let test_pos = fb.find("PREVIOUS TEST RUN FAILED").unwrap();
        assert!(judgment_pos < build_pos && build_pos < test_pos);
    }

    #[test]
    fn refine_feedback_for_is_empty_with_no_prior_failures() {
        let c = setup();
        c.execute("INSERT INTO contracts (slug, card_slug) VALUES ('CT-8', 'CARD-8')", []).unwrap();
        assert_eq!(refine_feedback_for(&c, &json!("CT-8"), "CT-8", 1), "");
    }

    #[test]
    fn decided_names_defaults_and_reads_naming_json() {
        let c = setup();
        c.execute("INSERT INTO contracts (slug, card_slug, naming_json) VALUES ('CT-9', 'CT-9', NULL)", []).unwrap();
        assert_eq!(decided_names_for(&c, &json!("CT-9")), "(none decided)");
        c.execute("UPDATE contracts SET naming_json = '{}' WHERE slug = 'CT-9'", []).unwrap();
        assert_eq!(decided_names_for(&c, &json!("CT-9")), "(none decided)");
        c.execute(r#"UPDATE contracts SET naming_json = '{"storage_key":"x"}' WHERE slug = 'CT-9'"#, []).unwrap();
        assert_eq!(decided_names_for(&c, &json!("CT-9")), r#"{"storage_key":"x"}"#);
    }

    #[test]
    fn lint_snippet_detects_each_violation_class() {
        assert_eq!(lint_snippet(&json!(""), &json!("f.js")), Some("f.js: empty file".to_string()));
        assert_eq!(lint_snippet(&json!("<<<<<<< HEAD\nx"), &json!("f.js")), Some("f.js: merge-conflict markers".to_string()));
        assert_eq!(lint_snippet(&json!("```js\nx\n```"), &json!("f.js")), Some("f.js: markdown code fence inside source".to_string()));
        assert_eq!(lint_snippet(&json!("TODO"), &json!("f.js")), Some("f.js: TODO-only body".to_string()));
        assert_eq!(lint_snippet(&json!("export const x = 1;"), &json!("f.js")), None);
    }

    #[test]
    fn parse_loose_extracts_fenced_then_raw_then_braces() {
        assert_eq!(parse_loose(&json!("```json\n{\"code\":\"x\"}\n```")), json!({"code":"x"}));
        assert_eq!(parse_loose(&json!("{\"code\":\"y\"}")), json!({"code":"y"}));
        assert_eq!(parse_loose(&json!("prose then {\"code\":\"z\"} trailing")), json!({"code":"z"}));
        assert_eq!(parse_loose(&json!("no json here")), Value::Null);
        assert_eq!(parse_loose(&Value::Null), Value::Null);
    }

    #[test]
    fn pick_snippet_finds_the_top_level_snippet_json_field() {
        let r = json!({ "output_kv": [
            { "field_name": "other", "value_text": "nope" },
            { "field_name": "snippet_json", "parent_output_id": 5, "value_text": "nested-skip" },
            { "field_name": "snippet_json", "parent_output_id": null, "value_text": "{\"code\":\"ok\"}" },
        ] });
        assert_eq!(pick_snippet(&r), json!("{\"code\":\"ok\"}"));
        assert_eq!(pick_snippet(&json!({})), Value::Null);
    }

    #[test]
    fn is_authorable_const_requires_const_type_and_equals_signature() {
        assert!(is_authorable_const(&json!({ "type": "const", "signature": "= 1" })));
        assert!(!is_authorable_const(&json!({ "type": "const", "signature": "1" })));
        assert!(!is_authorable_const(&json!({ "type": "function", "signature": "= 1" })));
    }
}

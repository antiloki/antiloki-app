//! contract-templates.rs — twin of skills/contract/contract-templates.mjs (no-db, pure — plus a
//! default-export introspector). NAMES ARE CONTRACT: `getTemplate` · `universalOutOfScopeList` ·
//! `noTemplateGoalMd` · `contractTemplates` (default).
//!
//! SCOPE: each of the 6 TEMPLATES[kind] entries carries TWO kinds of fields — 5 function-VALUED
//! ones (files_touched_pattern, naming_from_slug, resolveFiles [3 kinds only], acceptance_baseline,
//! goal_md_baseline; resolveFiles additionally does REAL recursive fs traversal via node:fs) and 7
//! plain, static, JSON-serializable ones (kind, default_size, default_risk, edge_cases_baseline,
//! reference_artifact_hints, must_obey_extra, out_of_scope_extra) that ARE real production data —
//! contract-draft-auto.mjs reads template?.default_risk / default_size / must_obey_extra /
//! out_of_scope_extra / reference_artifact_hints and template.edge_cases_baseline straight into
//! every drafted contract. `template_static()` below ports those 7 fields for real, transcribed
//! field-by-field from the JS source, with full #[cfg(test)] coverage of all 6 kinds plus the
//! unknown-kind case.
//!
//! Neither `getTemplate(kind)` nor `contractTemplates()` can pass a conform-db fixture, though —
//! NOT because the static fields have "no logic value" (the module's ORIGINAL claim, corrected
//! here), but for a narrower, empirically-verified reason: both directly return an object whose
//! closures are enumerable properties on the SAME object as the static fields. The harness's
//! `normalizeResult` (orchestrator/compare.mjs) walks Object.keys() recursively and stringifies a
//! function value via `String(fn)` rather than dropping it — confirmed by running
//! `normalizeResult(getTemplate('law-hook-add'))` for real: the result keeps all 11 keys, 5 of
//! them full V8 source text. `compare.mjs`'s object comparator then requires EXACT key-set
//! equality (`Object.keys(a).sort().join(',') !== Object.keys(b).sort().join(',')`) with no
//! partial/projection mode. A twin that (correctly) omits the unportable closures therefore ALWAYS
//! diverges on keys against the real 11-key (or, for contractTemplates, six nested 11-key) return —
//! independent of how byte-perfect the 7 static fields are. Closing that gap for real would mean
//! transcribing the EXACT V8 source text of up to 30 closures — the same transcription-risk-with-
//! no-logic-value the original reasoning correctly rejected, just for the wrong stated reason (it
//! also rejected the 7 static fields as unportable, which they are not). Both fns therefore stay
//! `class:"transitive"` (a real `pub fn` exists for names_ok, but no fixture/arm) — same precedent
//! as state-machine-rules.rs's `stateMachineRules()`, except here it is the harness's key-set
//! comparator that blocks it, not merely "no consumer reads the raw shape".
//!
//! `universalOutOfScopeList()` (static string array) and `noTemplateGoalMd(card)` (the no-template
//! fallback goal_md builder — pure string composition over `extractBodySection`/`section`/
//! `substrateInvariants`) are ported for real, with full fixture coverage.
use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
// JS String(v) — used to stringify a JSON.parse'd non-object/array body before the regex text-scan.
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

// extractBodySection(card, ...keys): card.body_json, JSON.parse'd when it's a string (kept as the
// original string on parse failure). An object result is key-scanned for the first non-empty
// string field; anything else (array, or a parse failure that stayed a string, or a JSON scalar
// like null/number) falls back to a `KEY: value` prose-marker regex scan over String(body).
fn extract_body_section(card: &Value, keys: &[&str]) -> Option<String> {
    let body_json = card.get("body_json");
    if !truthy(body_json) {
        return None;
    }
    let body_json = body_json.unwrap();
    let parsed: Value = match body_json {
        Value::String(s) => serde_json::from_str::<Value>(s).unwrap_or_else(|_| Value::String(s.clone())),
        other => other.clone(),
    };
    match &parsed {
        Value::Object(map) => {
            for k in keys {
                if let Some(Value::String(s)) = map.get(*k) {
                    let t = s.trim();
                    if !t.is_empty() {
                        return Some(t.to_string());
                    }
                }
            }
            None
        }
        Value::Array(_) => None, // typeof [] === 'object' in JS but no string key ever matches
        _ => {
            let text = js_string(&parsed);
            for k in keys {
                let pat = format!(r"(?is){k}\s*:\s*(.+?)(?:\n\n|\n[A-Z][A-Z_ ]*:|$)");
                if let Ok(re) = Regex::new(&pat) {
                    if let Some(caps) = re.captures(&text) {
                        return Some(caps[1].trim().to_string());
                    }
                }
            }
            None
        }
    }
}

fn section(heading: &str, content: Option<&str>) -> String {
    match content {
        None => String::new(),
        Some(c) if c.is_empty() => String::new(),
        Some(c) => format!("## {heading}\n{c}\n\n"),
    }
}

fn substrate_invariants() -> String {
    [
        "- All mutations of cards/agents/actions/memory/laws go through `with-audit` (NEVER_BYPASS_WITH_AUDIT).",
        "- Skills must be invoked via actions, not imported directly outside skill composition (LONE_SKILL_DISPATCH_FORBIDDEN).",
        "- Every \"I built X\" claim needs a verification command in the same response (CLAIM_REQUIRES_VERIFICATION).",
        "- Read the existing file before editing (READ_BEFORE_WRITING). Match the file's own conventions.",
    ]
    .join("\n")
}

pub fn universalOutOfScopeList() -> Value {
    json!([
        "no formatting/lint changes outside files_touched",
        "no rename of unrelated identifiers",
        "no test-suite reorganization",
        "no dependency upgrades unrelated to the contract",
        "no comment cleanup of pre-existing code",
    ])
}

pub fn noTemplateGoalMd(card: &Value) -> Value {
    let motivation = extract_body_section(card, &["motivation", "why", "diagnosis"]);
    let fix = extract_body_section(card, &["fix", "strategy", "plan"]);

    let mission = format!(
        "{}. (No template classified this card — TL must fill the structural fields below before DEV can act.)",
        tmpl(card.get("title"))
    );
    let background = motivation.unwrap_or_else(|| "This card didn't match any built-in template heuristic. The contract is a stub: `files_touched` is the `<UNKNOWN_PATHS_TL_REVIEW>` marker, acceptance and naming are empty, and DEV should NOT write code yet.".to_string());
    let before_dev = "- `files_touched` — real file paths (no placeholders).\n\
- `acceptance_json` — at least one test or diff check.\n\
- `naming_json` — function names, file basenames where applicable.\n\
- `must_obey_json` — laws this work must respect (`system.law.find` by topic).\n\
- `template_kind` — pick the closest match, or explicitly mark as bespoke.";
    let risk = "- Scope creep: the card may secretly be 3 cards. Watch for \"and also...\" in the body.\n\
- Hidden dependencies: the body may mention modules the AI hasn't read.\n\
- Phantom requirements: \"should support X eventually\" belongs in OUT-OF-SCOPE, not the implementation.";
    let open_q = "- Almost certainly several. Surface them in the commit message rather than guessing.\n\
- If a clarifying call to TL would unblock you for hours of work — make the call.";
    let patterns = "None known. Read the closest substrate skill by topic and model after it.";
    let invariants = substrate_invariants();

    let mut out = String::new();
    out.push_str(&section("Mission", Some(&mission)));
    out.push_str(&section("Background", Some(&background)));
    out.push_str(&section("Strategy hints from the card", fix.as_deref()));
    out.push_str(&section("Before DEV proceeds, TL must fill", Some(before_dev)));
    out.push_str(&section("Risk profile", Some(risk)));
    out.push_str(&section("Open questions for DEV judgment", Some(open_q)));
    out.push_str(&section("Patterns to emulate", Some(patterns)));
    out.push_str(&section("Substrate invariants you must respect", Some(&invariants)));
    json!(out.trim())
}

const TEMPLATE_KINDS: [&str; 6] =
    ["law-hook-add", "skill-modify", "skill-add", "action-add", "migration-add", "render-html-add"];

// The 7 static, JSON-serializable fields of a TEMPLATES[kind] entry — see module doc comment for
// why this is a deliberate projection (kind/default_size/default_risk/edge_cases_baseline/
// reference_artifact_hints/must_obey_extra/out_of_scope_extra), never the raw entry. Key order
// matches the entries' own insertion order in the JS object literal with the function-valued keys
// (files_touched_pattern, naming_from_slug, resolveFiles, acceptance_baseline, goal_md_baseline)
// removed. Returns None for a kind absent from TEMPLATES (JS: `TEMPLATES[kind] ?? null`).
fn template_static(kind: &str) -> Option<Value> {
    match kind {
        "law-hook-add" => Some(json!({
            "kind": "law-hook-add",
            "default_size": "S",
            "default_risk": 3,
            "edge_cases_baseline": [
                {"when": "action_name is null or empty", "then": "return { allowed: true } (no-op for unrelated dispatches)"},
                {"when": "input_data is null", "then": "treat as empty, do not throw"},
                {"when": "escape_hatch env var is set", "then": "return { allowed: true, escape_hatch: \"...VAR=true\" }"},
                {"when": "the action being checked is not in the law's applicable set", "then": "return { allowed: true } (fast-path)"}
            ],
            "reference_artifact_hints": ["law-hook-no-drift-on-audit-commit", "law-hook-no-destructive-git"],
            "must_obey_extra": ["NEVER_BYPASS_WITH_AUDIT", "CLAIM_REQUIRES_VERIFICATION"],
            "out_of_scope_extra": [
                "do not modify any other law-hook skill",
                "do not modify the law-enforcement dispatch loop in action-run.mjs"
            ]
        })),
        "skill-modify" => Some(json!({
            "kind": "skill-modify",
            "default_size": "S",
            "default_risk": 2,
            "edge_cases_baseline": [
                {"when": "existing callers pass legacy input shape", "then": "preserve backward-compat OR explicitly document break + retroactive decrement"},
                {"when": "new behavior differs from JSDoc", "then": "update JSDoc same commit"}
            ],
            "reference_artifact_hints": [],
            "must_obey_extra": ["CARD_STATUS_MIRRORS_REALITY"],
            "out_of_scope_extra": ["do not modify call sites unless their behavior depends on the change"]
        })),
        "skill-add" => Some(json!({
            "kind": "skill-add",
            "default_size": "S",
            "default_risk": 2,
            "edge_cases_baseline": [
                {"when": "required input field missing", "then": "throw AntilokiError code VALIDATION_REQUIRED"},
                {"when": "context.db missing", "then": "throw AntilokiError code CONTEXT_MISSING"},
                {"when": "caller passes unexpected extra fields", "then": "ignore silently (no exception)"}
            ],
            "reference_artifact_hints": [],
            "must_obey_extra": ["CARD_STATUS_MIRRORS_REALITY", "SKILL_EXPORTS_DEFAULT_FUNCTION", "SKILL_DEFAULT_TAKES_INPUT_CONTEXT"],
            "out_of_scope_extra": [
                "do not also register an action — that's a separate action-add contract",
                "do not import the new skill from any existing skill until the action layer exists"
            ]
        })),
        "action-add" => Some(json!({
            "kind": "action-add",
            "default_size": "S",
            "default_risk": 2,
            "edge_cases_baseline": [
                {"when": "required input field missing", "then": "throw AntilokiError code VALIDATION_REQUIRED"},
                {"when": "context.db missing", "then": "throw AntilokiError code CONTEXT_MISSING"}
            ],
            "reference_artifact_hints": [],
            "must_obey_extra": ["CARD_STATUS_MIRRORS_REALITY", "NEVER_BYPASS_WITH_AUDIT"],
            "out_of_scope_extra": ["do not register multiple actions in one contract — split per action"]
        })),
        "migration-add" => Some(json!({
            "kind": "migration-add",
            "default_size": "M",
            "default_risk": 4,
            "edge_cases_baseline": [
                {"when": "migration applied twice", "then": "idempotent via schema_migrations table check"},
                {"when": "rollback needed", "then": "NOT supported — fix-forward via new migration"}
            ],
            "reference_artifact_hints": [],
            "must_obey_extra": ["NEVER_BYPASS_WITH_AUDIT", "HISTORY_IS_SACRED_NO_REVERT"],
            "out_of_scope_extra": ["do not edit existing migration files", "do not drop tables without explicit human authorization"]
        })),
        "render-html-add" => Some(json!({
            "kind": "render-html-add",
            "default_size": "S",
            "default_risk": 1,
            "edge_cases_baseline": [
                {"when": "context.db missing", "then": "throw CONTEXT_MISSING"},
                {"when": "no rows in source table", "then": "render empty-state placeholder, not crash"}
            ],
            "reference_artifact_hints": ["anatomy-html", "memory-graph-html", "substrate-defense-html"],
            "must_obey_extra": ["CLAIM_REQUIRES_VERIFICATION"],
            "out_of_scope_extra": ["no external CDN dependencies (offline-renderable)"]
        })),
        _ => None,
    }
}

// class:"transitive" — see module doc comment: the real TEMPLATES[kind] entry carries function-
// valued fields (files_touched_pattern, naming_from_slug, resolveFiles, acceptance_baseline,
// goal_md_baseline) as enumerable properties on the SAME object as the 7 static fields, so the
// harness's normalizeResult+compare.mjs key-set equality can never match a twin that (correctly)
// omits them. This DOES return the real static-field data (verified in #[cfg(test)] below) —
// class:"transitive" reflects "not conform-db-fixturable", not "returns nothing".
pub fn getTemplate(kind: &Value) -> Value {
    let k = kind.as_str().unwrap_or("");
    template_static(k).unwrap_or(Value::Null)
}

// class:"transitive" — see module doc comment: {kinds, templates: TEMPLATES} carries the same
// per-kind closures as getTemplate, across all 6 kinds. Returns the real static-field projection
// for every kind (verified in #[cfg(test)] below).
pub fn contractTemplates(_input: &Value, _db: &Connection) -> Value {
    let mut templates = Map::new();
    for k in TEMPLATE_KINDS {
        templates.insert(k.to_string(), template_static(k).expect("TEMPLATE_KINDS entry must have static data"));
    }
    json!({
        "kinds": TEMPLATE_KINDS,
        "templates": templates
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn out_of_scope_list_is_the_static_five() {
        assert_eq!(
            universalOutOfScopeList(),
            json!([
                "no formatting/lint changes outside files_touched",
                "no rename of unrelated identifiers",
                "no test-suite reorganization",
                "no dependency upgrades unrelated to the contract",
                "no comment cleanup of pre-existing code",
            ])
        );
    }

    #[test]
    fn goal_md_null_body_uses_fallbacks_and_omits_strategy_section() {
        let card = json!({ "slug": "WEIRD-CARD", "title": "something novel", "body_json": null });
        let md = noTemplateGoalMd(&card).as_str().unwrap().to_string();
        assert!(md.contains("## Mission"));
        assert!(md.starts_with("## Mission\nsomething novel. (No template classified"));
        assert!(md.contains("## Before DEV proceeds, TL must fill"));
        assert!(md.contains("files_touched"));
        assert!(md.contains("acceptance_json"));
        assert!(md.contains("naming_json"));
        assert!(!md.contains("## Strategy hints from the card")); // fix is None → section omitted
        assert!(md.contains("didn't match any built-in template heuristic")); // Background fallback
    }

    #[test]
    fn goal_md_object_body_prefers_motivation_and_includes_fix() {
        let card = json!({
            "slug": "BAR",
            "title": "Fix bar",
            "body_json": { "motivation": "root cause X", "fix": "patch Y" },
        });
        let md = noTemplateGoalMd(&card).as_str().unwrap().to_string();
        assert!(md.contains("## Background\nroot cause X"));
        assert!(md.contains("## Strategy hints from the card\npatch Y"));
    }

    #[test]
    fn goal_md_missing_title_renders_undefined() {
        let card = json!({ "slug": "NO-TITLE" });
        let md = noTemplateGoalMd(&card).as_str().unwrap().to_string();
        assert!(md.starts_with("## Mission\nundefined. (No template classified"));
    }

    #[test]
    fn get_template_law_hook_add_returns_the_7_static_fields() {
        let v = getTemplate(&json!("law-hook-add"));
        assert_eq!(
            v,
            json!({
                "kind": "law-hook-add",
                "default_size": "S",
                "default_risk": 3,
                "edge_cases_baseline": [
                    {"when": "action_name is null or empty", "then": "return { allowed: true } (no-op for unrelated dispatches)"},
                    {"when": "input_data is null", "then": "treat as empty, do not throw"},
                    {"when": "escape_hatch env var is set", "then": "return { allowed: true, escape_hatch: \"...VAR=true\" }"},
                    {"when": "the action being checked is not in the law's applicable set", "then": "return { allowed: true } (fast-path)"}
                ],
                "reference_artifact_hints": ["law-hook-no-drift-on-audit-commit", "law-hook-no-destructive-git"],
                "must_obey_extra": ["NEVER_BYPASS_WITH_AUDIT", "CLAIM_REQUIRES_VERIFICATION"],
                "out_of_scope_extra": [
                    "do not modify any other law-hook skill",
                    "do not modify the law-enforcement dispatch loop in action-run.mjs"
                ]
            })
        );
    }

    #[test]
    fn get_template_skill_modify() {
        let v = getTemplate(&json!("skill-modify"));
        assert_eq!(v["kind"], json!("skill-modify"));
        assert_eq!(v["default_size"], json!("S"));
        assert_eq!(v["default_risk"], json!(2));
        assert_eq!(v["edge_cases_baseline"].as_array().unwrap().len(), 2);
        assert_eq!(v["reference_artifact_hints"], json!([]));
        assert_eq!(v["must_obey_extra"], json!(["CARD_STATUS_MIRRORS_REALITY"]));
        assert_eq!(v["out_of_scope_extra"], json!(["do not modify call sites unless their behavior depends on the change"]));
    }

    #[test]
    fn get_template_skill_add() {
        let v = getTemplate(&json!("skill-add"));
        assert_eq!(v["default_size"], json!("S"));
        assert_eq!(v["default_risk"], json!(2));
        assert_eq!(v["edge_cases_baseline"].as_array().unwrap().len(), 3);
        assert_eq!(
            v["must_obey_extra"],
            json!(["CARD_STATUS_MIRRORS_REALITY", "SKILL_EXPORTS_DEFAULT_FUNCTION", "SKILL_DEFAULT_TAKES_INPUT_CONTEXT"])
        );
        assert_eq!(
            v["out_of_scope_extra"],
            json!([
                "do not also register an action — that's a separate action-add contract",
                "do not import the new skill from any existing skill until the action layer exists"
            ])
        );
    }

    #[test]
    fn get_template_action_add() {
        let v = getTemplate(&json!("action-add"));
        assert_eq!(v["default_size"], json!("S"));
        assert_eq!(v["default_risk"], json!(2));
        assert_eq!(v["must_obey_extra"], json!(["CARD_STATUS_MIRRORS_REALITY", "NEVER_BYPASS_WITH_AUDIT"]));
        assert_eq!(v["out_of_scope_extra"], json!(["do not register multiple actions in one contract — split per action"]));
    }

    #[test]
    fn get_template_migration_add() {
        let v = getTemplate(&json!("migration-add"));
        assert_eq!(v["default_size"], json!("M"));
        assert_eq!(v["default_risk"], json!(4));
        assert_eq!(v["must_obey_extra"], json!(["NEVER_BYPASS_WITH_AUDIT", "HISTORY_IS_SACRED_NO_REVERT"]));
        assert_eq!(
            v["out_of_scope_extra"],
            json!(["do not edit existing migration files", "do not drop tables without explicit human authorization"])
        );
    }

    #[test]
    fn get_template_render_html_add() {
        let v = getTemplate(&json!("render-html-add"));
        assert_eq!(v["default_size"], json!("S"));
        assert_eq!(v["default_risk"], json!(1));
        assert_eq!(v["reference_artifact_hints"], json!(["anatomy-html", "memory-graph-html", "substrate-defense-html"]));
        assert_eq!(v["must_obey_extra"], json!(["CLAIM_REQUIRES_VERIFICATION"]));
        assert_eq!(v["out_of_scope_extra"], json!(["no external CDN dependencies (offline-renderable)"]));
    }

    #[test]
    fn get_template_unknown_kind_is_null() {
        assert_eq!(getTemplate(&json!("not-a-real-kind")), Value::Null);
        assert_eq!(getTemplate(&json!(null)), Value::Null);
    }

    #[test]
    fn contract_templates_lists_all_6_kinds_with_static_fields() {
        let v = contractTemplates(&Value::Null, &Connection::open_in_memory().unwrap());
        assert_eq!(
            v["kinds"],
            json!(["law-hook-add", "skill-modify", "skill-add", "action-add", "migration-add", "render-html-add"])
        );
        let templates = v["templates"].as_object().unwrap();
        assert_eq!(templates.len(), 6);
        for kind in ["law-hook-add", "skill-modify", "skill-add", "action-add", "migration-add", "render-html-add"] {
            assert_eq!(templates[kind]["kind"], json!(kind));
            assert_eq!(templates[kind], getTemplate(&json!(kind)));
        }
    }
}

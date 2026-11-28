//! card-draft-auto.rs — twin of skills/card/card-draft-auto.mjs (db READ lane, attached `memory`
//! db for past-rejection mining). NAMES ARE CONTRACT: `cardDraftAuto` (default, async-over-sync —
//! the JS `await`s only sync SQLite reads via context.db), `detectTemplateKind` (no-db helper).
//! PL-side orchestrator: no-decisions lint, deterministic template detection (explicit override or
//! keyword heuristic), lexical-fallback RAG over `cards` (LIKE-token scoring), past PL-rejection
//! mining over `memory.memories`, assembled into a 95%-filled card draft. No LLM call.
use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::sync::OnceLock;

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }
// = JS String(v)
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
// = JS String(v) where v may be the `undefined` of an absent destructured key.
fn js_string_opt(v: Option<&Value>) -> String { match v { None => "undefined".into(), Some(x) => js_string(x) } }

// Keyword heuristics — order matters (for loop returns the FIRST match), mirrors the JS
// `heuristics` array exactly.
fn re_bug_fix() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"\b(bug|broken|fail|error|fix|crash|reproduce)\b").unwrap()) }
fn re_refactor() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"\b(refactor|rename|restructure|cleanup|reorganize)\b").unwrap()) }
fn re_tech_debt() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"\b(tech[- ]?debt|cruft|deprecated|legacy|cleanup)\b").unwrap()) }
fn re_arch() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"\b(architecture|design|adr|decision|choose|approach)\b").unwrap()) }
fn re_docs() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"\b(docs?|documentation|readme|guide|tutorial|explain)\b").unwrap()) }
fn re_ops() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"\b(deploy|env|config|pre-commit|hook|migration|infrastructure)\b").unwrap()) }
fn re_feature_add() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"\b(add|new|feature|implement|build|create)\b").unwrap()) }

// Decision-defer phrases (DEFER_PHRASES, case-insensitive) — only reachable via the guard-throw
// path, which conform never fixtures (the harness compares thrown MESSAGES, fragile by design).
fn re_defer_appropriate() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"(?i)\bappropriate(ly)?\b").unwrap()) }
fn re_defer_as_needed() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"(?i)\bas needed\b").unwrap()) }
fn re_defer_if_necessary() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"(?i)\bif necessary\b").unwrap()) }
fn re_defer_where_applicable() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"(?i)\bwhere applicable\b").unwrap()) }
fn re_defer_somehow() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"(?i)\bsomehow\b").unwrap()) }
fn re_defer_some_kind_of() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"(?i)\bsome kind of\b").unwrap()) }
fn re_defer_etc() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"(?i)\betc\.?$").unwrap()) }

/// JS named export `detectTemplateKind({ raw_intent, explicit })` — single destructured-object arg.
pub fn detectTemplateKind(input: &Value) -> Value {
    let obj = input.as_object();
    let get = |k: &str| obj.and_then(|o| o.get(k));

    let explicit = get("explicit").cloned().unwrap_or(Value::Null);
    if truthy(Some(&explicit)) && !crate::card_card_templates::getTemplate(&explicit).is_null() {
        return json!({ "kind": explicit, "source": "explicit", "confidence": 1.0 });
    }

    let haystack = js_string_opt(get("raw_intent")).to_lowercase();
    let heuristics: [(&str, &Regex); 7] = [
        ("bug-fix", re_bug_fix()),
        ("refactor", re_refactor()),
        ("tech-debt", re_tech_debt()),
        ("arch", re_arch()),
        ("docs", re_docs()),
        ("ops", re_ops()),
        ("feature-add", re_feature_add()),
    ];
    for (kind, re) in heuristics {
        if re.is_match(&haystack) {
            return json!({ "kind": kind, "source": "keyword-heuristic", "confidence": 0.65 });
        }
    }
    json!({ "kind": Value::Null, "source": "no-template", "confidence": 0.0 })
}

// Internal (non-exported) helper mirroring `ragSimilarCards` — lexical-fallback RAG over `cards`.
fn rag_similar_cards(raw_intent: &str, db: &Connection, limit: &Value) -> Value {
    let tokens: Vec<String> = raw_intent
        .to_lowercase()
        .split_whitespace()
        .filter(|w| w.chars().count() > 3)
        .take(6)
        .map(|s| s.to_string())
        .collect();
    if tokens.is_empty() { return json!([]); }

    let conds = vec!["(LOWER(title) LIKE ? OR LOWER(body_json) LIKE ?)"; tokens.len()].join(" OR ");
    let sql = format!("SELECT slug, title FROM cards WHERE {conds} LIMIT ?");
    let mut params: Vec<Value> = Vec::new();
    for t in &tokens {
        params.push(json!(format!("%{t}%")));
        params.push(json!(format!("%{t}%")));
    }
    params.push(limit.clone());

    match crate::db_json::query_rows(db, &sql, &params) {
        Ok(Value::Array(rows)) => Value::Array(
            rows.into_iter()
                .map(|r| json!({
                    "slug": r.get("slug").cloned().unwrap_or(Value::Null),
                    "title": r.get("title").cloned().unwrap_or(Value::Null),
                    "source": "lexical-fallback",
                }))
                .collect(),
        ),
        _ => json!([]),
    }
}

// Internal (non-exported) helper mirroring `pastRejectionHints` — cross-db read of memory.memories.
fn past_rejection_hints(db: &Connection) -> Value {
    match crate::db_json::query_rows(
        db,
        "SELECT id, situation, guidance FROM memory.memories WHERE type = 'correction' AND tags_json LIKE '%card%' ORDER BY id DESC LIMIT 5",
        &[],
    ) {
        Ok(Value::Array(rows)) => Value::Array(
            rows.into_iter()
                .map(|r| json!({
                    "memory_id": r.get("id").cloned().unwrap_or(Value::Null),
                    "guidance": r.get("guidance").cloned().unwrap_or(Value::Null),
                    "situation": r.get("situation").cloned().unwrap_or(Value::Null),
                }))
                .collect(),
        ),
        _ => json!([]),
    }
}

/// JS default export `cardDraftAuto(input, context)` — standard db lane (async-over-sync).
pub fn cardDraftAuto(input: &Value, db: &Connection) -> Result<Value, String> {
    let obj = input.as_object();
    let get = |k: &str| obj.and_then(|o| o.get(k));

    // ── validate raw_intent (guard-throw path; not fixture-exercised) ──────────────────────
    let raw_intent_val = get("raw_intent").cloned().unwrap_or(Value::Null);
    let raw_intent = match raw_intent_val.as_str() {
        Some(s) if s.chars().count() >= 5 => s.to_string(),
        _ => return Err("card-draft-auto: raw_intent (\u{2265}5 chars) is required".into()),
    };

    // ── 1. no-decisions lint (guard-throw path; not fixture-exercised) ─────────────────────
    let defer_res: [&Regex; 7] = [
        re_defer_appropriate(), re_defer_as_needed(), re_defer_if_necessary(),
        re_defer_where_applicable(), re_defer_somehow(), re_defer_some_kind_of(), re_defer_etc(),
    ];
    if defer_res.iter().any(|re| re.is_match(&raw_intent)) {
        return Err("card-draft-auto: raw_intent contains decision-defer phrase(s); resolve them upfront".into());
    }

    let template_kind_input = get("template_kind").cloned().unwrap_or(Value::Null);
    let priority_val = get("priority").cloned().unwrap_or(Value::Null);
    let domain_val = get("domain").cloned().unwrap_or(Value::Null);
    // Default applies ONLY when the key is ABSENT (JS destructuring default) — an explicit null
    // present in input overrides the default and flows through as-is.
    let rag_limit_val = match get("rag_limit") { None => json!(3), Some(v) => v.clone() };

    // ── 2. template detection ───────────────────────────────────────────────────────────────
    let tpl_detect = detectTemplateKind(&json!({ "raw_intent": raw_intent, "explicit": template_kind_input }));
    let tpl_kind_val = tpl_detect.get("kind").cloned().unwrap_or(Value::Null);
    let template = if truthy(Some(&tpl_kind_val)) { crate::card_card_templates::getTemplate(&tpl_kind_val) } else { Value::Null };
    let is_template = !template.is_null();

    // ── 3 & 4. RAG similar cards + past PL-rejection hints ──────────────────────────────────
    let similar = rag_similar_cards(&raw_intent, db, &rag_limit_val);
    let hints_from_past = past_rejection_hints(db);

    // ── 5. template-derived fields ───────────────────────────────────────────────────────────
    // naming_from_intent is a FUNCTION VALUE per template in the JS source, always of the shape
    // `${slug_prefix}-${toKebab(intent).slice(0, 40)}` — reproduced directly rather than "called".
    let kebab = crate::card_card_templates::toKebab(&json!(raw_intent));
    let kebab_str = kebab.as_str().unwrap_or("");
    let sliced40: String = kebab_str.chars().take(40).collect();
    let naming_slug = if is_template {
        let prefix = template.get("slug_prefix").and_then(|v| v.as_str()).unwrap_or("");
        format!("{prefix}-{sliced40}")
    } else {
        sliced40.to_uppercase()
    };

    let body_skeleton = if is_template {
        template.get("body_skeleton").and_then(|v| v.as_str()).unwrap_or("").to_string()
    } else {
        format!("## Intent\n{raw_intent}\n\n## Acceptance\n<TL must define>")
    };
    let out_of_scope = if is_template {
        template.get("baseline_out_of_scope").cloned().unwrap_or(json!([]))
    } else {
        json!(["no scope expansion beyond stated intent"])
    };
    let acceptance_hints = if is_template {
        template.get("baseline_acceptance_hints").cloned().unwrap_or(json!([]))
    } else {
        json!([])
    };

    // ── 6. classify priority ────────────────────────────────────────────────────────────────
    let template_default_priority = if is_template { template.get("default_priority").cloned().unwrap_or(Value::Null) } else { Value::Null };
    let final_priority = nz(Some(&priority_val)).or(nz(Some(&template_default_priority))).cloned().unwrap_or(json!(3));

    // ── 7. assemble card ─────────────────────────────────────────────────────────────────────
    let title: String = raw_intent.chars().take(80).collect();
    let related_cards: Vec<Value> = similar
        .as_array()
        .map(|a| a.iter().map(|s| s.get("slug").cloned().unwrap_or(Value::Null)).collect())
        .unwrap_or_default();

    let card = json!({
        "slug": naming_slug,
        "title": title,
        "template_kind": tpl_kind_val,
        "priority": final_priority,
        "domain": domain_val,
        "body_md": body_skeleton,
        "out_of_scope": out_of_scope,
        "acceptance_hints": acceptance_hints,
        "related_cards": related_cards,
        "confidence": {
            "template_kind": tpl_detect.get("confidence").cloned().unwrap_or(Value::Null),
            "slug": if is_template { 0.85 } else { 0.4 },
            "body_skeleton": if is_template { 0.9 } else { 0.2 },
            "out_of_scope": if is_template { 0.7 } else { 0.3 },
            "priority": if !priority_val.is_null() { 1.0 } else if is_template { 0.7 } else { 0.3 },
        },
    });

    Ok(json!({
        "card": card,
        "sources": {
            "template_detected": tpl_detect,
            "similar_cards": similar,
            "past_rejection_hints": hints_from_past,
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE cards (slug TEXT PRIMARY KEY, title TEXT, body_json TEXT);
             ATTACH ':memory:' AS memory;
             CREATE TABLE memory.memories (
               id INTEGER PRIMARY KEY AUTOINCREMENT, type TEXT NOT NULL, title TEXT NOT NULL,
               situation TEXT NOT NULL, guidance TEXT NOT NULL, tags_json TEXT NOT NULL DEFAULT '[]',
               created_by TEXT NOT NULL
             );",
        )
        .unwrap();
        c
    }

    #[test]
    fn detect_template_kind_explicit_wins() {
        let r = detectTemplateKind(&json!({ "raw_intent": "whatever", "explicit": "bug-fix" }));
        assert_eq!(r, json!({ "kind": "bug-fix", "source": "explicit", "confidence": 1.0 }));
    }

    #[test]
    fn detect_template_kind_invalid_explicit_falls_back_to_heuristic() {
        let r = detectTemplateKind(&json!({ "raw_intent": "fix login crash", "explicit": "not-a-real-kind" }));
        assert_eq!(r["kind"], json!("bug-fix"));
        assert_eq!(r["source"], json!("keyword-heuristic"));
    }

    #[test]
    fn detect_template_kind_heuristics_first_match_wins_and_lowercases() {
        assert_eq!(detectTemplateKind(&json!({ "raw_intent": "Fix the LOGIN Crash" }))["kind"], json!("bug-fix"));
        assert_eq!(detectTemplateKind(&json!({ "raw_intent": "remove deprecated legacy tech debt module" }))["kind"], json!("tech-debt"));
        assert_eq!(detectTemplateKind(&json!({ "raw_intent": "decide architecture for the new substrate" }))["kind"], json!("arch"));
        assert_eq!(detectTemplateKind(&json!({ "raw_intent": "write documentation guide for onboarding" }))["kind"], json!("docs"));
        assert_eq!(detectTemplateKind(&json!({ "raw_intent": "configure deploy pipeline and pre-commit hook" }))["kind"], json!("ops"));
        assert_eq!(detectTemplateKind(&json!({ "raw_intent": "add new feature for user dashboard" }))["kind"], json!("feature-add"));
    }

    #[test]
    fn detect_template_kind_no_match_returns_null() {
        let r = detectTemplateKind(&json!({ "raw_intent": "wibble wobble esoteric thing", "explicit": null }));
        assert_eq!(r, json!({ "kind": null, "source": "no-template", "confidence": 0.0 }));
    }

    #[test]
    fn detect_template_kind_non_string_raw_intent_and_absent_key() {
        assert_eq!(detectTemplateKind(&json!({ "raw_intent": 123 }))["kind"], Value::Null);
        assert_eq!(detectTemplateKind(&json!({}))["kind"], Value::Null);
    }

    #[test]
    fn card_draft_auto_happy_path_bug_fix_template() {
        let c = db();
        let r = cardDraftAuto(&json!({ "raw_intent": "fix the broken login crash" }), &c).unwrap();
        assert_eq!(r["card"]["template_kind"], json!("bug-fix"));
        assert!(r["card"]["slug"].as_str().unwrap().starts_with("BUG-"));
        assert!(r["card"]["body_md"].as_str().unwrap().contains("Symptom"));
        assert!(r["card"]["body_md"].as_str().unwrap().contains("Reproducer"));
        assert!(r["card"]["out_of_scope"].as_array().unwrap().len() >= 2);
        assert!(r["card"]["confidence"]["template_kind"].as_f64().unwrap() > 0.0);
    }

    #[test]
    fn card_draft_auto_novel_intent_without_template() {
        let c = db();
        let r = cardDraftAuto(&json!({ "raw_intent": "wibble unprecedented" }), &c).unwrap();
        assert_eq!(r["card"]["template_kind"], Value::Null);
        assert!(!r["card"]["slug"].as_str().unwrap().is_empty());
        assert!(!r["card"]["body_md"].as_str().unwrap().is_empty());
        assert_eq!(r["card"]["confidence"]["template_kind"], json!(0.0));
    }

    #[test]
    fn card_draft_auto_priority_override_beats_template_default() {
        let c = db();
        let r = cardDraftAuto(&json!({ "raw_intent": "add new feature for user dashboard", "priority": 1 }), &c).unwrap();
        assert_eq!(r["card"]["priority"], json!(1));
        assert_eq!(r["card"]["confidence"]["priority"], json!(1.0));
    }

    #[test]
    fn card_draft_auto_priority_zero_is_not_nullish() {
        let c = db();
        let r = cardDraftAuto(&json!({ "raw_intent": "add a new small feature toggle", "priority": 0 }), &c).unwrap();
        assert_eq!(r["card"]["priority"], json!(0));
        assert_eq!(r["card"]["confidence"]["priority"], json!(1.0));
    }

    #[test]
    fn card_draft_auto_surfaces_lexically_similar_existing_cards_and_respects_rag_limit() {
        let c = db();
        c.execute("INSERT INTO cards (slug, title, body_json) VALUES ('OLD-GIZMO', 'Legacy gizmo overhaul', '\"notes\"')", []).unwrap();
        c.execute("INSERT INTO cards (slug, title, body_json) VALUES ('GIZMO-2', 'Another gizmo idea', NULL)", []).unwrap();
        c.execute("INSERT INTO cards (slug, title, body_json) VALUES ('GIZMO-3', 'gizmo prototype', NULL)", []).unwrap();
        c.execute("INSERT INTO cards (slug, title, body_json) VALUES ('UNRELATED', 'Something else entirely', '\"other\"')", []).unwrap();
        let r = cardDraftAuto(&json!({ "raw_intent": "wibble unprecedented gizmo", "domain": "ops-tools", "rag_limit": 2 }), &c).unwrap();
        assert_eq!(r["card"]["domain"], json!("ops-tools"));
        assert_eq!(r["sources"]["similar_cards"].as_array().unwrap().len(), 2);
        assert!(r["sources"]["similar_cards"].as_array().unwrap().iter().all(|s| s["slug"].as_str().unwrap().starts_with("GIZMO") || s["slug"] == json!("OLD-GIZMO")));
    }

    #[test]
    fn card_draft_auto_empty_tokens_yields_no_similar_cards() {
        let c = db();
        c.execute("INSERT INTO cards (slug, title, body_json) VALUES ('X', 'do it now ok', NULL)", []).unwrap();
        let r = cardDraftAuto(&json!({ "raw_intent": "do it now ok" }), &c).unwrap();
        assert_eq!(r["sources"]["similar_cards"], json!([]));
    }

    #[test]
    fn card_draft_auto_past_rejection_hints_filters_type_tag_and_limits_5_desc() {
        let c = db();
        let ins = |t: &str, tags: &str| {
            c.execute(
                "INSERT INTO memory.memories (type, title, situation, guidance, tags_json, created_by) VALUES (?, 'title', 'sit', 'guide', ?, 'tester')",
                rusqlite::params![t, tags],
            ).unwrap();
        };
        ins("correction", "[\"card\"]"); // id 1 — qualifies, dropped by LIMIT 5
        ins("correction", "[\"card\"]"); // id 2 — qualifies
        ins("lesson", "[\"card\"]");     // id 3 — wrong type, excluded
        ins("correction", "[\"other\"]"); // id 4 — no card tag, excluded
        ins("correction", "[\"card\"]"); // id 5 — qualifies
        ins("correction", "[\"card\"]"); // id 6 — qualifies
        ins("correction", "[\"card\"]"); // id 7 — qualifies
        ins("correction", "[\"card\"]"); // id 8 — qualifies

        let r = cardDraftAuto(&json!({ "raw_intent": "refactor the reporting pipeline for clarity" }), &c).unwrap();
        let hints = r["sources"]["past_rejection_hints"].as_array().unwrap();
        assert_eq!(hints.len(), 5);
        let ids: Vec<i64> = hints.iter().map(|h| h["memory_id"].as_i64().unwrap()).collect();
        assert_eq!(ids, vec![8, 7, 6, 5, 2]);
        assert_eq!(hints[0]["guidance"], json!("guide"));
        assert_eq!(hints[0]["situation"], json!("sit"));
    }

    #[test]
    fn card_draft_auto_invalid_explicit_template_kind_falls_back_to_heuristic() {
        let c = db();
        let r = cardDraftAuto(&json!({ "raw_intent": "fix the broken login crash", "template_kind": "not-a-real-kind" }), &c).unwrap();
        assert_eq!(r["card"]["template_kind"], json!("bug-fix"));
    }
}

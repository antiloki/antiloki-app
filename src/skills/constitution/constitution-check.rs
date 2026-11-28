//! constitution-check.rs — twin of skills/constitution/constitution-check.mjs (db lane,
//! cross-db `laws`+`judgments`, plus a real native tree-sitter parse for the `ast`/`stmt`
//! detector tiers). NAMES ARE CONTRACT: `constitutionCheck` (default).
//!
//! THE MEASURED merge gate: given a candidate `content` for `path`, re-runs the active
//! content-scope laws (`regex`/`absent`/`import`/`ast`/`stmt`) over the candidate text and
//! compares the resulting hit list against the SAME laws' violations stored in the latest
//! `done` review snapshot. Pure computation over already-composed state: $0, no AI, no writes.
//!
//! `async function constitutionCheck` (JS) → ordinary sync `pub fn` here. The only `await` in
//! the JS source is `getTree(grammar, content)`, and ts-query.rs's native `getTree` has no I/O
//! step to await (see its module doc) — "async over sync", per the playbook's async-default-
//! export convention.
//!
//! Composes FOUR already-converted twins: `constitution__helpers_schema::ensureConstitutionSchema`,
//! `constitution_constitution_law::{composedConstitution, resolveFrameworks}` (the REAL `pub`
//! versions, not an inlined copy — constitution-law.mjs has a twin now), `constitution__helpers_ts_query::
//! {grammarForPath, getTree}`, `constitution__helpers_cognitive::analyzeTree` (this pass's new port —
//! see that module's doc for why the earlier PARITY-DEFERRED blocker no longer holds), and
//! `constitution__helpers_detectors::{lawApplies, exceptionCovers, runDetector}`. `runDetector`
//! takes the two extra native params (`ts_tree`/`ts_language`) its own module doc explains; this
//! twin holds the `(Tree, Language)` pair alive in a local `Option` for the lifetime of the
//! per-law loop, exactly mirroring how `ctx.tsTree`/`ctx.tsLanguage` stay live in the JS closure.
//!
//! `ctx.grammar`/`ctx.stmt` are JS keys added ONLY inside `if (grammar) { ... }` (so they read as
//! `undefined` when absent); this twin always inserts them (`Value::Null` when not parsed) since
//! detectors.rs's `runDetector` treats a missing key and an explicit `null` identically via
//! `ctx.get(...)` — same convention as `ctx.spans`/`ctx.metrics`/`ctx.smells`, which the JS source
//! itself always sets to `null` up front.
//!
//! `Math.round` in `pen()`'s penalty sum is JS's HALF-UP round (not Rust's half-away-from-zero
//! `.round()`); ported via the `js_round` helper already established in constitution-estimate.rs.
//! Every value fed into it here is non-negative (`weight` 0-10, `confidence` 0-1, count >= 0), so
//! the half-up/half-away-from-zero distinction (which only differs on negative inputs) is inert in
//! practice — carried anyway for a faithful, non-coincidental port.
//!
//! `context.db` missing → JS throws `err.required('constitution-check', 'context.db')`. Not
//! modeled: `db: &Connection` is a required, non-nullable reference — same convention as every
//! sibling twin.
// QUALITY: 80/100 — real cross-db reads (laws+judgments) + a real native AST parse for the
// ast/stmt tiers; the arithmetic (count-diff, penalty rollup) is a direct 1:1 port of small pure
// helpers with no hidden branching.

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use tree_sitter::{Language, Tree};

fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } } // = JS String(v ?? '')
fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand

// JS `Math.round`: floor(x + 0.5) — rounds HALF UP, unlike Rust's half-away-from-zero `.round()`.
fn js_round(x: f64) -> f64 { (x + 0.5).floor() }

const CONTENT_DETECTORS: [&str; 5] = ["regex", "absent", "import", "ast", "stmt"];

// `const m = new Map(); for (v of arr) m.set(key, (m.get(key) ?? 0) + 1)` — insertion-ordered
// per-law_key counts (a plain Vec keeps JS Map's insertion order without pulling in indexmap).
fn cnt(arr: &[Value]) -> Vec<(String, i64)> {
    let mut m: Vec<(String, i64)> = Vec::new();
    for v in arr {
        let k = v.get("law_key").and_then(Value::as_str).unwrap_or("").to_string();
        match m.iter_mut().find(|e| e.0 == k) {
            Some(e) => e.1 += 1,
            None => m.push((k, 1)),
        }
    }
    m
}
fn get_count(m: &[(String, i64)], k: &str) -> i64 {
    m.iter().find(|e| e.0 == k).map(|e| e.1).unwrap_or(0)
}

// `const e = by.get(k) ?? { n: 0, w: v.weight ?? 1, c: v.confidence ?? 1 }; e.n += 1;` — w/c are
// captured from the FIRST row seen for a given law_key only (never updated on later hits), then
// `s += Math.round(e.w * e.n * e.c)` summed per key.
fn pen(arr: &[Value]) -> i64 {
    let mut by: Vec<(String, i64, f64, f64)> = Vec::new(); // (law_key, n, w, c)
    for v in arr {
        let key = v.get("law_key").and_then(Value::as_str).unwrap_or("").to_string();
        match by.iter_mut().find(|e| e.0 == key) {
            Some(e) => e.1 += 1,
            None => {
                let w = v.get("weight").and_then(Value::as_f64).unwrap_or(1.0);
                let c = v.get("confidence").and_then(Value::as_f64).unwrap_or(1.0);
                by.push((key, 1, w, c));
            }
        }
    }
    by.into_iter().map(|(_, n, w, c)| js_round(w * (n as f64) * c) as i64).sum()
}

/// `export default async function constitutionCheck(input, context)`
pub fn constitutionCheck(input: &Value, db: &Connection) -> Result<Value, String> {
    let _ = crate::constitution__helpers_schema::ensureConstitutionSchema(db);

    let path = str_qq(input.get("path")).replace('\\', "/").trim().to_string();
    if path.is_empty() {
        return Err("constitution-check: path is required".to_string());
    }
    let content = str_qq(input.get("content"));
    if content.trim().is_empty() {
        return Err("constitution-check: content is required".to_string());
    }

    let composed = crate::constitution_constitution_law::composedConstitution(db);
    let mut laws = composed.get("laws").and_then(Value::as_array).cloned().unwrap_or_default();
    // EXTRA LAWS (a team's review node, 2026-11-16): rules the LEADER wrote in the constitution's own format, judged by the same detectors for THIS check only. Nothing is saved: the project's laws are untouched.
    if let Some(extra) = input.get("extra_laws").and_then(Value::as_array) {
        for l in extra { let mut l = l.clone(); if let Some(o) = l.as_object_mut() { o.insert("status".into(), json!("ratified")); o.insert("enforcement".into(), json!("score")); o.entry("severity").or_insert(json!("error")); o.entry("weight").or_insert(json!(5)); } laws.push(l); }
    }
    let exceptions = composed.get("exceptions").cloned().unwrap_or_else(|| json!([]));
    let frameworks = crate::constitution_constitution_law::resolveFrameworks(db);

    let scoring: Vec<Value> = laws
        .iter()
        .filter(|l| {
            l.get("status").and_then(Value::as_str) == Some("ratified")
                && l.get("enforcement").and_then(Value::as_str) == Some("score")
        })
        .cloned()
        .collect();

    let applies_ctx = json!({ "path": path, "frameworks": frameworks });
    let checkable: Vec<Value> = scoring
        .into_iter()
        .filter(|l| {
            let detector = l.get("detector").and_then(Value::as_str).unwrap_or("");
            if !CONTENT_DETECTORS.contains(&detector) {
                return false;
            }
            if !truthy(Some(&crate::constitution__helpers_detectors::lawApplies(l, &applies_ctx))) {
                return false;
            }
            let key_v = l.get("key").cloned().unwrap_or(Value::Null);
            !truthy(Some(&crate::constitution__helpers_detectors::exceptionCovers(&exceptions, &key_v, &json!(path.clone()))))
        })
        .collect();

    // ── AFTER: run the checkable laws over the candidate text (one parse, like the judge) ──
    let grammar_val = crate::constitution__helpers_ts_query::grammarForPath(&json!(path.clone()));
    let grammar_str = grammar_val.as_str().map(|s| s.to_string());

    let mut ctx = Map::new();
    ctx.insert("path".into(), json!(path.clone()));
    ctx.insert("source".into(), json!(content.clone()));
    ctx.insert("spans".into(), Value::Null);
    ctx.insert("metrics".into(), Value::Null);
    ctx.insert("smells".into(), Value::Null);
    ctx.insert("frameworks".into(), frameworks);
    ctx.insert("grammar".into(), Value::Null);
    ctx.insert("stmt".into(), Value::Null);
    ctx.insert("project_rules".into(), crate::constitution__helpers_project_rules::projectRules(&str_qq(Some(&crate::engine_workspace_root::getWorkspaceRoot(db)))));
    // A CAP LIMITS WHAT IS LISTED, NEVER WHAT IS COUNTED (2026-10-06): the detectors keep five hits per law per file; the law-check A/B showed an agent copy a legacy pattern four
    // more times into a file that already had five and read "violations before 10 -> after 10, no change". The check counts every hit.
    ctx.insert("uncapped".into(), json!(true));

    let mut tree_holder: Option<(Tree, Language)> = None;
    if let Some(g) = &grammar_str {
        if let Some((tree, language)) = crate::constitution__helpers_ts_query::getTree(g, &content) {
            let stmt = crate::constitution__helpers_cognitive::analyzeTree(g, &tree, &content);
            ctx.insert("grammar".into(), json!(g.clone()));
            ctx.insert("stmt".into(), stmt);
            tree_holder = Some((tree, language));
        }
    }
    let ctx_val = Value::Object(ctx);
    let (tree_ref, lang_ref) = match &tree_holder {
        Some((t, l)) => (Some(t), Some(l)),
        None => (None, None),
    };

    let mut after_all: Vec<Value> = Vec::new();
    for law in &checkable {
        let hits = crate::constitution__helpers_detectors::runDetector(law, &ctx_val, tree_ref, lang_ref);
        let Some(hits_arr) = hits.as_array() else { continue }; // fog — not judgeable here, never a hit
        for h in hits_arr {
            let title = nz(law.get("title")).or_else(|| law.get("key")).cloned().unwrap_or(Value::Null);
            after_all.push(json!({
                "law_key": law.get("key").cloned().unwrap_or(Value::Null),
                "line": h.get("line").cloned().unwrap_or(Value::Null),
                "evidence": h.get("evidence").cloned().unwrap_or(Value::Null),
                "weight": law.get("weight").cloned().unwrap_or(Value::Null),
                "confidence": law.get("confidence").cloned().unwrap_or(Value::Null),
                "title": title,
                "severity": law.get("severity").cloned().unwrap_or(Value::Null),
            }));
        }
    }

    // the view the SCORE uses: at most HITS_CAP per law, exactly as the judge scores and stores them (so `compliance` stays comparable with the stored penalty)
    let cap = crate::constitution__helpers_detectors::HITS_CAP;
    let after: Vec<Value> = {
        let mut seen: Vec<(String, usize)> = Vec::new();
        after_all.iter().filter(|h| {
            let k = h.get("law_key").and_then(Value::as_str).unwrap_or("").to_string();
            match seen.iter_mut().find(|e| e.0 == k) { Some(e) => { e.1 += 1; e.1 <= cap } None => { seen.push((k, 1)); true } }
        }).cloned().collect()
    };

    // ── BEFORE: the same families' violations in the latest snapshot (the stored truth) ──
    let snap = crate::db_json::query_rows(
        db,
        "SELECT id FROM judgments.review_snapshots WHERE status='done' ORDER BY id DESC LIMIT 1",
        &[],
    )?
    .as_array()
    .and_then(|a| a.first().cloned());

    let stored: Vec<Value> = match &snap {
        Some(s) => {
            let snap_id = s.get("id").cloned().unwrap_or(Value::Null);
            crate::db_json::query_rows(
                db,
                "SELECT law_key, line, weight, confidence FROM judgments.review_violations WHERE snapshot_id=? AND path=?",
                &[snap_id, json!(path.clone())],
            )?
            .as_array()
            .cloned()
            .unwrap_or_default()
        }
        None => Vec::new(),
    };

    let mut detector_of: HashMap<String, String> = HashMap::new();
    let mut title_of: HashMap<String, Value> = HashMap::new();
    for l in &laws {
        let key = l.get("key").and_then(Value::as_str).unwrap_or("").to_string();
        detector_of.insert(key.clone(), l.get("detector").and_then(Value::as_str).unwrap_or("").to_string());
        let title = nz(l.get("title")).or_else(|| l.get("key")).cloned().unwrap_or(Value::Null);
        title_of.insert(key, title);
    }

    let before: Vec<Value> = stored
        .iter()
        .filter(|v| {
            let key = v.get("law_key").and_then(Value::as_str).unwrap_or("");
            detector_of.get(key).map(|d| CONTENT_DETECTORS.contains(&d.as_str())).unwrap_or(false)
        })
        .cloned()
        .collect();
    let unchecked = stored.len() as i64 - before.len() as i64;

    // THE TRUE TOTALS (the judge records `total_hits` per file and law in the snapshot's breakdown; rows are only the first HITS_CAP). A snapshot from before that has none:
    // a stored count BELOW the cap is still exact, a count AT the cap is only "at least" - the baseline is unknown, and the check says so instead of "no change".
    let totals: HashMap<String, i64> = match &snap {
        Some(sn) => {
            let snap_id = sn.get("id").cloned().unwrap_or(Value::Null);
            let rows = crate::db_json::query_rows(db, "SELECT breakdown_json FROM judgments.review_scores WHERE snapshot_id=? AND path=?", &[snap_id, json!(path.clone())])?;
            let mut m = HashMap::new();
            if let Some(b) = rows.as_array().and_then(|a| a.first()).and_then(|r| r.get("breakdown_json")).and_then(Value::as_str).and_then(|t| serde_json::from_str::<Value>(t).ok()) {
                if let Some(o) = b.as_object() {
                    for (k, v) in o {
                        let content_law = detector_of.get(k).map(|d| CONTENT_DETECTORS.contains(&d.as_str())).unwrap_or(false);
                        if let (true, Some(t)) = (content_law, v.get("total_hits").and_then(Value::as_i64)) { m.insert(k.clone(), t); }
                    }
                }
            }
            m
        }
        None => HashMap::new(),
    };
    let bc = cnt(&before);                      // stored ROWS per law (capped)
    let ac = cnt(&after_all);                   // EVERY hit now
    let exact_before = |k: &str| -> Option<i64> {
        if let Some(t) = totals.get(k) { return Some(*t); }
        let n = get_count(&bc, k);
        if (n as usize) < cap { Some(n) } else { None }
    };
    let mut law_keys: Vec<String> = Vec::new();
    for (k, _) in ac.iter().chain(bc.iter()) { if !law_keys.contains(k) { law_keys.push(k.clone()); } }
    for k in totals.keys() { if !law_keys.contains(k) { law_keys.push(k.clone()); } }

    let mut introduced: Vec<Value> = Vec::new();
    let mut unverifiable: Vec<Value> = Vec::new();
    for (k, n) in &ac {
        let title = nz(title_of.get(k)).cloned().unwrap_or_else(|| json!(k));
        match exact_before(k) {
            Some(bn) if *n > bn => {
                // where the law hits (the first four lines - which of them are the NEW ones cannot be told from counts alone)
                let lines: Vec<Value> = after_all.iter()
                    .filter(|v| v.get("law_key").and_then(Value::as_str) == Some(k.as_str()))
                    .filter_map(|v| v.get("line").cloned()).filter(|l| !l.is_null()).take(4).collect();
                introduced.push(json!({ "law_key": k, "title": title, "count": n - bn, "lines": lines }));
            }
            Some(_) => {}
            None if (*n as usize) >= cap => unverifiable.push(json!({ "law_key": k, "title": title, "now": n, "recorded": format!("at least {cap}") })),
            None => {}
        }
    }
    let mut fixed: Vec<Value> = Vec::new();
    for k in &law_keys {
        let an = get_count(&ac, k);
        let title = nz(title_of.get(k)).cloned().unwrap_or_else(|| json!(k));
        match exact_before(k) {
            Some(bn) if bn > an => fixed.push(json!({ "law_key": k, "title": title, "count": bn - an })),
            None if (an as usize) < cap => fixed.push(json!({ "law_key": k, "title": title, "count": cap as i64 - an, "at_least": true })),
            _ => {}
        }
    }

    // ── compliance: exact over what we re-measured; unchecked families carry over unchanged ──
    let total_pen = pen(&stored);
    let before_pen = pen(&before);
    let after_pen = pen(&after);

    let compliance_before = if !stored.is_empty() || snap.is_some() {
        json!((100 - total_pen.min(90)).max(10))
    } else {
        Value::Null
    };
    let diff = (total_pen - before_pen + after_pen).max(0).min(90);
    let after_est = (100 - diff).max(10);

    let checked_laws = checkable.len() as i64;
    let after_list: Vec<Value> = after_all.iter().take(30).cloned().collect();
    let before_count: i64 = law_keys.iter().map(|k| exact_before(k).unwrap_or_else(|| get_count(&bc, k))).sum();
    let after_count = after_all.len() as i64;

    let unverifiable_note = if unverifiable.is_empty() { String::new() } else {
        format!("CANNOT TELL for {}: this file's stored review kept only the first {cap} hits of the law and no total, so how many are new is unknown ({} now). Re-run the review (codebase rebuild) for an exact comparison.",
            unverifiable.iter().filter_map(|u| u.get("law_key").and_then(Value::as_str)).map(|k| format!("`{k}`")).collect::<Vec<_>>().join(", "),
            unverifiable.iter().filter_map(|u| u.get("now").and_then(Value::as_i64)).map(|n| n.to_string()).collect::<Vec<_>>().join("/"))
    };
    let note = if unchecked != 0 {
        json!(format!(
            "{unchecked} stored violation(s) are metric/graph/linter-scope — not re-measurable from text; assumed unchanged (the \u{2248})."
        ))
    } else {
        Value::Null
    };

    Ok(json!({
        "ok": true,
        "path": path,
        "checked_laws": checked_laws,
        "before": { "violations": before_count },
        "after": { "violations": after_count, "list": after_list },
        "fixed": fixed,
        "introduced": introduced,
        "unverifiable": unverifiable,
        "unchecked": unchecked,
        "compliance": { "before": compliance_before, "after_est": after_est },
        "note": if unverifiable_note.is_empty() { note } else { json!(match &note { Value::String(n) => format!("{n}\n{unverifiable_note}"), _ => unverifiable_note.clone() }) },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS laws; ATTACH ':memory:' AS judgments").unwrap();
        let _ = crate::constitution__helpers_schema::ensureConstitutionSchema(&c);
        c
    }

    fn seed_law(c: &Connection, key: &str, detector: &str, config: &str, weight: i64, confidence: f64, title: &str) {
        c.execute(
            "INSERT INTO laws.review_laws (scope, key, title, rule_md, detector, config_json, weight, confidence, enforcement, status) \
             VALUES ('project', ?, ?, 'r', ?, ?, ?, ?, 'score', 'ratified')",
            rusqlite::params![key, title, detector, config, weight, confidence],
        )
        .unwrap();
    }

    #[test]
    fn guard_throws_for_empty_path_and_content() {
        let c = conn();
        assert_eq!(
            constitutionCheck(&json!({ "path": "", "content": "x" }), &c).unwrap_err(),
            "constitution-check: path is required"
        );
        assert_eq!(
            constitutionCheck(&json!({ "path": "a.js", "content": "   " }), &c).unwrap_err(),
            "constitution-check: content is required"
        );
    }

    #[test]
    fn no_laws_no_snapshot_yields_null_before_compliance() {
        let c = conn();
        let r = constitutionCheck(&json!({ "path": "a.js", "content": "x" }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["checked_laws"], json!(0));
        assert_eq!(r["before"], json!({ "violations": 0 }));
        assert_eq!(r["after"], json!({ "violations": 0, "list": [] }));
        assert_eq!(r["compliance"]["before"], Value::Null); // no stored rows AND no snapshot
        assert_eq!(r["compliance"]["after_est"], json!(100));
        assert_eq!(r["unchecked"], json!(0));
        assert_eq!(r["note"], Value::Null);
    }

    #[test]
    fn regex_law_hits_the_candidate_text() {
        let c = conn();
        seed_law(&c, "no-console", "regex", &json!({"pattern":"console\\.log\\s*\\("}).to_string(), 5, 0.9, "No console.log");
        let r = constitutionCheck(&json!({ "path": "a.js", "content": "x();\nconsole.log(1);\n" }), &c).unwrap();
        assert_eq!(r["checked_laws"], json!(1));
        assert_eq!(r["after"]["violations"], json!(1));
        let hit = &r["after"]["list"][0];
        assert_eq!(hit["law_key"], json!("no-console"));
        assert_eq!(hit["line"], json!(2));
        assert_eq!(hit["title"], json!("No console.log"));
    }

    #[test]
    fn path_exclude_exception_suppresses_the_law() {
        let c = conn();
        seed_law(&c, "no-console", "regex", &json!({"pattern":"console\\.log"}).to_string(), 5, 0.9, "No console.log");
        c.execute("INSERT INTO laws.review_law_exceptions (law_key, path_glob, reason) VALUES ('no-console', 'legacy/**', 'grandfathered')", []).unwrap();
        let r = constitutionCheck(&json!({ "path": "legacy/old.js", "content": "console.log(1);" }), &c).unwrap();
        assert_eq!(r["checked_laws"], json!(0));
        assert_eq!(r["after"]["violations"], json!(0));
    }

    #[test]
    fn non_content_detector_is_not_checkable() {
        let c = conn();
        seed_law(&c, "big-file", "metric", &json!({"metric":"loc","max":200}).to_string(), 5, 0.9, "Loc limit");
        let r = constitutionCheck(&json!({ "path": "a.js", "content": "x();" }), &c).unwrap();
        assert_eq!(r["checked_laws"], json!(0));
    }

    #[test]
    fn stmt_detector_uses_real_analyze_tree_for_nested_loops() {
        let c = conn();
        seed_law(&c, "nested-loop", "stmt", &json!({"check":"nested-loop"}).to_string(), 4, 0.7, "Nested loop");
        let src = "function f() {\n  for (let i=0;i<n;i++) {\n    for (let j=0;j<n;j++) { x(); }\n  }\n}\n";
        let r = constitutionCheck(&json!({ "path": "a.js", "content": src }), &c).unwrap();
        assert_eq!(r["checked_laws"], json!(1));
        assert_eq!(r["after"]["violations"], json!(1));
        assert_eq!(r["after"]["list"][0]["line"], json!(3));
    }

    #[test]
    fn stored_snapshot_drives_before_and_fixed_and_introduced() {
        let c = conn();
        seed_law(&c, "no-console", "regex", &json!({"pattern":"console\\.log"}).to_string(), 5, 1.0, "No console.log");
        seed_law(&c, "no-eval", "regex", &json!({"pattern":"\\beval\\("}).to_string(), 3, 1.0, "No eval");
        c.execute(
            "INSERT INTO judgments.review_snapshots (constitution_hash, status) VALUES ('h', 'done')",
            [],
        )
        .unwrap();
        // stored: no-console hit once, no-eval hit once (this candidate FIXES no-eval, still has no-console)
        c.execute(
            "INSERT INTO judgments.review_violations (snapshot_id, path, law_key, line, weight, confidence) VALUES (1, 'a.js', 'no-console', 2, 5, 1.0)",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO judgments.review_violations (snapshot_id, path, law_key, line, weight, confidence) VALUES (1, 'a.js', 'no-eval', 9, 3, 1.0)",
            [],
        )
        .unwrap();
        let r = constitutionCheck(&json!({ "path": "a.js", "content": "console.log(1);\nconsole.log(2);\n" }), &c).unwrap();
        assert_eq!(r["before"]["violations"], json!(2));
        assert_eq!(r["after"]["violations"], json!(2)); // no-console now hits twice
        let fixed = r["fixed"].as_array().unwrap();
        assert_eq!(fixed.len(), 1);
        assert_eq!(fixed[0]["law_key"], json!("no-eval"));
        assert_eq!(fixed[0]["count"], json!(1));
        let introduced = r["introduced"].as_array().unwrap();
        assert_eq!(introduced.len(), 1);
        assert_eq!(introduced[0]["law_key"], json!("no-console"));
        assert_eq!(introduced[0]["count"], json!(1));
        assert_eq!(introduced[0]["lines"], json!([1, 2]));
        // compliance.before: pen(stored) = round(5*1*1) + round(3*1*1) = 8 -> 100-8=92
        assert_eq!(r["compliance"]["before"], json!(92));
    }

    /// A file whose stored review already holds the FULL cap of a law: the rows are the first five, the breakdown carries the true total.
    fn seed_capped_baseline(c: &Connection, stored_total: Option<i64>) {
        seed_law(c, "no-console", "regex", &json!({"pattern":"console\\.log"}).to_string(), 5, 1.0, "No console.log");
        c.execute("INSERT INTO judgments.review_snapshots (constitution_hash, status) VALUES ('h', 'done')", []).unwrap();
        for line in 1..=5 {
            c.execute("INSERT INTO judgments.review_violations (snapshot_id, path, law_key, line, weight, confidence) VALUES (1, 'a.js', 'no-console', ?, 5, 1.0)", [line]).unwrap();
        }
        let bd = match stored_total {
            Some(t) => json!({ "no-console": { "hits": 5, "total_hits": t, "weight": 5, "confidence": 1.0, "penalty": 25 } }),
            None => json!({ "no-console": { "hits": 5, "weight": 5, "confidence": 1.0, "penalty": 25 } }),   // a snapshot from before totals were recorded
        };
        c.execute("INSERT INTO judgments.review_scores (snapshot_id, path, score, breakdown_json) VALUES (1, 'a.js', 75, ?)", [bd.to_string()]).unwrap();
    }
    fn logs(n: usize) -> String { (1..=n).map(|i| format!("console.log({i});\n")).collect() }

    #[test]
    fn a_file_already_at_the_cap_still_shows_a_new_violation_when_the_review_recorded_its_total() {
        // THE BUG (law-check A/B, 2026-10-06): five stored hits and nine hits now read "before 5 -> after 5, no change" because the detector stopped counting at five
        let c = conn();
        seed_capped_baseline(&c, Some(7));
        let r = constitutionCheck(&json!({ "path": "a.js", "content": logs(9) }), &c).unwrap();
        assert_eq!(r["before"]["violations"], json!(7), "the recorded total, not the five rows");
        assert_eq!(r["after"]["violations"], json!(9), "every hit now, not five");
        let intro = r["introduced"].as_array().unwrap();
        assert_eq!(intro.len(), 1);
        assert_eq!(intro[0]["count"], json!(2), "9 now against 7 recorded");
        assert_eq!(r["unverifiable"], json!([]));
        assert!(!crate::constitution__helpers_review_markdown::check_md(&r).contains("no change in violations"));
        // and the same file with the same seven is no change at all
        let same = constitutionCheck(&json!({ "path": "a.js", "content": logs(7) }), &c).unwrap();
        assert_eq!(same["introduced"], json!([])); assert_eq!(same["fixed"], json!([]));
        assert!(crate::constitution__helpers_review_markdown::check_md(&same).contains("no change in violations"));
        // removing two is fixed x2
        let less = constitutionCheck(&json!({ "path": "a.js", "content": logs(5) }), &c).unwrap();
        assert_eq!(less["fixed"][0]["count"], json!(2));
    }

    #[test]
    fn a_snapshot_that_kept_only_the_capped_count_says_it_cannot_tell_instead_of_no_change() {
        let c = conn();
        seed_capped_baseline(&c, None);
        for n in [5usize, 9] {
            let r = constitutionCheck(&json!({ "path": "a.js", "content": logs(n) }), &c).unwrap();
            assert_eq!(r["introduced"], json!([]), "nothing is claimed that cannot be known");
            let u = r["unverifiable"].as_array().unwrap();
            assert_eq!(u.len(), 1, "n={n}");
            assert_eq!(u[0]["law_key"], json!("no-console"));
            assert_eq!(u[0]["now"], json!(n as i64));
            let md = crate::constitution__helpers_review_markdown::check_md(&r);
            assert!(md.contains("CANNOT TELL `no-console`") && !md.contains("no change in violations"), "{md}");
            assert!(r["note"].as_str().unwrap().contains("Re-run the review"));
        }
        // fewer than the cap now: at least (5 - 3) were fixed, whatever the true baseline was
        let fewer = constitutionCheck(&json!({ "path": "a.js", "content": logs(3) }), &c).unwrap();
        assert_eq!(fewer["fixed"][0]["count"], json!(2));
        assert_eq!(fewer["fixed"][0]["at_least"], json!(true));
        assert_eq!(fewer["unverifiable"], json!([]));
    }

    #[test]
    fn the_scored_view_stays_capped_so_compliance_matches_the_stored_penalty() {
        let c = conn();
        seed_capped_baseline(&c, Some(7));
        let r = constitutionCheck(&json!({ "path": "a.js", "content": logs(9) }), &c).unwrap();
        // pen(stored 5 rows) = 25, pen(after capped to 5) = 25: the estimate does not drift just because the check now COUNTS more than it SCORES
        assert_eq!(r["compliance"]["before"], json!(75));
        assert_eq!(r["compliance"]["after_est"], json!(75));
    }

    #[test]
    fn unchecked_counts_a_non_content_detector_stored_violation() {
        let c = conn();
        seed_law(&c, "no-console", "regex", &json!({"pattern":"console\\.log"}).to_string(), 5, 1.0, "No console.log");
        seed_law(&c, "god-file", "graph", &json!({"smell":"god-file"}).to_string(), 6, 0.9, "God file");
        c.execute("INSERT INTO judgments.review_snapshots (constitution_hash, status) VALUES ('h', 'done')", []).unwrap();
        c.execute(
            "INSERT INTO judgments.review_violations (snapshot_id, path, law_key, line, weight, confidence) VALUES (1, 'a.js', 'god-file', NULL, 6, 0.9)",
            [],
        )
        .unwrap();
        let r = constitutionCheck(&json!({ "path": "a.js", "content": "x();" }), &c).unwrap();
        assert_eq!(r["before"]["violations"], json!(0)); // graph is not a CONTENT_DETECTOR
        assert_eq!(r["unchecked"], json!(1));
        assert!(r["note"].as_str().unwrap().contains("1 stored violation(s)"));
    }
}

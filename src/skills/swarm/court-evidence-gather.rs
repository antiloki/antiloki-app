//! court-evidence-gather.rs — twin of skills/swarm/court-evidence-gather.mjs (db lane). NAMES ARE
//! CONTRACT: `courtEvidenceGather` (default), `renderEvidence` (named, no-db helper). Assembles the
//! QA court's deterministic evidence bundle (card + contract + blueprint + PL screening + Wave G
//! codebase context + stack facts) into `corpus` + a citable `evidence_md`, and — when a scope is
//! given — persists it into `judgments.court_evidence` (upsert, keyed by judgment_id).
// QUALITY: cross-module (blueprint_block_shape, swarm_judgment_id, codebase_codebase_context,
// codebase_stack_facts) + cross-db (judgments.court_evidence, ATTACH lane).

use rusqlite::Connection;
use serde_json::{json, Map, Value};

// ── standard JS coercion helpers (playbook-standard set) ───────────────────
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
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(x) => str_qq(Some(x)),
    }
}
fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null())
}
// `.slice(0, n)` on a string (JS is UTF-16; fixtures stay ASCII/BMP).
fn take_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

// body_json (or any JSON-text column) → parsed value, or `fallback` on parse failure / literal null.
// JS: `function safe(j, fb) { try { const v = JSON.parse(j ?? ''); return v == null ? fb : v; } catch { return fb; } }`
fn safe_json(j: Option<&str>, fallback: Value) -> Value {
    match serde_json::from_str::<Value>(j.unwrap_or("")) {
        Ok(v) if !v.is_null() => v,
        _ => fallback,
    }
}
// body_json may be double-encoded (caller stringified before storage) — unwrap once.
fn unwrap_body(v: Value) -> Value {
    if let Value::String(s) = &v {
        let p = safe_json(Some(s.as_str()), Value::Null);
        if !p.is_null() && (p.is_object() || p.is_array()) {
            return p;
        }
    }
    v
}
fn col_str<'a>(row: &'a Value, key: &str) -> Option<&'a str> {
    row.get(key).and_then(|v| v.as_str())
}
fn query_one(db: &Connection, sql: &str, params: &[Value]) -> Result<Option<Value>, String> {
    let rows = crate::db_json::query_rows(db, sql, params)?;
    Ok(rows.as_array().and_then(|a| a.first()).cloned())
}

/// JS: `export default function courtEvidenceGather(input, context)`.
pub fn courtEvidenceGather(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const { db } = context ?? {}; if (!db) throw err.contextMissing(...)` — unreachable
    // here: Rust's `db: &Connection` is a required, non-nullable reference (same convention as
    // every sibling db-lane twin).
    let slug_v = input.get("contract_slug").cloned().unwrap_or(Value::Null);
    if !truthy(Some(&slug_v)) {
        return Err("court-evidence-gather: contract_slug is required".into());
    }

    let contract = query_one(
        db,
        "SELECT slug, card_slug, acceptance_json, microtasks_json, files_touched_json FROM contracts WHERE slug = ?",
        &[slug_v.clone()],
    )?;
    let contract = match contract {
        Some(c) => c,
        None => return Err(format!("court-evidence-gather: contract '{}' not found", tmpl(Some(&slug_v)))),
    };

    // ── CARD: body fields (why/kind/motivation/…) + acceptance features ──
    let contract_card_slug = contract.get("card_slug").cloned().unwrap_or(Value::Null);
    let mut card: Value = Value::Null;
    if truthy(Some(&contract_card_slug)) {
        let card_row = query_one(db, "SELECT slug, title, body_json, acceptance_json FROM cards WHERE slug = ?", &[contract_card_slug.clone()])?;
        if let Some(row) = card_row {
            let body = unwrap_body(safe_json(col_str(&row, "body_json"), json!({})));
            let mut fields = Map::new();
            if let Value::Object(obj) = &body {
                for (k, v) in obj {
                    let fv = match v {
                        Value::String(s) => s.clone(),
                        other => serde_json::to_string(other).unwrap_or_default(),
                    };
                    fields.insert(k.clone(), json!(fv));
                }
            }
            card = json!({
                "slug": row.get("slug").cloned().unwrap_or(Value::Null),
                "title": row.get("title").cloned().unwrap_or(Value::Null),
                "fields": Value::Object(fields),
                "acceptance": safe_json(col_str(&row, "acceptance_json"), json!([])),
            });
        }
    }

    // ── CONTRACT: numbered acceptance + plan microtasks + files ──
    let acceptance = safe_json(col_str(&contract, "acceptance_json"), json!([]));
    let microtasks = safe_json(col_str(&contract, "microtasks_json"), json!([]));
    let files_touched = safe_json(col_str(&contract, "files_touched_json"), json!([]));
    let empty: Vec<Value> = Vec::new();
    // contract_md: the line-numbered acceptance the EXISTING contract:<line> grounding resolves against.
    let contract_md = acceptance
        .as_array()
        .unwrap_or(&empty)
        .iter()
        .map(|a| {
            let is_test = a.get("kind").and_then(|v| v.as_str()) == Some("test");
            let val = if is_test { nz(a.get("cmd")).or_else(|| nz(a.get("command"))) } else { nz(a.get("intent")).or_else(|| nz(a.get("text"))) };
            match val {
                Some(v) => js_string(v),
                None => serde_json::to_string(a).unwrap_or_default(),
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    // ── TL BLUEPRINT (active design) → recursive block tree for grounding.
    // normalize-on-read: default the {blocks,bindings,coverage} arrays if any are missing. ──
    let mut blueprint: Value = Value::Null;
    let mut blocks: Value = json!([]);
    let bp_row = query_one(
        db,
        "SELECT id, version, lint_passed, blueprint_json FROM blueprints WHERE contract_slug = ? AND superseded_by IS NULL ORDER BY id DESC LIMIT 1",
        &[slug_v.clone()],
    )?;
    if let Some(row) = bp_row {
        let bp = crate::blueprint_block_shape::normalizeBlueprint(&safe_json(col_str(&row, "blueprint_json"), json!({})));
        let lint_passed = matches!(row.get("lint_passed").and_then(|v| v.as_i64()), Some(1));
        let bindings = bp.get("bindings").filter(|v| v.is_array()).cloned().unwrap_or(json!([]));
        let coverage = bp.get("coverage").filter(|v| v.is_array()).cloned().unwrap_or(json!([]));
        blueprint = json!({
            "id": row.get("id").cloned().unwrap_or(Value::Null),
            "version": row.get("version").cloned().unwrap_or(Value::Null),
            "lint_passed": lint_passed,
            "bindings": bindings,
            "coverage": coverage,
        });
        blocks = bp.get("blocks").filter(|v| v.is_array()).cloned().unwrap_or(json!([]));
    }

    // ── PL SCREENING (refine: enriched context + S-tasks) ──
    let mut screening: Value = Value::Null;
    if let Ok(Some(row)) = query_one(db, "SELECT screening_json FROM pl_screenings WHERE card_slug = ? ORDER BY id DESC LIMIT 1", &[contract_card_slug.clone()]) {
        let s = safe_json(col_str(&row, "screening_json"), Value::Null);
        if s.is_object() {
            screening = s;
        }
    }
    // pre-v2 DB without pl_screenings: query_one's Err is swallowed by the `if let Ok(...)` above.

    let snippets = input.get("snippets").filter(|v| v.is_array()).cloned().unwrap_or(json!([]));
    // ── CODEBASE (Wave G): the REAL existing code these files live in (atlas slice + blast
    // radius), so a claim about a regression grounds against reality, not a guess. Additive —
    // degrades to null when the atlas has nothing for them; the court argues exactly as before. ──
    let mut codebase_context: Value = Value::Null;
    if let Ok(cc) = crate::codebase_codebase_context::codebaseContext(&json!({ "paths": files_touched, "hops": 1 }), db) {
        if cc.get("grounded") == Some(&Value::Bool(true)) {
            codebase_context = cc;
        }
    }
    // STACK & CONVENTIONS (context-bundle stack-facts — used SYNC here because the court runs sync; we take
    // just this one bone item, not the async resolver). The antidote to "the existing Redux store"-class
    // regressions the court would otherwise argue from a guessed stack. Additive; null off-manifest.
    let mut stack_facts: Value = Value::Null;
    if let Ok(sf) = crate::codebase_stack_facts::stackFacts(&json!({}), db) {
        if sf.get("found") == Some(&Value::Bool(true)) {
            stack_facts = sf.get("facts_md").cloned().unwrap_or(Value::Null);
        }
    }
    let corpus = json!({
        "contract_md": contract_md,
        "microtasks": microtasks,
        "snippets": snippets,
        "files_touched": files_touched,
        "card": card,
        "blueprint": blueprint,
        "blocks": blocks,
        "screening": screening,
        "codebase_context": codebase_context,
        "stack_facts": stack_facts,
    });
    let evidence_md = renderEvidence(&corpus);
    // file blocks (top level) + nested blocks, for the persisted summary counts.
    let file_blocks = blocks.as_array().map(|a| a.len()).unwrap_or(0) as i64;
    let nested_blocks = crate::blueprint_block_shape::flattenBlocks(&blocks).as_object().map(|m| m.len()).unwrap_or(0) as i64 - file_blocks;

    // GENERATE THE JSON OF EVIDENCES — persist the bundle as the court's durable
    // research-of-record, BEFORE the debate, keyed by judgment_id (one per judgment;
    // a re-gather upserts). Only when a scope is given (review passes it); the
    // compute-only call (no scope) just returns the bundle. A read-only / absent
    // judgments DB degrades to no-write (the in-memory bundle still flows on).
    let mut judgment_id: Value = Value::Null;
    let mut persisted = false;
    let scope_kind = str_qq(input.get("scope_kind")).to_lowercase();
    let scope_ref = nz(input.get("scope_ref")).cloned().unwrap_or(json!(""));
    let scope_kind_valid = crate::swarm_judgment_id::SCOPE_KINDS.as_array().map(|a| a.iter().any(|v| v.as_str() == Some(scope_kind.as_str()))).unwrap_or(false);
    let persist_flag = !matches!(input.get("persist"), Some(Value::Bool(false)));
    if scope_kind_valid && persist_flag {
        let card_slug_for_id = nz(card.get("slug")).cloned().unwrap_or_else(|| slug_v.clone());
        let jid = crate::swarm_judgment_id::makeJudgmentId(&card_slug_for_id, &json!(scope_kind), &scope_ref)?;
        judgment_id = json!(jid);
        // n_blocks/n_nested_blocks/has_blueprint populated from the block tree —
        // file blocks + nested blocks.
        let corpus_json = serde_json::to_string(&corpus).map_err(|e| e.to_string())?;
        let params = [
            json!(jid),
            slug_v.clone(),
            json!(scope_kind),
            scope_ref.clone(),
            json!(corpus_json),
            evidence_md.clone(),
            json!(acceptance.as_array().map(|a| a.len()).unwrap_or(0) as i64),
            json!(microtasks.as_array().map(|a| a.len()).unwrap_or(0) as i64),
            json!(file_blocks),
            json!(nested_blocks),
            json!(if card.is_null() { 0 } else { 1 }),
            json!(if blueprint.is_null() { 0 } else { 1 }),
            json!(if screening.is_null() { 0 } else { 1 }),
        ];
        let sql = &format!("
        INSERT INTO judgments.court_evidence
          (judgment_id, contract_slug, scope_kind, scope_ref, corpus_json, evidence_md,
           n_acceptance, n_microtasks, n_blocks, n_nested_blocks, has_card, has_blueprint, has_screening)
        VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)
        ON CONFLICT(judgment_id) DO UPDATE SET
          corpus_json=excluded.corpus_json, evidence_md=excluded.evidence_md,
          n_acceptance=excluded.n_acceptance, n_microtasks=excluded.n_microtasks,
          n_blocks=excluded.n_blocks, n_nested_blocks=excluded.n_nested_blocks,
          has_card=excluded.has_card, has_blueprint=excluded.has_blueprint,
          has_screening=excluded.has_screening, created_at={}
        ", crate::clock::now_sql());
        if crate::db_json::run_write(db, sql, &params).is_ok() {
            persisted = true;
        }
        // judgments DB absent / read-only — return the bundle anyway (persisted stays false).
    }

    Ok(json!({
        "contract_slug": slug_v,
        "judgment_id": judgment_id,
        "persisted": persisted,
        "corpus": corpus,
        "evidence_md": evidence_md,
    }))
}

/// JS: `export function renderEvidence(corpus)` — no-db helper. Renders the bundle as the
/// EVIDENCE block the debaters + judge read, every section labelled with the exact `kind:<ref>`
/// a claim must cite to ground against it.
pub fn renderEvidence(corpus: &Value) -> Value {
    let mut l: Vec<String> = Vec::new();
    if let Some(card) = corpus.get("card").filter(|v| !v.is_null()) {
        l.push("═══ CARD (cite as card:<field>) ═══".to_string());
        l.push(format!("card:slug → {}", tmpl(card.get("slug"))));
        l.push(format!("card:title → {}", tmpl(card.get("title"))));
        if let Some(Value::Object(fields)) = card.get("fields") {
            for (k, v) in fields {
                l.push(format!("card:{k} → {}", take_chars(&str_qq(Some(v)), 300)));
            }
        }
        if let Some(a) = card.get("acceptance").and_then(|v| v.as_array()).filter(|a| !a.is_empty()) {
            l.push("card acceptance features:".to_string());
            for f in a {
                l.push(format!("  - {}: {}", tmpl(f.get("id")), tmpl(f.get("intent"))));
            }
        }
        l.push(String::new());
    }
    l.push("═══ CONTRACT ACCEPTANCE (cite as contract:<line>) ═══".to_string());
    for (i, line) in str_qq(corpus.get("contract_md")).split('\n').enumerate() {
        l.push(format!("{}: {line}", i + 1));
    }
    if let Some(mt) = corpus.get("microtasks").and_then(|v| v.as_array()).filter(|a| !a.is_empty()) {
        l.push(String::new());
        l.push("═══ PLAN microtasks (cite as microtask:<id>) ═══".to_string());
        for m in mt {
            let id = nz(m.get("id")).or_else(|| nz(m.get("microtask_id")));
            let file = str_qq(nz(m.get("file")));
            let suffix = if truthy(m.get("file_spec_md")) { format!(" — {}", take_chars(&js_string(m.get("file_spec_md").unwrap()), 120)) } else { String::new() };
            l.push(format!("- {}: {file}{suffix}", tmpl(id)));
        }
    }
    if let Some(bp) = corpus.get("blueprint").filter(|v| !v.is_null()) {
        l.push(String::new());
        let lint_state = if truthy(bp.get("lint_passed")) { "PASS" } else { "FAIL" };
        l.push(format!("═══ BLUEPRINT DESIGN v{} (lint {lint_state}) — cite any node as block:<id> ═══", tmpl(bp.get("version"))));
        let blocks = corpus.get("blocks").cloned().unwrap_or(json!([]));
        if let Value::Object(flat) = crate::blueprint_block_shape::flattenBlocks(&blocks) {
            for (id, entry) in &flat {
                let block = entry.get("block").cloned().unwrap_or(Value::Null);
                let btype = match nz(block.get("type")) {
                    Some(v) => str_qq(Some(v)),
                    None => "?".to_string(),
                };
                let intent = take_chars(&str_qq(nz(block.get("intent"))), 140);
                l.push(format!("• block:{id} [{btype}] — {intent}"));
            }
        }
        // FIX-COURT-SEES-COVERAGE (meli r11): the design's OWN acceptance→blocks map is its answer
        // to "how is line N covered?" — omitting it convicted correct designs ("fails to specify an
        // aria-label") when the coverage said "ac12 → ClickMeButton block; the CODE phase implements
        // it". Body-level details (attributes, calls) are judged in the code court; coverage of an
        // acceptance line is judged HERE, against this map + the block intents.
        if let Some(cov) = bp.get("coverage").and_then(|v| v.as_array()).filter(|a| !a.is_empty()) {
            l.push(String::new());
            l.push("═══ DESIGN COVERAGE — the design's acceptance→blocks map (an acceptance line is COVERED when a block claims it; body-level implementation happens in the CODE phase) ═══".to_string());
            for c in cov {
                let maps: Vec<String> = c.get("maps_to_blocks").and_then(|v| v.as_array()).map(|a| a.iter().map(js_string).collect()).unwrap_or_default();
                let joined = maps.join(", ");
                let display = if joined.is_empty() { "(no block!)".to_string() } else { joined };
                l.push(format!("coverage:{} → {display}", tmpl(c.get("acceptance_id"))));
            }
        }
    }
    if let Some(s) = corpus.get("snippets").and_then(|v| v.as_array()).filter(|a| !a.is_empty()) {
        l.push(String::new());
        l.push("═══ AUTHORED FILES (cite as snippet:<path>) ═══".to_string());
        for sn in s {
            l.push(format!("### {}\n{}", tmpl(sn.get("path")), tmpl(sn.get("content"))));
        }
    }
    if let Some(Value::Object(sc)) = corpus.get("screening").filter(|v| !v.is_null()) {
        l.push(String::new());
        l.push("═══ PL SCREENING (cite as screening:<key>) ═══".to_string());
        for (k, v) in sc {
            let sval = match v {
                Value::String(s) => s.clone(),
                other => serde_json::to_string(other).unwrap_or_default(),
            };
            l.push(format!("screening:{k} → {}", take_chars(&sval, 400)));
        }
    }
    let cc_md = corpus.get("codebase_context").and_then(|v| v.get("context_md"));
    if truthy(cc_md) {
        l.push(String::new());
        l.push("═══ CODEBASE — the REAL existing code these files live in (cite a file as codebase:<path>) ═══".to_string());
        l.push(tmpl(cc_md));
    }
    let sf = corpus.get("stack_facts");
    if truthy(sf) {
        l.push(String::new());
        l.push("═══ STACK & CONVENTIONS (the real project — do NOT argue a regression against tech that is ABSENT here) ═══".to_string());
        l.push(tmpl(sf));
    }
    json!(l.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const BP_JSON: &str = r#"{"blocks":[{"id":"src/filter.ts","type":"file","path":"src/filter.ts","intent":"filter males>30","children":[{"id":"src/filter.ts.f","type":"function","signature":"f(u): User[]","intent":"filter users by age","children":[]}]}],"bindings":[{"from":"src/filter.ts","to":"src/main.ts"}],"coverage":[{"acceptance_id":"ac1","maps_to_blocks":["src/filter.ts"]}]}"#;

    fn seeded_db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS judgments;
             CREATE TABLE contracts (slug TEXT, card_slug TEXT, acceptance_json TEXT, microtasks_json TEXT, files_touched_json TEXT);
             CREATE TABLE cards (slug TEXT, title TEXT, body_json TEXT, acceptance_json TEXT);
             CREATE TABLE blueprints (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT, version INTEGER, blueprint_json TEXT, lint_passed INTEGER, superseded_by INTEGER);
             CREATE TABLE pl_screenings (id INTEGER PRIMARY KEY AUTOINCREMENT, card_slug TEXT, screening_json TEXT);
             CREATE TABLE code_nodes (id TEXT, origin TEXT, path TEXT, type TEXT, name TEXT, signature TEXT, intent TEXT, parent_id TEXT);
             CREATE TABLE code_edges (origin TEXT, from_node TEXT, to_node TEXT, type TEXT, kind TEXT);
             CREATE TABLE judgments.court_evidence (
               judgment_id TEXT PRIMARY KEY, contract_slug TEXT NOT NULL, scope_kind TEXT NOT NULL DEFAULT '',
               scope_ref TEXT NOT NULL DEFAULT '', corpus_json TEXT NOT NULL, evidence_md TEXT,
               n_acceptance INTEGER NOT NULL DEFAULT 0, n_microtasks INTEGER NOT NULL DEFAULT 0,
               n_blocks INTEGER NOT NULL DEFAULT 0, n_nested_blocks INTEGER NOT NULL DEFAULT 0,
               has_card INTEGER NOT NULL DEFAULT 0, has_blueprint INTEGER NOT NULL DEFAULT 0,
               has_screening INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')));",
        )
        .unwrap();
        c
    }
    fn seed(c: &Connection) {
        c.execute(
            "INSERT INTO cards (slug, title, body_json, acceptance_json) VALUES ('CARD-E','filter card',?1,?2)",
            rusqlite::params![
                r#"{"why":"users need an age filter","kind":"feature"}"#,
                r#"[{"id":"f1","intent":"only adults over 30"}]"#
            ],
        )
        .unwrap();
        c.execute(
            "INSERT INTO contracts (slug, card_slug, acceptance_json, microtasks_json, files_touched_json) VALUES ('CARD-E-CONTRACT-1','CARD-E',?1,?2,?3)",
            rusqlite::params![
                r#"[{"id":"ac1","kind":"file-exists","path":"src/filter.ts","intent":"filter exists"}]"#,
                r#"[{"id":"m1","file":"src/filter.ts","file_spec_md":"write the filter"}]"#,
                r#"["src/filter.ts"]"#
            ],
        )
        .unwrap();
        c.execute(
            "INSERT INTO blueprints (contract_slug, version, blueprint_json, lint_passed) VALUES ('CARD-E-CONTRACT-1', 2, ?1, 1)",
            rusqlite::params![BP_JSON],
        )
        .unwrap();
        c.execute(
            "INSERT INTO pl_screenings (card_slug, screening_json) VALUES ('CARD-E', ?1)",
            rusqlite::params![r#"{"enriched_context":"the codebase uses TS strict","s_tasks":["add a unit test"]}"#],
        )
        .unwrap();
    }

    #[test]
    fn gathers_card_contract_blueprint_screening_into_one_corpus_plus_cited_evidence_md() {
        let c = seeded_db();
        seed(&c);
        let r = courtEvidenceGather(&json!({ "contract_slug": "CARD-E-CONTRACT-1" }), &c).unwrap();
        assert_eq!(r["contract_slug"], json!("CARD-E-CONTRACT-1"));
        assert_eq!(r["corpus"]["card"]["slug"], json!("CARD-E"));
        assert_eq!(r["corpus"]["card"]["fields"]["why"], json!("users need an age filter"));
        assert_eq!(r["corpus"]["card"]["acceptance"][0]["id"], json!("f1"));
        assert!(r["corpus"]["contract_md"].as_str().unwrap().contains("filter exists"));
        assert_eq!(r["corpus"]["microtasks"][0]["id"], json!("m1"));
        assert_eq!(r["corpus"]["files_touched"], json!(["src/filter.ts"]));
        assert_eq!(r["corpus"]["blueprint"]["version"], json!(2));
        assert_eq!(r["corpus"]["blocks"][0]["path"], json!("src/filter.ts"));
        assert_eq!(r["corpus"]["blocks"][0]["type"], json!("file"));
        assert_eq!(r["corpus"]["blocks"][0]["children"][0]["id"], json!("src/filter.ts.f"));
        assert_eq!(r["corpus"]["screening"]["enriched_context"], json!("the codebase uses TS strict"));
        let md = r["evidence_md"].as_str().unwrap();
        for probe in ["card:why", "contract:", "block:src/filter.ts", "block:src/filter.ts.f", "screening:"] {
            assert!(md.contains(probe), "evidence_md should mention {probe}");
        }
    }

    #[test]
    fn grounds_in_the_real_codebase_wave_g() {
        let c = seeded_db();
        seed(&c);
        c.execute_batch(
            "INSERT INTO code_nodes (id, origin, path, type, name, signature, parent_id) VALUES
               ('src/filter.ts@atlas','atlas','src/filter.ts','file','src/filter.ts',NULL,NULL),
               ('src/main.ts@atlas','atlas','src/main.ts','file','src/main.ts',NULL,NULL),
               ('src/filter.ts#f','atlas','src/filter.ts','function','f','f(u): User[]','src/filter.ts@atlas');
             INSERT INTO code_edges (origin, from_node, to_node, type) VALUES ('atlas','src/main.ts@atlas','src/filter.ts@atlas','imports');",
        )
        .unwrap();
        let r = courtEvidenceGather(&json!({ "contract_slug": "CARD-E-CONTRACT-1" }), &c).unwrap();
        assert_eq!(r["corpus"]["codebase_context"]["grounded"], json!(true));
        let md = r["evidence_md"].as_str().unwrap();
        assert!(md.contains("═══ CODEBASE"), "evidence_md exposes the codebase section");
        assert!(md.contains("f(u): User[]"), "real signature is in the evidence");
        assert!(md.contains("imported by") && md.contains("src/main.ts"), "blast radius (importer) shown");
    }

    #[test]
    fn no_atlas_codebase_context_null_additive() {
        let c = seeded_db();
        seed(&c);
        let r = courtEvidenceGather(&json!({ "contract_slug": "CARD-E-CONTRACT-1" }), &c).unwrap();
        assert_eq!(r["corpus"]["codebase_context"], Value::Null);
        assert!(!r["evidence_md"].as_str().unwrap().contains("═══ CODEBASE"));
    }

    #[test]
    fn degrades_cleanly_when_card_blueprint_screening_are_absent() {
        let c = seeded_db();
        c.execute(
            "INSERT INTO contracts (slug, acceptance_json) VALUES ('BARE-CONTRACT-1', ?1)",
            rusqlite::params![r#"[{"id":"ac1","kind":"test","cmd":"npm test"}]"#],
        )
        .unwrap();
        let r = courtEvidenceGather(&json!({ "contract_slug": "BARE-CONTRACT-1" }), &c).unwrap();
        assert_eq!(r["corpus"]["card"], Value::Null);
        assert_eq!(r["corpus"]["blueprint"], Value::Null);
        assert_eq!(r["corpus"]["screening"], Value::Null);
        assert_eq!(r["corpus"]["blocks"], json!([]));
        assert!(r["corpus"]["contract_md"].as_str().unwrap().contains("npm test"));
    }

    #[test]
    fn with_a_scope_it_persists_court_evidence_and_upserts() {
        let c = seeded_db();
        seed(&c);
        let r = courtEvidenceGather(&json!({ "contract_slug": "CARD-E-CONTRACT-1", "scope_kind": "blueprint", "scope_ref": "design-v2" }), &c).unwrap();
        assert_eq!(r["persisted"], json!(true));
        assert_eq!(r["judgment_id"], json!("CARD-E::blueprint:design-v2"));
        let row = crate::db_json::query_rows(
            &c,
            "SELECT corpus_json, evidence_md, n_blocks, n_acceptance, has_card, has_blueprint, has_screening FROM judgments.court_evidence WHERE judgment_id=?",
            &[r["judgment_id"].clone()],
        )
        .unwrap();
        let row = &row[0];
        assert_eq!(row["n_blocks"], json!(1));
        assert_eq!(row["has_card"], json!(1));
        assert_eq!(row["has_blueprint"], json!(1));
        assert_eq!(row["has_screening"], json!(1));
        let corpus: Value = serde_json::from_str(row["corpus_json"].as_str().unwrap()).unwrap();
        assert_eq!(corpus["card"]["fields"]["why"], json!("users need an age filter"));
        assert_eq!(corpus["blocks"][0]["path"], json!("src/filter.ts"));
        // re-gather upserts the same row (one bundle per judgment)
        courtEvidenceGather(&json!({ "contract_slug": "CARD-E-CONTRACT-1", "scope_kind": "blueprint", "scope_ref": "design-v2" }), &c).unwrap();
        let cnt = crate::db_json::query_rows(&c, "SELECT count(*) n FROM judgments.court_evidence WHERE judgment_id=?", &[r["judgment_id"].clone()]).unwrap();
        assert_eq!(cnt[0]["n"], json!(1));
    }

    #[test]
    fn compute_only_no_scope_does_not_persist() {
        let c = seeded_db();
        seed(&c);
        let r = courtEvidenceGather(&json!({ "contract_slug": "CARD-E-CONTRACT-1" }), &c).unwrap();
        assert_eq!(r["persisted"], json!(false));
        assert_eq!(r["judgment_id"], Value::Null);
        let cnt = crate::db_json::query_rows(&c, "SELECT count(*) n FROM judgments.court_evidence", &[]).unwrap();
        assert_eq!(cnt[0]["n"], json!(0));
    }

    #[test]
    fn missing_contract_is_not_found() {
        let c = seeded_db();
        let err = courtEvidenceGather(&json!({ "contract_slug": "GHOST" }), &c).unwrap_err();
        assert!(err.contains("contract"), "{err}");
    }
}

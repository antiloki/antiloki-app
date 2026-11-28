//! law-add.rs — twin of skills/entities/law-add.mjs (db WRITE lane, cross-db laws via ATTACH).
//! NAMES ARE CONTRACT: `lawAdd` (default). Validates kind/enforcement/clause_type against the
//! canonical enums, refuses a duplicate active slug (ANY existing active row with the slug — the
//! JS source never actually compares content despite the module docstring's aspirational claim),
//! INSERTs the law row plus any clauses in the same case, and returns the freshly-inserted row
//! (SELECT * WHERE id = ?).
use rusqlite::Connection;
use serde_json::{json, Value};

const ALLOWED_KINDS: [&str; 6] = ["constitutional", "procedural", "permission", "constraint", "decision", "principle"];
const ALLOWED_ENFORCEMENT: [&str; 3] = ["hard", "soft", "archived"];
const ALLOWED_CLAUSE_TYPES: [&str; 5] = ["exception", "condition", "example", "antipattern", "consequence"];

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } }
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }

// FAIL-CLOSED GUARD (2026-06-09, JS: findSkillPath — a live recursive scan of antiloki-core's own
// skills/ tree, mirroring load-skill.mjs's buildIndex). Only reached when enforcement=='hard' AND
// enforcement_hook is truthy AND allow_pending_hook !== true — a combination no fixture case in
// this twin's suite exercises (guard-throws are not fixture-tested, per the crossing playbook: the
// resolution requires the REAL antiloki-core checkout on disk, which the JSON-fixture harness has
// no way to seed/control). Implemented faithfully anyway so the twin stays behaviorally complete;
// its exact result never enters a byte-identical comparison.
fn hook_resolves(name: &str) -> bool {
    fn scan(dir: &std::path::Path, name: &str) -> bool {
        let entries = match std::fs::read_dir(dir) { Ok(e) => e, Err(_) => return false };
        for entry in entries.flatten() {
            let fname = entry.file_name().to_string_lossy().into_owned();
            let path = entry.path();
            if path.is_dir() {
                if fname.starts_with('.') || fname.starts_with('_') { continue; }
                if scan(&path, name) { return true; }
            } else if let Some(base) = fname.strip_suffix(".rs") {
                if base == name { return true; }
            }
        }
        false
    }
    // This crate's own tree. It used to look one directory UP and sideways into an antiloki-core
    // checkout that no longer exists — which would have made every hook-skill existence check
    // answer "missing" and every hard law fail to register.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join("skills");
    scan(&root, name)
}

pub fn lawAdd(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const { db, actor = 'unknown' } = context ?? {}` — the db lane's context object carries
    // only `db` (no `actor` key), so `actor` is always its default 'unknown' in this harness.
    let slug = input.get("slug").cloned().unwrap_or(Value::Null);
    if !truthy(Some(&slug)) { return Err("law-add: slug is required".into()); }

    let kind = input.get("kind").cloned().unwrap_or(Value::Null);
    if !truthy(Some(&kind)) { return Err("law-add: kind is required".into()); }
    if kind.as_str().map_or(true, |k| !ALLOWED_KINDS.contains(&k)) {
        return Err(format!("law-add: kind must be one of: {} (got '{}')", ALLOWED_KINDS.join(", "), tmpl(Some(&kind))));
    }

    let domain = input.get("domain").cloned().unwrap_or(Value::Null);
    if !truthy(Some(&domain)) { return Err("law-add: domain is required".into()); }
    let title = input.get("title").cloned().unwrap_or(Value::Null);
    if !truthy(Some(&title)) { return Err("law-add: title is required".into()); }
    let statement = input.get("statement").cloned().unwrap_or(Value::Null);
    if !truthy(Some(&statement)) { return Err("law-add: statement is required".into()); }
    let rationale = input.get("rationale").cloned().unwrap_or(Value::Null);
    if !truthy(Some(&rationale)) { return Err("law-add: rationale is required".into()); }

    // `enforcement = 'soft'` — the JS destructure default fires only when the key is ABSENT
    // (an explicit `enforcement: null` stays null and fails the enum check below, same as JS).
    let enforcement = match input.get("enforcement") { Some(v) => v.clone(), None => json!("soft") };
    if enforcement.as_str().map_or(true, |e| !ALLOWED_ENFORCEMENT.contains(&e)) {
        return Err(format!("law-add: enforcement must be one of: {} (got '{}')", ALLOWED_ENFORCEMENT.join(", "), tmpl(Some(&enforcement))));
    }
    // `enforcement_hook = null` / `trigger_sql = null` / `source_card = null` / `source_event = null`
    // / `source_memory = null` — an absent key and an explicit null both collapse to Value::Null,
    // so no Some/None distinction is needed for these (unlike enforcement/established_by above/below,
    // whose defaults are non-null).
    let enforcement_hook = input.get("enforcement_hook").cloned().unwrap_or(Value::Null);
    let trigger_sql = input.get("trigger_sql").cloned().unwrap_or(Value::Null);
    // `established_by = actor` (== 'unknown' in this harness) — default only on an ABSENT key.
    let established_by = match input.get("established_by") { Some(v) => v.clone(), None => json!("unknown") };
    let source_card = input.get("source_card").cloned().unwrap_or(Value::Null);
    let source_event = input.get("source_event").cloned().unwrap_or(Value::Null);
    let source_memory = input.get("source_memory").cloned().unwrap_or(Value::Null);

    // `clauses = []` — default only on an ABSENT key (explicit null fails Array.isArray below).
    let clauses_v = match input.get("clauses") { Some(v) => v.clone(), None => json!([]) };
    let clauses = match clauses_v.as_array() {
        Some(a) => a.clone(),
        None => return Err("law-add: clauses must be an array".into()),
    };
    for c in &clauses {
        let ctype = c.get("clause_type");
        if !truthy(ctype) || ctype.and_then(|v| v.as_str()).map_or(true, |s| !ALLOWED_CLAUSE_TYPES.contains(&s)) {
            return Err(format!("law-add: clauses[].clause_type must be one of: {} (got '{}')", ALLOWED_CLAUSE_TYPES.join(", "), tmpl(ctype)));
        }
        if !truthy(c.get("text")) { return Err("law-add: clauses[].text is required".into()); }
    }

    // FAIL-CLOSED GUARD (2026-06-09) — see hook_resolves() above.
    let allow_pending_hook = matches!(input.get("allow_pending_hook"), Some(Value::Bool(true)));
    if enforcement.as_str() == Some("hard") && truthy(Some(&enforcement_hook)) && !allow_pending_hook {
        let hook_name = tmpl(Some(&enforcement_hook));
        if !hook_resolves(&hook_name) {
            return Err(format!(
                "law-add: hard law '{}' references hook '{}' which does not resolve \u{2014} with fail-closed enforcement this would block all dispatch. Land the hook first, or pass allow_pending_hook: true to defer.",
                tmpl(Some(&slug)), hook_name
            ));
        }
    }

    // Idempotency: ANY active row with this slug refuses — use law-supersede to replace.
    let existing = crate::db_json::query_rows(db,
        "SELECT id FROM laws.laws WHERE slug = ? AND superseded_by IS NULL", &[slug.clone()])?;
    if existing.as_array().map_or(false, |a| !a.is_empty()) {
        return Err(format!("law-add: active law with slug '{}' already exists (use law-supersede to replace)", tmpl(Some(&slug))));
    }

    let (_changes, law_id) = crate::db_json::run_write(db,
        "INSERT INTO laws.laws \
           (slug, kind, domain, title, statement, rationale, enforcement, \
            enforcement_hook, trigger_sql, established_by, \
            source_card, source_event, source_memory) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        &[slug, kind, domain, title, statement, rationale, enforcement.clone(),
          enforcement_hook, trigger_sql, established_by,
          source_card, source_event, source_memory])?;

    if !clauses.is_empty() {
        for (i, c) in clauses.iter().enumerate() {
            let order_index = nz(c.get("order_index")).cloned().unwrap_or(json!(i as i64));
            crate::db_json::run_write(db,
                "INSERT INTO laws.law_clauses (law_id, clause_type, text, order_index) VALUES (?, ?, ?, ?)",
                &[json!(law_id), c.get("clause_type").cloned().unwrap_or(Value::Null),
                  c.get("text").cloned().unwrap_or(Value::Null), order_index])?;
        }
    }
    // JS: `if (enforcement === 'hard' && enforcement_hook) clearHookCache();` — clears an
    // in-process cache with no effect on the return value or db state; nothing to mirror here.

    let out = crate::db_json::query_rows(db, "SELECT * FROM laws.laws WHERE id = ?", &[json!(law_id)])?;
    Ok(out.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS laws; \
          CREATE TABLE laws.laws (id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT NOT NULL UNIQUE, kind TEXT NOT NULL, domain TEXT NOT NULL, title TEXT NOT NULL, statement TEXT NOT NULL, rationale TEXT NOT NULL, enforcement TEXT NOT NULL, enforcement_hook TEXT, trigger_sql TEXT, established_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00', established_by TEXT NOT NULL, source_card TEXT, source_event INTEGER, source_memory INTEGER, superseded_by INTEGER, superseded_at TEXT); \
          CREATE TABLE laws.law_clauses (id INTEGER PRIMARY KEY AUTOINCREMENT, law_id INTEGER NOT NULL, clause_type TEXT NOT NULL, text TEXT NOT NULL, order_index INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00');").unwrap();
        c
    }

    fn basic(overrides: Value) -> Value {
        let mut m = json!({
            "slug": "TEST_LAW", "kind": "principle", "domain": "test",
            "title": "A test law", "statement": "Always test", "rationale": "Testing catches bugs",
            "enforcement": "soft", "established_by": "test",
        }).as_object().unwrap().clone();
        if let Some(o) = overrides.as_object() { for (k, v) in o { m.insert(k.clone(), v.clone()); } }
        Value::Object(m)
    }

    #[test]
    fn creates_a_law_row_with_all_required_fields() {
        let c = db();
        let r = lawAdd(&basic(json!({})), &c).unwrap();
        assert_eq!(r["slug"], json!("TEST_LAW"));
        assert_eq!(r["kind"], json!("principle"));
        assert_eq!(r["enforcement"], json!("soft"));
        assert_eq!(r["superseded_by"], Value::Null);
        assert_eq!(r["established_at"], json!("2026-01-01 00:00:00"));
    }

    #[test]
    fn established_by_defaults_to_unknown_when_absent() {
        let c = db();
        let mut input = basic(json!({})).as_object().unwrap().clone();
        input.remove("established_by");
        let r = lawAdd(&Value::Object(input), &c).unwrap();
        assert_eq!(r["established_by"], json!("unknown"));
    }

    #[test]
    fn required_field_and_enum_guards() {
        let c = db();
        assert!(lawAdd(&basic(json!({ "slug": null })), &c).is_err());
        assert!(lawAdd(&basic(json!({ "kind": "wishful" })), &c).is_err());
        assert!(lawAdd(&basic(json!({ "enforcement": "maybe" })), &c).is_err());
    }

    #[test]
    fn duplicate_active_slug_refuses() {
        let c = db();
        lawAdd(&basic(json!({})), &c).unwrap();
        let e = lawAdd(&basic(json!({})), &c).unwrap_err();
        assert!(e.contains("already exists"), "{e}");
    }

    #[test]
    fn inserts_clauses_atomically_with_default_and_explicit_order_index() {
        let c = db();
        let r = lawAdd(&basic(json!({ "clauses": [
            { "clause_type": "antipattern", "text": "Don't skip tests" },
            { "clause_type": "example", "text": "Run npm test before commit", "order_index": 9 },
        ] })), &c).unwrap();
        let rows = crate::db_json::query_rows(&c,
            "SELECT clause_type, order_index FROM laws.law_clauses WHERE law_id = ? ORDER BY id", &[r["id"].clone()]).unwrap();
        assert_eq!(rows, json!([
            { "clause_type": "antipattern", "order_index": 0 },
            { "clause_type": "example", "order_index": 9 },
        ]));
    }

    #[test]
    fn rejects_unknown_clause_type() {
        let c = db();
        let e = lawAdd(&basic(json!({ "clauses": [{ "clause_type": "wishful", "text": "x" }] })), &c).unwrap_err();
        assert!(e.contains("clause_type"), "{e}");
    }

    #[test]
    fn soft_enforcement_with_a_hook_never_triggers_the_fail_closed_scan() {
        // enforcement != 'hard' short-circuits the guard entirely, so an unresolved hook name is fine.
        let c = db();
        let r = lawAdd(&basic(json!({ "enforcement": "soft", "enforcement_hook": "definitely-not-a-real-skill" })), &c).unwrap();
        assert_eq!(r["enforcement_hook"], json!("definitely-not-a-real-skill"));
    }

    #[test]
    fn hard_enforcement_with_allow_pending_hook_skips_the_scan() {
        let c = db();
        let r = lawAdd(&basic(json!({ "enforcement": "hard", "enforcement_hook": "definitely-not-a-real-skill", "allow_pending_hook": true })), &c).unwrap();
        assert_eq!(r["enforcement"], json!("hard"));
        assert_eq!(r["enforcement_hook"], json!("definitely-not-a-real-skill"));
    }
}

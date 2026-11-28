//! constitution-packs.rs — twin of skills/constitution/constitution-packs.mjs (db lane, cross-db
//! `laws`+`judgments`, plus a main-db `code_nodes` read). NAMES ARE CONTRACT: `constitutionPacks`
//! (default). ENGINEERING PACKS management: a pack is a NAMED, auto-activating predicate VIEW over
//! the law set (never a duplicated table) — list every pack's status, enable installs the pack's
//! catalog+seed laws and ratifies its suspended/candidate ones, disable removes code-backed
//! (seed/catalog) laws and suspends operator/AI-authored ones. Composes `constitution-law`'s
//! dispatcher (already a twin: `constitution_constitution_law::constitutionLaw`) for every mutation.
//!
//! REAL PREDICATES, NOT THE STRINGIFIED DATA: `_helpers/packs.rs`'s `PACKS`/`packById` is a twin of
//! a DIFFERENT call site — one where the pack list itself is the OUTPUT and `match` (an arrow fn)
//! survives only as `String(fn)` source text (that module's own doc explains why). Here `p.match` is
//! actually CALLED as `laws.filter(p.match)` — a real predicate over real law rows — so this unit
//! carries its OWN executable pack table (id/name/kind/desc/activate/matcher), independently
//! transcribed from packs.mjs, mirroring the SCOPES-duplication precedent in constitution-law.rs /
//! constitution-estimate.rs (a private sibling copy can't be reused, so this unit owns its copy).
//!
//! `appliesOf`/`hasFw`/`hasLang` (packs.mjs) read `l.applies_json` specifically — LAW_CATALOG entries
//! carry their languages/frameworks under a DIFFERENT key (`applies`, not `applies_json`), so for
//! catalog rows `appliesOf` always sees `{}` and only the key-prefix fallback (`react-`/`node-`/`py-`)
//! ever matches. This is a faithful port of that exact (unintentional-looking but real) JS behavior,
//! not a bug to "fix" — e.g. `react-no-dangerously-set-html` (SEED_LAWS, scope framework) actually
//! resolves to the SECURITY pack because `categoryOf` checks its security regex ("dangerously")
//! BEFORE the framework/prefix branch — ported verbatim, exercised by this unit's own JS test.
//!
//! SEED_ROWS normalization: `SEED_LAWS.map(s => ({...s, applies_json: JSON.stringify(s.applies_json
//! ?? {})}))` — seed-laws.mjs's `applies_json` field is an OBJECT; the pack matchers need it as a
//! JSON STRING (matching what a real `laws.review_laws.applies_json` TEXT column holds), so this
//! unit stringifies it once, same as the JS module-level `SEED_ROWS` constant.
//!
//! `langSet` reads `code_nodes` UNQUALIFIED (main db, not `code.code_nodes` — matches the JS source
//! verbatim) and swallows any failure (missing table = no atlas yet) to an empty set.
//!
//! `context.db` missing → JS throws `err.required('constitution-packs', 'context.db')`. Not modeled:
//! `db: &Connection` is non-nullable here, same convention as every sibling twin.
// QUALITY: 76/100 — a 3-op dispatcher; the real complexity is the 9 hand-ported match predicates and
// the LAW_CATALOG/applies_json key-mismatch nuance documented above.

use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::sync::OnceLock;

fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } } // = JS String(v ?? '')
fn js_string(v: &Value) -> String { match v { Value::Null=>"null".into(), Value::String(s)=>s.clone(), Value::Number(n)=>n.to_string(), Value::Bool(b)=>b.to_string(), Value::Array(a)=>a.iter().map(js_string).collect::<Vec<_>>().join(","), Value::Object(_)=>"[object Object]".into() } } // = JS String(v)
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand

// `try { JSON.parse(l.applies_json ?? '{}') ?? {} } catch { {} }` — malformed JSON, a `null` parse
// result, and a missing/null column all collapse to `{}`.
fn applies_of(l: &Value) -> Value {
    let raw = nz(l.get("applies_json")).cloned().unwrap_or_else(|| json!("{}"));
    let text = js_string(&raw);
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Null) | Err(_) => json!({}),
        Ok(v) => v,
    }
}
fn has_fw(l: &Value, f: &str) -> bool {
    applies_of(l).get("frameworks").and_then(Value::as_array).map(|a| a.iter().any(|x| x.as_str() == Some(f))).unwrap_or(false)
}
fn has_lang(l: &Value, x: &str) -> bool {
    let in_arr = applies_of(l).get("languages").and_then(Value::as_array).map(|a| a.iter().any(|v| v.as_str() == Some(x))).unwrap_or(false);
    in_arr || str_qq(l.get("key")).starts_with(&format!("{x}-"))
}

fn m_security(l: &Value) -> bool { crate::constitution__helpers_categories::categoryOf(l) == json!("security") }
fn m_architecture(l: &Value) -> bool { crate::constitution__helpers_categories::categoryOf(l) == json!("architecture") }
fn m_testing(l: &Value) -> bool { crate::constitution__helpers_categories::categoryOf(l) == json!("testing") }
fn m_correctness(l: &Value) -> bool { crate::constitution__helpers_categories::categoryOf(l) == json!("correctness") }
fn m_patterns(l: &Value) -> bool { crate::constitution__helpers_categories::categoryOf(l) == json!("patterns") }
fn m_performance(l: &Value) -> bool { crate::constitution__helpers_categories::categoryOf(l) == json!("performance") }
fn m_react(l: &Value) -> bool { has_fw(l, "react") || str_qq(l.get("key")).starts_with("react-") }
fn m_node(l: &Value) -> bool { has_fw(l, "node") || str_qq(l.get("key")).starts_with("node-") }
fn m_python(l: &Value) -> bool { has_lang(l, "py") }

struct PackDef {
    id: &'static str,
    name: &'static str,
    kind: &'static str,
    desc: &'static str,
    activate: Value, // "always" | "manual" | {"framework":x} | {"language":x}
    matcher: fn(&Value) -> bool,
}

// verbatim transcription of packs.mjs's PACKS (id·name·kind·desc·activate order matches the JS
// object literal); `activate` is data (compared/inspected, never stringified into output here).
fn pack_defs() -> Vec<PackDef> {
    vec![
        PackDef { id: "security", name: "Security", kind: "security", desc: "OWASP / CWE \u{2014} secrets \u{b7} injection \u{b7} XSS \u{b7} unsafe APIs", activate: json!("always"), matcher: m_security },
        PackDef { id: "architecture", name: "Architecture", kind: "architecture", desc: "cycles \u{b7} layering \u{b7} god files \u{b7} dead code \u{b7} size limits", activate: json!("always"), matcher: m_architecture },
        PackDef { id: "testing", name: "Testing hygiene", kind: "testing", desc: "no focused / skipped tests \u{b7} central files must be tested", activate: json!("always"), matcher: m_testing },
        PackDef { id: "correctness", name: "Correctness", kind: "language", desc: "swallowed errors \u{b7} loose equality \u{b7} var \u{b7} debugger", activate: json!("always"), matcher: m_correctness },
        PackDef { id: "patterns", name: "Design Patterns (GoF)", kind: "patterns", desc: "pattern usage + anti-patterns \u{2014} singleton \u{b7} god object \u{b7} composition", activate: json!("manual"), matcher: m_patterns },
        PackDef { id: "performance", name: "Performance & memory", kind: "performance", desc: "the statement tier \u{2014} await/alloc/concat in loops \u{b7} nested loops \u{b7} the leak family (listeners \u{b7} intervals \u{b7} subscriptions)", activate: json!("manual"), matcher: m_performance },
        PackDef { id: "react", name: "React", kind: "framework", desc: "React rules \u{2014} keys \u{b7} dangerouslySetInnerHTML \u{b7} bind-in-render", activate: json!({"framework":"react"}), matcher: m_react },
        PackDef { id: "node", name: "Node.js", kind: "framework", desc: "Node rules \u{2014} process.exit \u{b7} sync fs in request paths", activate: json!({"framework":"node"}), matcher: m_node },
        PackDef { id: "python", name: "Python", kind: "language", desc: "Python rules \u{2014} exec/eval \u{b7} wildcard imports \u{b7} bare except \u{b7} mutable defaults", activate: json!({"language":"py"}), matcher: m_python },
    ]
}

// `SEED_ROWS = SEED_LAWS.map(s => ({...s, applies_json: JSON.stringify(s.applies_json ?? {})}))`
fn seed_rows() -> Vec<Value> {
    let arr = crate::constitution__helpers_seed_laws::SEED_LAWS.as_array().cloned().unwrap_or_default();
    arr.into_iter()
        .map(|s| {
            let mut m = s.as_object().cloned().unwrap_or_default();
            let applies_v = nz(m.get("applies_json")).cloned().unwrap_or_else(|| json!({}));
            m.insert("applies_json".to_string(), json!(serde_json::to_string(&applies_v).unwrap()));
            Value::Object(m)
        })
        .collect()
}

struct LangRes { py: Regex, ts: Regex, js: Regex }
fn lang_res() -> &'static LangRes {
    static R: OnceLock<LangRes> = OnceLock::new();
    R.get_or_init(|| LangRes {
        py: Regex::new(r"\.py$").unwrap(),
        ts: Regex::new(r"\.tsx?$").unwrap(),
        js: Regex::new(r"\.[cm]?jsx?$").unwrap(),
    })
}

/// `function langSet(db)` — the atlas language census (main-db `code_nodes`, UNQUALIFIED — this
/// table lives outside the laws/judgments quadrant). Any query failure (no atlas yet) swallows to
/// an empty set, matching the JS try/catch.
fn lang_set(db: &Connection) -> HashSet<String> {
    let mut s = HashSet::new();
    if let Ok(rows) = crate::db_json::query_rows(db, "SELECT DISTINCT path FROM code_nodes WHERE origin='atlas' AND type='file'", &[]) {
        if let Some(arr) = rows.as_array() {
            let r = lang_res();
            for row in arr {
                let p = row.get("path").map(js_string).unwrap_or_default().to_lowercase();
                if r.py.is_match(&p) { s.insert("py".to_string()); }
                if r.ts.is_match(&p) { s.insert("ts".to_string()); }
                if r.js.is_match(&p) { s.insert("js".to_string()); }
            }
        }
    }
    s
}

/// `function statusOf(p, laws, frameworks, langs)`
fn status_of(p: &PackDef, laws: &[Value], frameworks: &[String], langs: &HashSet<String>) -> Value {
    let matching: Vec<&Value> = laws.iter().filter(|l| (p.matcher)(l)).collect();
    let active = matching.iter().filter(|l| l.get("status").and_then(Value::as_str) == Some("ratified")).count() as i64;
    let inactive = matching.len() as i64 - active;
    let by_key: HashSet<&str> = laws.iter().filter_map(|l| l.get("key").and_then(Value::as_str)).collect();
    // installable but not present: catalog matches + seed matches (a disabled pack REMOVES its
    // code-backed laws) — no dedup between the two sources, matching the JS spread+filter chain.
    let catalog_arr = crate::constitution__helpers_law_catalog::LAW_CATALOG.as_array().cloned().unwrap_or_default();
    let seed_arr = seed_rows();
    let available = catalog_arr
        .iter()
        .chain(seed_arr.iter())
        .filter(|c| (p.matcher)(c) && !by_key.contains(c.get("key").and_then(Value::as_str).unwrap_or("")))
        .count() as i64;
    let total = active + inactive + available;

    let compatible = match &p.activate {
        Value::String(s) if s == "always" || s == "manual" => true,
        Value::Object(o) => {
            if let Some(fw) = o.get("framework").and_then(Value::as_str) {
                frameworks.iter().any(|f| f == fw)
            } else if let Some(lang) = o.get("language").and_then(Value::as_str) {
                langs.contains(lang)
            } else {
                false
            }
        }
        _ => false,
    };
    let auto = matches!(&p.activate, Value::String(s) if s == "always") || (p.activate.is_object() && compatible);
    let trigger = match &p.activate {
        Value::String(s) => s.clone(),
        Value::Object(o) => o.keys().next().cloned().unwrap_or_default(),
        _ => String::new(),
    };

    let mut m = Map::new();
    m.insert("id".into(), json!(p.id));
    m.insert("name".into(), json!(p.name));
    m.insert("kind".into(), json!(p.kind));
    m.insert("desc".into(), json!(p.desc));
    m.insert("active".into(), json!(active));
    m.insert("inactive".into(), json!(inactive));
    m.insert("available".into(), json!(available));
    m.insert("total".into(), json!(total));
    m.insert("compatible".into(), json!(compatible));
    m.insert("auto".into(), json!(auto));
    m.insert("trigger".into(), json!(trigger));
    Value::Object(m)
}

/// `export default function constitutionPacks(input, context)`
pub fn constitutionPacks(input: &Value, db: &Connection) -> Result<Value, String> {
    let _ = crate::constitution__helpers_schema::ensureConstitutionSchema(db);
    let op = match nz(input.get("op")) { Some(v) => js_string(v), None => "list".to_string() };
    let laws: Vec<Value> = crate::db_json::query_rows(db, "SELECT * FROM laws.review_laws", &[])?
        .as_array()
        .cloned()
        .unwrap_or_default();

    if op == "list" {
        let frameworks_v = crate::constitution_constitution_law::resolveFrameworks(db);
        let frameworks: Vec<String> = frameworks_v.as_array().cloned().unwrap_or_default().iter().filter_map(|v| v.as_str().map(String::from)).collect();
        let langs = lang_set(db);
        let packs_out: Vec<Value> = pack_defs().iter().map(|p| status_of(p, &laws, &frameworks, &langs)).collect();
        return Ok(json!({ "ok": true, "packs": packs_out }));
    }

    let pack_id = match nz(input.get("id")) { Some(v) => js_string(v), None => String::new() };
    let pack = pack_defs().into_iter().find(|p| p.id == pack_id);
    let pack = match pack {
        Some(p) => p,
        None => return Err("constitution-packs: id (a known pack) is required".to_string()),
    };
    let by_key: HashSet<&str> = laws.iter().filter_map(|l| l.get("key").and_then(Value::as_str)).collect();
    let matching: Vec<&Value> = laws.iter().filter(|l| (pack.matcher)(l)).collect();

    if op == "enable" {
        let catalog_arr = crate::constitution__helpers_law_catalog::LAW_CATALOG.as_array().cloned().unwrap_or_default();
        let to_install: Vec<String> = catalog_arr
            .iter()
            .filter(|c| (pack.matcher)(c))
            .filter_map(|c| c.get("key").and_then(Value::as_str).map(String::from))
            .filter(|k| !by_key.contains(k.as_str()))
            .collect();
        if !to_install.is_empty() {
            let keys_v: Vec<Value> = to_install.iter().map(|k| json!(k)).collect();
            crate::constitution_constitution_law::constitutionLaw(&json!({ "op": "enable", "keys": keys_v }), db)?;
        }
        let seed_arr = seed_rows();
        let seed_install: Vec<String> = seed_arr
            .iter()
            .filter(|s| (pack.matcher)(s))
            .filter_map(|s| s.get("key").and_then(Value::as_str).map(String::from))
            .filter(|k| !by_key.contains(k.as_str()))
            .collect();
        if !seed_install.is_empty() {
            let keys_v: Vec<Value> = seed_install.iter().map(|k| json!(k)).collect();
            crate::constitution_constitution_law::constitutionLaw(&json!({ "op": "seed", "keys": keys_v }), db)?;
        }
        let mut ratified: i64 = 0;
        for l in &matching {
            let status = l.get("status").and_then(Value::as_str).unwrap_or("");
            if status == "suspended" || status == "candidate" {
                let sub_op = if status == "candidate" { "ratify" } else { "unsuspend" };
                let key_v = l.get("key").cloned().unwrap_or(Value::Null);
                crate::constitution_constitution_law::constitutionLaw(&json!({ "op": sub_op, "key": key_v }), db)?;
                ratified += 1;
            }
        }
        return Ok(json!({ "ok": true, "id": pack.id, "installed": (to_install.len() + seed_install.len()) as i64, "ratified": ratified }));
    }

    // disable REMOVES the pack's code-backed (seed/catalog, re-creatable) laws from the book;
    // operator/AI/mined laws are never destroyed by a pack toggle — they suspend.
    if op == "disable" {
        let mut removed: i64 = 0;
        let mut suspended: i64 = 0;
        for l in &matching {
            let created_by = l.get("created_by").and_then(Value::as_str).unwrap_or("");
            let key_v = l.get("key").cloned().unwrap_or(Value::Null);
            if created_by == "seed" || created_by == "catalog" {
                crate::constitution_constitution_law::constitutionLaw(&json!({ "op": "remove", "key": key_v }), db)?;
                removed += 1;
            } else if l.get("status").and_then(Value::as_str) == Some("ratified") {
                crate::constitution_constitution_law::constitutionLaw(&json!({ "op": "suspend", "key": key_v }), db)?;
                suspended += 1;
            }
        }
        return Ok(json!({ "ok": true, "id": pack.id, "removed": removed, "suspended": suspended }));
    }

    Err(format!("constitution-packs: op must be one of: list, enable, disable (got '{op}')"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS laws; ATTACH ':memory:' AS judgments").unwrap();
        c.execute_batch(
            "CREATE TABLE workspaces (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE, \
             root_path TEXT NOT NULL, vcs TEXT NOT NULL DEFAULT 'git', is_active INTEGER NOT NULL DEFAULT 0, \
             created_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00')",
        )
        .unwrap();
        c.execute_batch(
            "CREATE TABLE code_nodes (id TEXT PRIMARY KEY, origin TEXT NOT NULL, path TEXT NOT NULL, type TEXT NOT NULL, \
             name TEXT, signature TEXT, intent TEXT, parent_id TEXT, span_json TEXT, content_hash TEXT, exported INTEGER, \
             version INTEGER NOT NULL DEFAULT 1, superseded_by TEXT, became TEXT, from_blueprint TEXT)",
        )
        .unwrap();
        c
    }

    #[test]
    fn list_reports_active_inactive_available_and_compatibility() {
        let c = conn();
        let _ = crate::constitution__helpers_schema::ensureConstitutionSchema(&c);
        c.execute(
            &format!("INSERT INTO workspaces (name, root_path, is_active) VALUES ('x', '{}/stack-facts-found', 1)", env!("ANTILOKI_FIXTURES")),
            [],
        )
        .unwrap();
        c.execute("INSERT INTO code_nodes (id, origin, path, type) VALUES ('1','atlas','src/app.py','file')", []).unwrap();
        c.execute("INSERT INTO laws.review_laws (scope, key, title, rule_md, status, created_by) VALUES ('community','no-eval','x','r','ratified','catalog')", []).unwrap();
        c.execute("INSERT INTO laws.review_laws (scope, key, title, rule_md, status, created_by) VALUES ('architecture','arch-no-cycles','x','r','candidate','catalog')", []).unwrap();

        let r = constitutionPacks(&json!({ "op": "list" }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        let packs = r["packs"].as_array().unwrap();
        assert_eq!(packs.len(), 9);
        let by_id = |id: &str| packs.iter().find(|p| p["id"] == json!(id)).unwrap();

        assert_eq!(
            by_id("security").as_object().unwrap().keys().cloned().collect::<Vec<_>>(),
            vec!["id", "name", "kind", "desc", "active", "inactive", "available", "total", "compatible", "auto", "trigger"]
        );
        assert_eq!(by_id("security")["active"], json!(1));
        assert_eq!(by_id("security")["compatible"], json!(true));
        assert_eq!(by_id("security")["trigger"], json!("always"));
        assert_eq!(by_id("architecture")["inactive"], json!(1));
        assert_eq!(by_id("react")["compatible"], json!(true)); // stack-facts finds React
        assert_eq!(by_id("react")["trigger"], json!("framework"));
        assert_eq!(by_id("node")["compatible"], json!(true)); // node is the safe default
        assert_eq!(by_id("python")["compatible"], json!(true)); // src/app.py present
    }

    #[test]
    fn list_falls_back_without_any_signal() {
        let c = conn();
        let r = constitutionPacks(&json!({}), &c).unwrap(); // op omitted -> 'list'
        let packs = r["packs"].as_array().unwrap();
        let by_id = |id: &str| packs.iter().find(|p| p["id"] == json!(id)).unwrap();
        assert_eq!(by_id("python")["compatible"], json!(false));
        assert_eq!(by_id("react")["compatible"], json!(false));
        assert_eq!(by_id("node")["compatible"], json!(true));
    }

    #[test]
    fn enable_installs_and_ratifies() {
        let c = conn();
        let _ = crate::constitution__helpers_schema::ensureConstitutionSchema(&c);
        c.execute("INSERT INTO laws.review_laws (scope, key, title, rule_md, status, created_by) VALUES ('community','no-eval','x','r','suspended','seed')", []).unwrap();
        c.execute("INSERT INTO laws.review_laws (scope, key, title, rule_md, status, created_by) VALUES ('community','sec-no-hardcoded-secret','x','r','candidate','seed')", []).unwrap();

        let r = constitutionPacks(&json!({ "op": "enable", "id": "security" }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["ratified"], json!(2));
        assert!(r["installed"].as_i64().unwrap() > 0);

        let row = crate::db_json::query_rows(&c, "SELECT status FROM laws.review_laws WHERE key='no-eval'", &[]).unwrap();
        assert_eq!(row[0]["status"], json!("ratified"));
        let danger = crate::db_json::query_rows(&c, "SELECT status, created_by FROM laws.review_laws WHERE key='react-no-dangerously-set-html'", &[]).unwrap();
        assert_eq!(danger[0]["status"], json!("ratified"));
        assert_eq!(danger[0]["created_by"], json!("seed"));
    }

    #[test]
    fn disable_removes_code_backed_and_suspends_operator_authored() {
        let c = conn();
        let _ = crate::constitution__helpers_schema::ensureConstitutionSchema(&c);
        c.execute("INSERT INTO laws.review_laws (scope, key, title, rule_md, status, created_by) VALUES ('community','no-eval','x','r','ratified','catalog')", []).unwrap();
        c.execute("INSERT INTO laws.review_laws (scope, key, title, rule_md, status, created_by) VALUES ('community','no-new-function','x','r','candidate','seed')", []).unwrap();
        c.execute("INSERT INTO laws.review_laws (scope, key, title, rule_md, status, created_by) VALUES ('business','my-secret-rule','x','r','ratified','operator')", []).unwrap();

        let r = constitutionPacks(&json!({ "op": "disable", "id": "security" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "id": "security", "removed": 2, "suspended": 1 }));

        let n = crate::db_json::query_rows(&c, "SELECT COUNT(*) n FROM laws.review_laws WHERE key IN ('no-eval','no-new-function')", &[]).unwrap();
        assert_eq!(n[0]["n"], json!(0));
        let secret = crate::db_json::query_rows(&c, "SELECT status FROM laws.review_laws WHERE key='my-secret-rule'", &[]).unwrap();
        assert_eq!(secret[0]["status"], json!("suspended"));
    }

    #[test]
    fn guard_throws_carry_the_expected_messages() {
        let c = conn();
        assert_eq!(constitutionPacks(&json!({ "op": "enable", "id": "not-a-pack" }), &c).unwrap_err(), "constitution-packs: id (a known pack) is required");
        assert!(constitutionPacks(&json!({ "op": "bogus", "id": "security" }), &c).unwrap_err().contains("op must be one of"));
    }
}

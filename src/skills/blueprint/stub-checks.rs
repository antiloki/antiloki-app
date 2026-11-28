//! stub-checks.rs — twin of skills/blueprint/stub-checks.mjs (db-read, async-over-sync per the
//! async-lane rule — the JS `await`s only sync SQLite reads via `context.db` + the already-converted
//! `parseDecls`/`gradeNames`/`isDegenerateSource` helpers, so this twin is an ordinary sync `pub fn`).
//! NAMES ARE CONTRACT: `stubChecks` (default). The deterministic battery over design stubs, run
//! before the design court (PLAN gate).
use rusqlite::Connection;
use serde_json::{json, Value};

fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

fn norm(p: Option<&Value>) -> String {
    str_qq(p).replace('\\', "/")
}

pub fn stubChecks(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const db = context?.db; if (!db) throw err.contextMissing('stub-checks', 'db')` —
    // unreachable in the db lane, a real Connection is always supplied.
    let contract_slug_v = input.get("contract_slug").cloned().unwrap_or(Value::Null);
    if !truthy(Some(&contract_slug_v)) {
        return Err("stub-checks: contract_slug is required".into());
    }

    let blueprint_id = input.get("blueprint_id").filter(|v| truthy(Some(v)));
    let rows = if let Some(bid) = blueprint_id {
        crate::db_json::query_rows(db, "SELECT id, blueprint_json FROM blueprints WHERE id = ?", &[bid.clone()])?
    } else {
        crate::db_json::query_rows(
            db,
            "SELECT id, blueprint_json FROM blueprints WHERE contract_slug = ? AND superseded_by IS NULL ORDER BY id DESC LIMIT 1",
            &[contract_slug_v.clone()],
        )?
    };
    let row = match rows.as_array().and_then(|a| a.first()) {
        Some(r) => r.clone(),
        None => return Err(format!("stub-checks: blueprint '{}' not found", str_qq(Some(&contract_slug_v)))),
    };

    let blueprint_json_str = row.get("blueprint_json").and_then(|v| v.as_str()).unwrap_or("");
    let raw_bp: Value = serde_json::from_str(blueprint_json_str).unwrap_or_else(|_| json!({}));
    let stub_files: Vec<Value> = raw_bp.get("stub_files").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    // pre-skeleton design (block-intent only, no parsed stubs) → nothing deterministic to check.
    if stub_files.is_empty() {
        return Ok(json!({ "ok": true, "errors": 0, "warnings": 0, "findings": [], "skipped": true }));
    }

    let planned_nodes: Vec<Value> = raw_bp.get("planned_nodes").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let mut fn_by_path: std::collections::HashMap<String, Vec<Value>> = std::collections::HashMap::new();
    for n in &planned_nodes {
        if n.get("type").and_then(|v| v.as_str()) != Some("function") {
            continue;
        }
        let p = norm(n.get("path"));
        fn_by_path.entry(p).or_default().push(json!(str_qq(n.get("name"))));
    }

    let mut findings: Vec<Value> = Vec::new();
    let mut planned_paths: std::collections::HashSet<String> = std::collections::HashSet::new();

    for f in &stub_files {
        let path = norm(f.get("path"));
        if path.is_empty() {
            continue;
        }
        planned_paths.insert(path.clone());
        let src = str_qq(f.get("source"));
        let pd = crate::blueprint__helpers_code_checks::parseDecls(&json!(path), &json!(src));
        let parses = pd.get("parses").and_then(|v| v.as_bool()).unwrap_or(false);
        let skipped = pd.get("skipped").and_then(|v| v.as_bool()).unwrap_or(false);
        if !skipped && !parses {
            findings.push(json!({ "kind": "parse", "severity": "error", "file": path, "detail": "the stub does not parse" }));
        }
        // DEGENERATE stub (meli r5): a short shell with NO declaration keyword anywhere is a
        // broken design output, not a design — HARD error, the redesign loop re-runs the stub call.
        let degenerate = crate::blueprint__helpers_code_checks::isDegenerateSource(&json!(src));
        if !skipped && degenerate.as_bool().unwrap_or(false) {
            findings.push(json!({
                "kind": "degenerate",
                "severity": "error",
                "file": path,
                "detail": format!("the stub ({} chars) declares nothing — a broken per-file design output", src.trim().chars().count()),
            }));
        }

        let names = fn_by_path.get(&path).cloned().unwrap_or_default();
        let grade = crate::blueprint__helpers_code_checks::gradeNames(&json!(names), &Value::Null);
        let duplicate: Vec<Value> = grade.get("duplicate").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        for d in &duplicate {
            let name = d.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let count = d.get("count").and_then(|v| v.as_i64()).unwrap_or(0);
            findings.push(json!({ "kind": "duplicate", "severity": "error", "file": path, "detail": format!("'{name}' planned {count}×") }));
        }
        let bad_identifier: Vec<Value> = grade.get("badIdentifier").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        for b in &bad_identifier {
            let bname = b.as_str().unwrap_or("");
            findings.push(json!({ "kind": "bad-name", "severity": "warn", "file": path, "detail": format!("'{bname}' is not a valid identifier") }));
        }
        if names.is_empty() {
            findings.push(json!({ "kind": "empty-file", "severity": "warn", "file": path, "detail": "the stub declares no functions" }));
        }
    }

    // acceptance coverage: a file-exists acceptance the design doesn't create a stub for is a
    // coverage gap. (Advisory — the design court is the real coverage judge; this is the
    // deterministic floor.) JS wraps this whole block in try/catch and swallows any failure
    // (unreadable acceptance_json, missing table) → skip coverage entirely. A failed lookup or
    // an unparseable acceptance_json here resolves to an empty acceptance list, which yields the
    // identical "no accept-gap findings" outcome.
    let acceptance: Vec<Value> = match crate::db_json::query_rows(db, "SELECT acceptance_json FROM contracts WHERE slug = ?", &[contract_slug_v.clone()]) {
        Ok(rows) => {
            let acc_json = rows.as_array().and_then(|a| a.first()).and_then(|r| r.get("acceptance_json")).cloned().unwrap_or(Value::Null);
            if truthy(Some(&acc_json)) {
                acc_json
                    .as_str()
                    .and_then(|s| serde_json::from_str::<Value>(s).ok())
                    .and_then(|v| v.as_array().cloned())
                    .unwrap_or_default()
            } else {
                Vec::new()
            }
        }
        Err(_) => Vec::new(),
    };
    for a in &acceptance {
        let kind = a.get("kind").and_then(|v| v.as_str());
        let path_present = a.get("path");
        if kind == Some("file-exists") && truthy(path_present) {
            let ap = norm(path_present);
            if !planned_paths.contains(&ap) {
                findings.push(json!({ "kind": "accept-gap", "severity": "warn", "file": ap, "detail": "a file-exists acceptance has no planned file" }));
            }
        }
    }

    let errors = findings.iter().filter(|f| f.get("severity") == Some(&json!("error"))).count();
    let warnings = findings.iter().filter(|f| f.get("severity") == Some(&json!("warn"))).count();
    Ok(json!({ "ok": errors == 0, "errors": errors as i64, "warnings": warnings as i64, "findings": findings }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup(db: &Connection) {
        db.execute_batch(
            "CREATE TABLE blueprints (id INTEGER PRIMARY KEY, contract_slug TEXT, blueprint_json TEXT, superseded_by INTEGER);
             CREATE TABLE contracts (slug TEXT PRIMARY KEY, acceptance_json TEXT);",
        )
        .unwrap();
    }

    #[test]
    fn skipped_when_no_stub_files() {
        let db = Connection::open_in_memory().unwrap();
        setup(&db);
        db.execute("INSERT INTO blueprints (contract_slug, blueprint_json) VALUES ('CT-A', '{}')", []).unwrap();
        let r = stubChecks(&json!({ "contract_slug": "CT-A" }), &db).unwrap();
        assert_eq!(r, json!({ "ok": true, "errors": 0, "warnings": 0, "findings": [], "skipped": true }));
    }

    #[test]
    fn contract_slug_required() {
        let db = Connection::open_in_memory().unwrap();
        setup(&db);
        assert_eq!(stubChecks(&json!({}), &db), Err("stub-checks: contract_slug is required".to_string()));
    }

    #[test]
    fn not_found() {
        let db = Connection::open_in_memory().unwrap();
        setup(&db);
        assert_eq!(
            stubChecks(&json!({ "contract_slug": "CT-Z" }), &db),
            Err("stub-checks: blueprint 'CT-Z' not found".to_string())
        );
    }

    #[test]
    fn flags_duplicate_bad_name_empty_file_and_accept_gap() {
        let db = Connection::open_in_memory().unwrap();
        setup(&db);
        let bp = json!({
            "stub_files": [
                { "path": "src/a.mjs", "source": "export function foo() { return 1; }" },
                { "path": "src/b.mjs", "source": "// TODO stub" },
            ],
            "planned_nodes": [
                { "type": "function", "path": "src/a.mjs", "name": "foo" },
                { "type": "function", "path": "src/a.mjs", "name": "foo" },
                { "type": "function", "path": "src/a.mjs", "name": "1bad" },
            ],
        });
        db.execute(
            "INSERT INTO blueprints (contract_slug, blueprint_json) VALUES ('CT-B', ?)",
            rusqlite::params![bp.to_string()],
        )
        .unwrap();
        db.execute(
            "INSERT INTO contracts (slug, acceptance_json) VALUES ('CT-B', ?)",
            rusqlite::params![json!([{ "kind": "file-exists", "path": "src/c.mjs" }]).to_string()],
        )
        .unwrap();
        let r = stubChecks(&json!({ "contract_slug": "CT-B" }), &db).unwrap();
        assert_eq!(r["ok"], json!(false));
        let kinds: Vec<&str> = r["findings"].as_array().unwrap().iter().map(|f| f["kind"].as_str().unwrap()).collect();
        assert!(kinds.contains(&"duplicate"));
        assert!(kinds.contains(&"bad-name"));
        assert!(kinds.contains(&"empty-file"));
        assert!(kinds.contains(&"accept-gap"));
    }

    #[test]
    fn degenerate_source_is_hard_error() {
        let db = Connection::open_in_memory().unwrap();
        setup(&db);
        let bp = json!({ "stub_files": [ { "path": "src/x.mjs", "source": "stub_source" } ] });
        db.execute(
            "INSERT INTO blueprints (contract_slug, blueprint_json) VALUES ('CT-C', ?)",
            rusqlite::params![bp.to_string()],
        )
        .unwrap();
        let r = stubChecks(&json!({ "contract_slug": "CT-C" }), &db).unwrap();
        assert_eq!(r["ok"], json!(false));
        let kinds: Vec<&str> = r["findings"].as_array().unwrap().iter().map(|f| f["kind"].as_str().unwrap()).collect();
        assert!(kinds.contains(&"degenerate"));
    }
}

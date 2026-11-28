//! snippet-checks.rs — twin of skills/blueprint/snippet-checks.mjs (db lane). NAMES ARE CONTRACT:
//! `snippetChecks` (default). The DETERMINISTIC battery on authored snippets, run before BUILD:
//! composes each planned file from its snippets, parses it (via the already-converted ast-parse
//! twin through code-checks::parseDecls), and grades declarations against the planned graph.
//! JS's default export is `async` — per the async-lane rule this stays an ordinary sync `pub fn`:
//! it only awaits parseDecls, which is itself sync-over-sync SQLite reads in Rust (astParse never
//! touches the throwaway in-memory connection code-checks hands it).
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

pub fn snippetChecks(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS throws err.contextMissing('snippet-checks','db') when no db — the db lane always injects
    // one, so that guard is unreachable here.
    let contract_slug = input.get("contract_slug").cloned().unwrap_or(Value::Null);
    if !truthy(Some(&contract_slug)) {
        return Err("snippet-checks: contract_slug is required".into());
    }

    let blueprint_id = input.get("blueprint_id").filter(|v| !v.is_null());
    let rows = if let Some(bid) = blueprint_id {
        crate::db_json::query_rows(db, "SELECT id, version, blueprint_json FROM blueprints WHERE id = ?", &[bid.clone()])?
    } else {
        crate::db_json::query_rows(
            db,
            "SELECT id, version, blueprint_json FROM blueprints WHERE contract_slug = ? AND superseded_by IS NULL ORDER BY id DESC LIMIT 1",
            &[contract_slug.clone()],
        )?
    };
    let row = match rows.as_array().and_then(|a| a.first()) {
        Some(r) => r.clone(),
        None => return Err(format!("snippet-checks: blueprint '{}' not found", str_qq(Some(&contract_slug)))),
    };
    let row_id = row.get("id").cloned().unwrap_or(Value::Null);

    let blueprint_json_str = row.get("blueprint_json").and_then(|v| v.as_str()).unwrap_or("");
    let raw_bp: Value = serde_json::from_str(blueprint_json_str).unwrap_or_else(|_| json!({}));
    let blueprint = crate::blueprint_block_shape::normalizeBlueprint(&raw_bp);
    let empty_arr = vec![];
    let planned_nodes = raw_bp.get("planned_nodes").and_then(|v| v.as_array()).unwrap_or(&empty_arr);

    // planned function names per file → the expected declaration set for that file.
    let mut expected_order: Vec<String> = Vec::new();
    let mut expected_by_path: std::collections::HashMap<String, Vec<(String, Value)>> = std::collections::HashMap::new();
    for n in planned_nodes {
        if n.get("type").and_then(|v| v.as_str()) != Some("function") {
            continue;
        }
        let p = str_qq(n.get("path"));
        let name = str_qq(n.get("name"));
        let signature = n.get("signature").cloned().unwrap_or_else(|| json!(""));
        let entry = expected_by_path.entry(p.clone()).or_insert_with(|| {
            expected_order.push(p.clone());
            Vec::new()
        });
        entry.push((name, signature));
    }
    let planned_fn_count: usize = expected_by_path.values().map(|v| v.len()).sum();

    let snip_rows = crate::db_json::query_rows(db, "SELECT block_id, code FROM blueprint_snippets WHERE blueprint_id = ?", &[row_id.clone()])?;
    let snips: Vec<Value> = snip_rows.as_array().cloned().unwrap_or_default();
    let mut authored = Map::new();
    for s in &snips {
        let key = js_string(s.get("block_id").unwrap_or(&Value::Null));
        authored.insert(key, json!({ "code": s.get("code").cloned().unwrap_or(Value::Null) }));
    }
    let authored_val = Value::Object(authored);

    let mut findings: Vec<Value> = Vec::new();
    let mut files: Vec<Value> = Vec::new();

    let blocks = blueprint.get("blocks").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    for fb in &blocks {
        if fb.get("type").and_then(|v| v.as_str()) != Some("file") {
            continue;
        }
        let composed = crate::blueprint_blueprint_compose::blueprintCompose(&json!({ "block": fb, "authored": authored_val }), db)?;
        let path_v = composed.get("path").cloned().unwrap_or(Value::Null);
        if !truthy(Some(&path_v)) {
            continue;
        }
        let path = str_qq(Some(&path_v));
        let source_v = composed.get("source").cloned().unwrap_or(Value::Null);
        let source_str = str_qq(Some(&source_v));

        let expected = expected_by_path.get(&path).cloned().unwrap_or_default();
        let pd = crate::blueprint__helpers_code_checks::parseDecls(&path_v, &json!(source_str));
        let parses = truthy(pd.get("parses"));
        let skipped = truthy(pd.get("skipped"));
        let decls = pd.get("decls").and_then(|v| v.as_array()).cloned().unwrap_or_default();

        if skipped {
            files.push(json!({ "path": path_v, "status": "skipped" }));
            continue;
        }
        if !parses {
            let mut detail = format!(
                "the composed file does not parse: {}",
                pd.get("error").and_then(|v| v.as_str()).unwrap_or("syntax error")
            );
            let ln = pd.get("loc").and_then(|l| l.get("line")).and_then(|v| v.as_i64());
            if let Some(line_no) = ln {
                if line_no > 0 {
                    if let Some(line_text) = source_str.split('\n').nth((line_no - 1) as usize) {
                        let trimmed: String = line_text.trim().chars().take(120).collect();
                        detail.push_str(&format!(" — line {}: {}", line_no, trimmed));
                    }
                }
            }
            findings.push(json!({ "kind": "parse", "severity": "error", "file": path_v, "detail": detail }));
            files.push(json!({ "path": path_v, "status": "fail" }));
            continue;
        }

        let names: Vec<Value> = decls.iter().map(|d| d.get("name").cloned().unwrap_or(Value::Null)).collect();
        let expected_names: Value = if !expected.is_empty() {
            json!(expected.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>())
        } else {
            Value::Null
        };
        let graded = crate::blueprint__helpers_code_checks::gradeNames(&json!(names), &expected_names);

        if let Some(dup) = graded.get("duplicate").and_then(|v| v.as_array()) {
            for d in dup {
                findings.push(json!({ "kind": "duplicate", "severity": "error", "file": path_v, "detail": format!("'{}' declared {}×", str_qq(d.get("name")), d.get("count").and_then(|v| v.as_i64()).unwrap_or(0)) }));
            }
        }
        if let Some(bad) = graded.get("badIdentifier").and_then(|v| v.as_array()) {
            for b in bad {
                findings.push(json!({ "kind": "duplicate", "severity": "error", "file": path_v, "detail": format!("'{}' is not a valid identifier", str_qq(Some(b))) }));
            }
        }
        if !expected.is_empty() && decls.is_empty() {
            findings.push(json!({ "kind": "empty", "severity": "error", "file": path_v, "detail": format!("{} function(s) planned, none declared", expected.len()) }));
        }
        if let Some(missing) = graded.get("missing").and_then(|v| v.as_array()) {
            for m in missing {
                findings.push(json!({ "kind": "missing", "severity": "error", "file": path_v, "detail": format!("planned function '{}' is not declared", str_qq(Some(m))) }));
            }
        }
        if let Some(extra) = graded.get("extra").and_then(|v| v.as_array()) {
            for x in extra {
                findings.push(json!({ "kind": "extra", "severity": "warn", "file": path_v, "detail": format!("'{}' is declared but not in the plan", str_qq(Some(x))) }));
            }
        }

        // signature drift (advisory) — informational, never gated.
        let mut got_sig: std::collections::HashMap<String, Value> = std::collections::HashMap::new();
        for d in &decls {
            got_sig.insert(str_qq(d.get("name")), d.get("signature").cloned().unwrap_or(Value::Null));
        }
        for (name, signature) in &expected {
            if let Some(g) = got_sig.get(name) {
                let g_norm = if truthy(Some(g)) { g.clone() } else { json!("") };
                let e_norm = if truthy(Some(signature)) { signature.clone() } else { json!("") };
                if g_norm != e_norm {
                    findings.push(json!({ "kind": "signature", "severity": "warn", "file": path_v, "detail": format!("'{}' signature drifted from the plan", name) }));
                }
            }
        }

        if truthy(Some(&crate::blueprint__helpers_code_checks::isStubBody(&json!(source_str)))) {
            findings.push(json!({ "kind": "unfilled", "severity": "error", "file": path_v, "detail": "still contains a TODO/throw placeholder — truncated or unfinished block; re-emit it COMPLETE" }));
        }

        let here: Vec<&Value> = findings.iter().filter(|f| f.get("file") == Some(&path_v)).collect();
        let status = if here.iter().any(|f| f.get("severity").and_then(|v| v.as_str()) == Some("error")) {
            "fail"
        } else if !here.is_empty() {
            "warn"
        } else {
            "ok"
        };
        files.push(json!({ "path": path_v, "status": status }));
    }

    // count: every planned FILE must have authored content, by file COVERAGE (not raw snippet count).
    if planned_fn_count > 0 {
        let planned_files: Vec<&String> = expected_order.iter().filter(|p| !p.is_empty()).collect();
        let mut covered: std::collections::HashSet<String> = std::collections::HashSet::new();
        for s in &snips {
            let bid = js_string(s.get("block_id").unwrap_or(&Value::Null));
            for pf in &planned_files {
                if &bid == *pf || bid.starts_with(&format!("{}.", pf)) {
                    covered.insert((*pf).clone());
                }
            }
        }
        let uncovered: Vec<&String> = planned_files.iter().filter(|p| !covered.contains(**p)).cloned().collect();
        if !uncovered.is_empty() {
            let sample: Vec<String> = uncovered.iter().take(3).map(|s| (*s).clone()).collect();
            findings.push(json!({
                "kind": "count", "severity": "error", "file": "(blueprint)",
                "detail": format!("{} planned file(s) have no authored snippet: {}", uncovered.len(), sample.join(", "))
            }));
        }
    }

    let errors: Vec<Value> = findings.iter().filter(|f| f.get("severity").and_then(|v| v.as_str()) == Some("error")).cloned().collect();
    let warnings: Vec<Value> = findings.iter().filter(|f| f.get("severity").and_then(|v| v.as_str()) == Some("warn")).cloned().collect();

    Ok(json!({
        "ok": errors.is_empty(),
        "errors": errors.len() as i64,
        "warnings": warnings.len() as i64,
        "findings": findings,
        "files": files,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE blueprints (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT, version INTEGER, blueprint_json TEXT, superseded_by INTEGER);
             CREATE TABLE blueprint_snippets (id INTEGER PRIMARY KEY AUTOINCREMENT, blueprint_id INTEGER, block_id TEXT, code TEXT);",
        )
        .unwrap();
        c
    }

    #[test]
    fn missing_contract_slug_errors() {
        let c = db();
        assert!(snippetChecks(&json!({}), &c).unwrap_err().contains("contract_slug is required"));
    }

    #[test]
    fn no_blueprint_errors() {
        let c = db();
        assert!(snippetChecks(&json!({ "contract_slug": "CT-N" }), &c).unwrap_err().contains("not found"));
    }

    #[test]
    fn happy_path_all_planned_functions_declared() {
        let c = db();
        let bp = json!({
            "blocks": [{ "id": "src/f.js", "type": "file", "path": "src/f.js", "children": [
                { "id": "src/f.js.fn", "type": "function" },
            ] }],
            "planned_nodes": [{ "id": "src/f.js#fn", "type": "function", "name": "fn", "path": "src/f.js", "signature": "" }],
        });
        c.execute("INSERT INTO blueprints (contract_slug, version, blueprint_json) VALUES ('CT-M', 1, ?1)", [bp.to_string()]).unwrap();
        let rows = crate::db_json::query_rows(&c, "SELECT id FROM blueprints WHERE contract_slug='CT-M'", &[]).unwrap();
        let bid = rows[0]["id"].as_i64().unwrap();
        c.execute(
            "INSERT INTO blueprint_snippets (blueprint_id, block_id, code) VALUES (?1, 'src/f.js.fn', 'export function fn() { return 1; }')",
            rusqlite::params![bid],
        )
        .unwrap();

        let r = snippetChecks(&json!({ "contract_slug": "CT-M" }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["errors"], json!(0));
        // Verified against the real snippet-checks.mjs via a node repro on this EXACT fixture
        // (batch-21 wrote this test write-only and never ran it): the planned signature is ""
        // but the authored `fn` takes no params, so parseDecls/parseJs reports signature "()" —
        // "()" !== "" trips the (advisory) `signature` drift finding, so real JS ALSO returns
        // status "warn" here (1 warning), not "ok". Wrong test assertion, not a port bug.
        assert_eq!(r["warnings"], json!(1));
        assert_eq!(r["files"][0]["status"], json!("warn"));
    }

    #[test]
    fn duplicate_declaration_is_flagged_and_missing_is_flagged() {
        let c = db();
        let bp = json!({
            "blocks": [{ "id": "src/g.js", "type": "file", "path": "src/g.js", "children": [
                { "id": "src/g.js.a", "type": "function" },
                { "id": "src/g.js.b", "type": "function" },
            ] }],
            "planned_nodes": [
                { "id": "src/g.js#a", "type": "function", "name": "a", "path": "src/g.js" },
                { "id": "src/g.js#b", "type": "function", "name": "b", "path": "src/g.js" },
            ],
        });
        c.execute("INSERT INTO blueprints (contract_slug, version, blueprint_json) VALUES ('CT-D', 1, ?1)", [bp.to_string()]).unwrap();
        let rows = crate::db_json::query_rows(&c, "SELECT id FROM blueprints WHERE contract_slug='CT-D'", &[]).unwrap();
        let bid = rows[0]["id"].as_i64().unwrap();
        // both blocks authored as the SAME name 'querySanitizer' — the 4x-same-name class.
        c.execute("INSERT INTO blueprint_snippets (blueprint_id, block_id, code) VALUES (?1, 'src/g.js.a', 'function querySanitizer(){}')", rusqlite::params![bid]).unwrap();
        c.execute("INSERT INTO blueprint_snippets (blueprint_id, block_id, code) VALUES (?1, 'src/g.js.b', 'function querySanitizer(){}')", rusqlite::params![bid]).unwrap();

        let r = snippetChecks(&json!({ "contract_slug": "CT-D" }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        let kinds: Vec<String> = r["findings"].as_array().unwrap().iter().map(|f| f["kind"].as_str().unwrap().to_string()).collect();
        // Verified against the real snippet-checks.mjs via a node repro on this EXACT fixture
        // (batch-21 wrote this test write-only and never ran it): a bare top-level `function`
        // redeclaration is NOT a parse error under acorn's sourceType:'module' — two
        // `FunctionDeclaration`s are var-like (TopLevelVarDeclaredNames), not lexical, so
        // duplicating one is valid grammar; only a duplicate LEXICAL binding (const/let/class,
        // or a duplicate EXPORTED name) early-errors as "Identifier ... has already been
        // declared". The module doc comment's "4x querySanitizer -> parse" scenario describes
        // that lexical/export case, not this bare-`function` one. Real JS instead reports it via
        // gradeNames: `duplicate` (2x querySanitizer) + `missing` (a, b) + `extra` (querySanitizer)
        // — errors=3, warnings=1 — byte-for-byte what this Rust twin already produced before this
        // fix. So this was a wrong test assertion (and a misleading test name), not an
        // oxc-vs-acorn port divergence: oxc's parser is equally permissive of this input.
        assert!(kinds.contains(&"duplicate".to_string()));
        assert!(!kinds.contains(&"parse".to_string()));
        assert_eq!(r["errors"], json!(3));
        assert_eq!(r["warnings"], json!(1));
        assert_eq!(r["files"][0]["status"], json!("fail"));
    }

    #[test]
    fn uncovered_planned_file_flags_count_finding() {
        let c = db();
        let bp = json!({
            "blocks": [{ "id": "src/h.js", "type": "file", "path": "src/h.js", "children": [] }],
            "planned_nodes": [{ "id": "src/h.js#fn", "type": "function", "name": "fn", "path": "src/h.js" }],
        });
        c.execute("INSERT INTO blueprints (contract_slug, version, blueprint_json) VALUES ('CT-U', 1, ?1)", [bp.to_string()]).unwrap();
        let r = snippetChecks(&json!({ "contract_slug": "CT-U" }), &c).unwrap();
        let kinds: Vec<String> = r["findings"].as_array().unwrap().iter().map(|f| f["kind"].as_str().unwrap().to_string()).collect();
        assert!(kinds.contains(&"count".to_string()));
        assert!(kinds.contains(&"empty".to_string()));
    }

    #[test]
    fn blueprint_id_input_overrides_contract_slug_lookup() {
        let c = db();
        let bp = json!({ "blocks": [], "planned_nodes": [] });
        c.execute("INSERT INTO blueprints (contract_slug, version, blueprint_json) VALUES ('CT-I', 1, ?1)", [bp.to_string()]).unwrap();
        let rows = crate::db_json::query_rows(&c, "SELECT id FROM blueprints WHERE contract_slug='CT-I'", &[]).unwrap();
        let bid = rows[0]["id"].as_i64().unwrap();
        let r = snippetChecks(&json!({ "contract_slug": "CT-I", "blueprint_id": bid }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
    }
}

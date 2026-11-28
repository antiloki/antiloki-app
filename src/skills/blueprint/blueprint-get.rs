//! blueprint-get.rs — twin of skills/blueprint/blueprint-get.mjs (db READ lane; cross-db via
//! judgments.*). NAMES ARE CONTRACT: `blueprintGet` (default). Reads a contract's active
//! BLUEPRINT for the FE render (pipeline v2): recursive blocks/bindings (normalized on read),
//! authored status (blueprint_snippets), the P4 AST-skeleton planned graph + delta, the
//! deterministic composed files, and the design/code judgment verdicts (judgments.sqlite).
//! Pure read, no AI.
// QUALITY: 81/100 — cross-module (block-shape normalize/flatten, blueprint-compose) + cross-db
// (judgments.judgment_cases/court_evidence/judgment_items) faithfully threaded; blueprint_json is
// assumed valid JSON text (schema NOT NULL) — a literal SQL-NULL cell (JSON.parse(null) -> null,
// then a raw property read on it) is a JS TypeError this twin does not model (out of scope: guard-
// throw-adjacent, not exercised by any fixture per the playbook's success-path-only rule).
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
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(x) => str_qq(Some(x)),
    }
}
// JS `acc.length ? size/acc.length : null` result rendering: integer-valued -> bare integer
// (JSON.stringify(1) is "1", not "1.0"); genuinely fractional -> f64 passed through unchanged.
fn js_num_value(f: f64) -> Value {
    if f.is_finite() && f == f.trunc() && f.abs() < 1e15 { json!(f as i64) } else { json!(f) }
}

// JS: recursive `annotate` closure — id/type/path/intent (?? null) + authored (Set.has, strict
// equality) + children (recurse on b?.children, default []).
fn annotate_blocks(bs: &Value, authored: &[Value]) -> Value {
    let empty = Vec::new();
    let arr = bs.as_array().unwrap_or(&empty);
    let out: Vec<Value> = arr
        .iter()
        .map(|b| {
            let id = b.get("id").cloned().unwrap_or(Value::Null);
            let is_authored = authored.contains(&id);
            let children = annotate_blocks(b.get("children").unwrap_or(&Value::Null), authored);
            json!({
                "id": id,
                "type": b.get("type").cloned().unwrap_or(Value::Null),
                "path": b.get("path").cloned().unwrap_or(Value::Null),
                "intent": b.get("intent").cloned().unwrap_or(Value::Null),
                "authored": is_authored,
                "children": children,
            })
        })
        .collect();
    Value::Array(out)
}

// JS: `coversForPath(p)` — coverage items whose `maps_to_blocks` (String-mapped) includes p,
// mapped to `String(c?.acceptance_id)`.
fn covers_for_path(coverage: &Value, p: &str) -> Vec<Value> {
    let empty = Vec::new();
    let arr = coverage.as_array().unwrap_or(&empty);
    arr.iter()
        .filter(|c| {
            c.get("maps_to_blocks")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().any(|x| js_string(x) == p))
                .unwrap_or(false)
        })
        .map(|c| json!(tmpl(c.get("acceptance_id"))))
        .collect()
}

// JS: the `judgmentFor(mode)` closure — null if no card_slug or no case/evidence/items found;
// else `{ ...(jc ?? {verdict:null}), evidence, items }`.
fn judgment_for(db: &Connection, card_slug: &Value, mode: &str, version: &Value) -> Result<Value, String> {
    if !truthy(Some(card_slug)) {
        return Ok(Value::Null);
    }
    let scope_ref = format!("{}-v{}", mode, tmpl(Some(version)));
    let jid = crate::swarm_judgment_id::makeJudgmentId(card_slug, &json!("blueprint"), &json!(scope_ref))?;

    let jc_rows = crate::db_json::query_rows(
        db,
        "SELECT verdict, goal_line, n_items, defender_trues, prosecutor_trues, defender_rate, prosecutor_rate, defender_accuracy, prosecutor_accuracy FROM judgments.judgment_cases WHERE judgment_id = ? LIMIT 1",
        &[json!(jid)],
    )?;
    let jc = jc_rows.as_array().and_then(|a| a.first()).cloned();

    let ev_rows = crate::db_json::query_rows(
        db,
        "SELECT n_acceptance, n_microtasks, n_blocks, n_nested_blocks, has_card, has_blueprint, has_screening, evidence_md, created_at FROM judgments.court_evidence WHERE judgment_id = ?",
        &[json!(jid.clone())],
    )?;
    let evidence = match ev_rows.as_array().and_then(|a| a.first()) {
        Some(e) => {
            let mut m = e.as_object().cloned().unwrap_or_default();
            let hc = truthy(m.get("has_card"));
            let hb = truthy(m.get("has_blueprint"));
            let hs = truthy(m.get("has_screening"));
            m.insert("has_card".into(), json!(hc));
            m.insert("has_blueprint".into(), json!(hb));
            m.insert("has_screening".into(), json!(hs));
            Value::Object(m)
        }
        None => Value::Null,
    };

    let items_rows = crate::db_json::query_rows(
        db,
        "SELECT side, item_id, statement, grounded, judge_value, judge_accuracy, judge_why FROM judgments.judgment_items WHERE judgment_id = ? ORDER BY side, item_id",
        &[json!(jid.clone())],
    )?;
    let mut prosecutor: Vec<Value> = Vec::new();
    let mut defender: Vec<Value> = Vec::new();
    if let Some(arr) = items_rows.as_array() {
        for r in arr {
            let it = json!({
                "item_id": r.get("item_id").cloned().unwrap_or(Value::Null),
                "statement": r.get("statement").cloned().unwrap_or(Value::Null),
                "grounded": truthy(r.get("grounded")),
                "value": truthy(r.get("judge_value")),
                "accuracy": r.get("judge_accuracy").cloned().unwrap_or(Value::Null),
                "why": if truthy(r.get("judge_why")) { r.get("judge_why").cloned().unwrap_or(Value::Null) } else { json!("") },
            });
            if r.get("side").and_then(|v| v.as_str()) == Some("prosecutor") {
                prosecutor.push(it);
            } else {
                defender.push(it);
            }
        }
    }

    if jc.is_none() && evidence.is_null() && prosecutor.is_empty() && defender.is_empty() {
        return Ok(Value::Null);
    }

    let base = jc.unwrap_or_else(|| json!({ "verdict": Value::Null }));
    let mut out = base.as_object().cloned().unwrap_or_default();
    out.insert("evidence".into(), evidence);
    out.insert("items".into(), json!({ "prosecutor": prosecutor, "defender": defender }));
    Ok(Value::Object(out))
}

pub fn blueprintGet(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS throws err.contextMissing('blueprint-get','db') when no db — the db lane always injects
    // one, so that guard is unreachable here.
    let contract_slug = input.get("contract_slug").cloned().unwrap_or(Value::Null);
    if !truthy(Some(&contract_slug)) {
        return Err("blueprint-get: contract_slug is required".into());
    }

    let rows = crate::db_json::query_rows(
        db,
        "SELECT id, version, blueprint_json, lint_passed FROM blueprints WHERE contract_slug = ? AND superseded_by IS NULL ORDER BY id DESC LIMIT 1",
        &[contract_slug.clone()],
    )?;
    let row = match rows.as_array().and_then(|a| a.first()) {
        Some(r) => r.clone(),
        None => return Ok(json!({ "contract_slug": contract_slug, "blueprint": Value::Null })),
    };
    let row_id = row.get("id").cloned().unwrap_or(Value::Null);
    let row_version = row.get("version").cloned().unwrap_or(Value::Null);

    // normalize-on-read: default the {blocks,bindings,coverage} arrays if any are missing. Keep
    // the RAW parse too — the P4 planned graph (planned_nodes/edges + delta) rides alongside.
    let blueprint_json_str = row.get("blueprint_json").and_then(|v| v.as_str()).unwrap_or("");
    let raw_parsed: Value = serde_json::from_str(blueprint_json_str).unwrap_or_else(|_| json!({}));
    let blueprint_norm = crate::blueprint_block_shape::normalizeBlueprint(&raw_parsed);
    let blocks = blueprint_norm.get("blocks").filter(|v| v.is_array()).cloned().unwrap_or(json!([]));
    let bindings = blueprint_norm.get("bindings").filter(|v| v.is_array()).cloned().unwrap_or(json!([]));
    let coverage = blueprint_norm.get("coverage").filter(|v| v.is_array()).cloned().unwrap_or(json!([]));

    // P4 (AST-skeleton): the atlas-native PLANNED graph + the delta vs the current atlas — lean
    // projection, only the fields the diagram needs.
    let planned_nodes_raw: Vec<Value> = raw_parsed.get("planned_nodes").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let planned_nodes: Vec<Value> = planned_nodes_raw
        .iter()
        .map(|n| {
            json!({
                "id": n.get("id").cloned().unwrap_or(Value::Null),
                "type": n.get("type").cloned().unwrap_or(Value::Null),
                "name": n.get("name").cloned().unwrap_or(Value::Null),
                "path": n.get("path").cloned().unwrap_or(Value::Null),
                "signature": n.get("signature").cloned().unwrap_or(Value::Null),
            })
        })
        .filter(|n| truthy(n.get("id")))
        .collect();
    let planned_edges_raw: Vec<Value> = raw_parsed.get("planned_edges").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let planned_edges: Vec<Value> = planned_edges_raw
        .iter()
        .map(|e| {
            json!({
                "from_node": e.get("from_node").cloned().unwrap_or(Value::Null),
                "to_node": e.get("to_node").cloned().unwrap_or(Value::Null),
                "type": e.get("type").cloned().unwrap_or(Value::Null),
            })
        })
        .filter(|e| truthy(e.get("from_node")) && truthy(e.get("to_node")))
        .collect();
    let delta_raw = raw_parsed.get("delta").cloned().unwrap_or(Value::Null);
    let delta = match &delta_raw {
        Value::Object(_) | Value::Array(_) => delta_raw.clone(),
        _ => Value::Null,
    };
    // the design's code STUBS (path + source) — so the FE can show a node's stub on click.
    let stub_files_raw: Vec<Value> = raw_parsed.get("stub_files").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let stub_files: Vec<Value> = stub_files_raw
        .iter()
        .map(|s| {
            let source = match s.get("source") {
                Some(Value::String(x)) => x.clone(),
                _ => String::new(),
            };
            json!({ "path": s.get("path").cloned().unwrap_or(Value::Null), "source": source })
        })
        .filter(|s| truthy(s.get("path")))
        .collect();

    // the COMPOSED authored files (each file block rendered from its authored snippets) — for the
    // snippets diff. Empty until BUILD authors the snippets. Pure render (blueprint-compose).
    let mut composed_files: Vec<Value> = Vec::new();
    let snip_rows = crate::db_json::query_rows(
        db,
        "SELECT block_id, code, imports_json FROM blueprint_snippets WHERE blueprint_id = ?",
        &[row_id.clone()],
    )?;
    let snips: Vec<Value> = snip_rows.as_array().cloned().unwrap_or_default();
    if !snips.is_empty() {
        let mut authored_map = Map::new();
        for s in &snips {
            let raw = match s.get("imports_json") {
                Some(Value::String(x)) => x.clone(),
                _ => "[]".to_string(),
            };
            let imports = serde_json::from_str::<Value>(&raw).unwrap_or(json!([]));
            let key = js_string(s.get("block_id").unwrap_or(&Value::Null));
            authored_map.insert(key, json!({ "code": s.get("code").cloned().unwrap_or(Value::Null), "imports": imports }));
        }
        let authored_map_val = Value::Object(authored_map);
        if let Some(arr) = blocks.as_array() {
            for fb in arr {
                if fb.get("type").and_then(|v| v.as_str()) != Some("file") {
                    continue;
                }
                let composed = crate::blueprint_blueprint_compose::blueprintCompose(&json!({ "block": fb, "authored": authored_map_val }), db)?;
                let path_v = composed.get("path").cloned().unwrap_or(Value::Null);
                let source_v = composed.get("source").cloned().unwrap_or(Value::Null);
                let source_str = source_v.as_str().unwrap_or("");
                if truthy(Some(&path_v)) && !source_str.is_empty() && !source_str.trim().is_empty() {
                    composed_files.push(json!({ "path": path_v, "source": source_v }));
                }
            }
        }
    }

    // block ids that have an authored snippet (BUILD progress per leaf).
    let mut authored: Vec<Value> = Vec::new();
    let bid_rows = crate::db_json::query_rows(db, "SELECT block_id FROM blueprint_snippets WHERE blueprint_id = ?", &[row_id.clone()])?;
    if let Some(arr) = bid_rows.as_array() {
        for r in arr {
            let bid = r.get("block_id").cloned().unwrap_or(Value::Null);
            if !authored.contains(&bid) {
                authored.push(bid);
            }
        }
    }
    let blocks_out = annotate_blocks(&blocks, &authored);

    // Design (design-vN) + code (snippets-vN) judgments from the counting court.
    let cmeta_rows = crate::db_json::query_rows(db, "SELECT card_slug, acceptance_json FROM contracts WHERE slug = ?", &[contract_slug.clone()])?;
    let cmeta = cmeta_rows.as_array().and_then(|a| a.first()).cloned();
    let card_slug = cmeta.as_ref().and_then(|c| c.get("card_slug")).cloned().unwrap_or(Value::Null);

    // P4 docs + design-digest — sourced from the DESIGN data, NOT the code_nodes projection. Only
    // computed when the design carried stubs (planned_nodes non-empty).
    let mut design_digest: Value = Value::Null;
    let mut design_docs: Vec<Value> = Vec::new();
    if !planned_nodes.is_empty() {
        let acc_str = match cmeta.as_ref().and_then(|c| c.get("acceptance_json")).and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => "[]".to_string(),
        };
        let acc: Vec<Value> = serde_json::from_str::<Value>(&acc_str).ok().and_then(|v| v.as_array().cloned()).unwrap_or_default();

        let mut covered_acc: std::collections::HashSet<String> = std::collections::HashSet::new();
        if let Some(cov_arr) = coverage.as_array() {
            for c in cov_arr {
                let len = c.get("maps_to_blocks").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
                if len > 0 {
                    covered_acc.insert(tmpl(c.get("acceptance_id")));
                }
            }
        }

        let is_file = |n: &Value| n.get("type").and_then(|v| v.as_str()) == Some("file");
        let is_symbol = |n: &Value| {
            let t = n.get("type");
            truthy(t) && t.and_then(|v| v.as_str()) != Some("file")
        };
        let file_nodes: Vec<&Value> = planned_nodes.iter().filter(|n| is_file(n)).collect();
        let symbol_nodes: Vec<&Value> = planned_nodes.iter().filter(|n| is_symbol(n)).collect();

        let mut by_kind = Map::new();
        for n in &symbol_nodes {
            let key = tmpl(n.get("type"));
            let cur = by_kind.get(&key).and_then(|v| v.as_i64()).unwrap_or(0);
            by_kind.insert(key, json!(cur + 1));
        }

        let coverage_pct = if !acc.is_empty() {
            js_num_value(covered_acc.len() as f64 / acc.len() as f64)
        } else {
            Value::Null
        };
        design_digest = json!({
            "files": file_nodes.len() as i64,
            "symbols": symbol_nodes.len() as i64,
            "edges": planned_edges.len() as i64,
            "by_kind": by_kind,
            "acceptance_total": acc.len() as i64,
            "acceptance_covered": covered_acc.len() as i64,
            "coverage_pct": coverage_pct,
            "delta": delta.get("counts").cloned().unwrap_or(Value::Null),
        });

        // a doc-card per planned symbol: name + signature + kind + delta status + the block
        // intent it came from + which acceptance line(s) its file covers.
        let flat = crate::blueprint_block_shape::flattenBlocks(&blocks);
        let mut intent_by_block_id: std::collections::HashMap<String, Value> = std::collections::HashMap::new();
        if let Some(obj) = flat.as_object() {
            for (id, entry) in obj {
                let intent = entry.get("block").and_then(|b| b.get("intent")).cloned().unwrap_or(Value::Null);
                intent_by_block_id.insert(id.clone(), intent);
            }
        }

        design_docs = symbol_nodes
            .iter()
            .map(|n| {
                let p = str_qq(n.get("path"));
                let name = str_qq(n.get("name"));
                let id = n.get("id").cloned().unwrap_or(Value::Null);
                let path_out = if p.is_empty() { Value::Null } else { json!(p) };
                let status = delta.get("by_id").and_then(|bi| bi.get(&tmpl(Some(&id)))).cloned().unwrap_or(Value::Null);
                let intent_key = format!("{}.{}", p, name);
                let intent = intent_by_block_id.get(&intent_key).cloned().unwrap_or(Value::Null);
                let covers = covers_for_path(&coverage, &p);
                json!({
                    "id": id,
                    "name": name,
                    "kind": n.get("type").cloned().unwrap_or(Value::Null),
                    "signature": n.get("signature").cloned().unwrap_or(Value::Null),
                    "path": path_out,
                    "status": status,
                    "intent": intent,
                    "covers": covers,
                })
            })
            .collect();
    }

    // the cached CHANGE narrative (blueprint-describe), if generated. Pure read.
    let origin = format!("blueprint:{}", tmpl(Some(&contract_slug)));
    let cd_rows = crate::db_json::query_rows(db, "SELECT summary_md FROM code_descriptions WHERE origin = ? AND path = '(change)'", &[json!(origin)])?;
    let change_description = cd_rows.as_array().and_then(|a| a.first()).and_then(|r| r.get("summary_md")).cloned().unwrap_or(Value::Null);

    let judgments = json!({
        "design": judgment_for(db, &card_slug, "design", &row_version)?,
        "code": judgment_for(db, &card_slug, "snippets", &row_version)?,
    });
    let lint_passed = row.get("lint_passed").and_then(|v| v.as_i64()) == Some(1);

    let mut out = Map::new();
    out.insert("contract_slug".into(), contract_slug);
    out.insert("blueprint_id".into(), row_id);
    out.insert("version".into(), row_version);
    out.insert("change_description".into(), change_description);
    out.insert("blocks".into(), blocks_out);
    out.insert("bindings".into(), bindings);
    out.insert("coverage".into(), coverage);
    out.insert("planned_nodes".into(), json!(planned_nodes));
    out.insert("planned_edges".into(), json!(planned_edges));
    out.insert("delta".into(), delta);
    out.insert("stub_files".into(), json!(stub_files));
    out.insert("composed_files".into(), json!(composed_files));
    out.insert("design_digest".into(), design_digest);
    out.insert("design_docs".into(), json!(design_docs));
    out.insert("authored_count".into(), json!(authored.len() as i64));
    out.insert("judgments".into(), judgments);
    out.insert("lint_passed".into(), json!(lint_passed));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS judgments;
             CREATE TABLE blueprints (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT, version INTEGER, blueprint_json TEXT, lint_passed INTEGER, superseded_by INTEGER);
             CREATE TABLE blueprint_snippets (id INTEGER PRIMARY KEY AUTOINCREMENT, blueprint_id INTEGER, block_id TEXT, code TEXT, imports_json TEXT);
             CREATE TABLE contracts (slug TEXT, card_slug TEXT, acceptance_json TEXT);
             CREATE TABLE code_descriptions (origin TEXT, path TEXT, summary_md TEXT);
             CREATE TABLE judgments.judgment_cases (judgment_id TEXT, verdict TEXT, goal_line TEXT, n_items INTEGER, defender_trues INTEGER, prosecutor_trues INTEGER, defender_rate REAL, prosecutor_rate REAL, defender_accuracy REAL, prosecutor_accuracy REAL);
             CREATE TABLE judgments.court_evidence (judgment_id TEXT, n_acceptance INTEGER, n_microtasks INTEGER, n_blocks INTEGER, n_nested_blocks INTEGER, has_card INTEGER, has_blueprint INTEGER, has_screening INTEGER, evidence_md TEXT, created_at TEXT);
             CREATE TABLE judgments.judgment_items (judgment_id TEXT, side TEXT, item_id TEXT, statement TEXT, grounded INTEGER, judge_value INTEGER, judge_accuracy REAL, judge_why TEXT);",
        )
        .unwrap();
        c
    }

    #[test]
    fn no_blueprint_returns_null() {
        let c = db();
        c.execute("INSERT INTO contracts (slug, card_slug) VALUES ('CT-N','CARD-N')", []).unwrap();
        let r = blueprintGet(&json!({ "contract_slug": "CT-N" }), &c).unwrap();
        assert_eq!(r, json!({ "contract_slug": "CT-N", "blueprint": null }));
    }

    #[test]
    fn missing_contract_slug_errors() {
        let c = db();
        assert!(blueprintGet(&json!({}), &c).unwrap_err().contains("contract_slug is required"));
    }

    #[test]
    fn reads_blocks_bindings_and_annotates_authored() {
        let c = db();
        let bp = json!({
            "blocks": [{ "id": "src/f.ts", "type": "file", "path": "src/f.ts", "intent": "the file", "children": [
                { "id": "src/f.ts.fn", "type": "function", "intent": "fn", "children": [] },
                { "id": "src/f.ts.T", "type": "type", "intent": "type", "children": [] },
            ] }],
            "bindings": [{ "output": "src/f.ts.T", "input": "src/f.ts.fn", "type": "T" }],
            "coverage": [{ "acceptance_id": "ac1", "maps_to_blocks": ["src/f.ts"] }],
        });
        c.execute("INSERT INTO contracts (slug, card_slug) VALUES ('CT-M','CARD-M')", []).unwrap();
        c.execute("INSERT INTO blueprints (contract_slug, version, blueprint_json, lint_passed) VALUES ('CT-M', 1, ?1, 1)", [bp.to_string()]).unwrap();
        let rows = crate::db_json::query_rows(&c, "SELECT id FROM blueprints WHERE contract_slug='CT-M'", &[]).unwrap();
        let bid = rows[0]["id"].clone();
        c.execute(
            "INSERT INTO blueprint_snippets (blueprint_id, block_id, code, imports_json) VALUES (?1, 'src/f.ts.fn', 'export function fn(){}', '[]')",
            rusqlite::params![bid.as_i64().unwrap()],
        )
        .unwrap();

        let r = blueprintGet(&json!({ "contract_slug": "CT-M" }), &c).unwrap();
        assert_eq!(r["version"], json!(1));
        assert_eq!(r["lint_passed"], json!(true));
        assert_eq!(r["blocks"][0]["path"], json!("src/f.ts"));
        assert_eq!(r["blocks"][0]["children"].as_array().unwrap().len(), 2);
        assert_eq!(r["bindings"][0]["type"], json!("T"));
        assert_eq!(r["authored_count"], json!(1));
        let byid: std::collections::HashMap<String, bool> = r["blocks"][0]["children"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| (e["id"].as_str().unwrap().to_string(), e["authored"].as_bool().unwrap()))
            .collect();
        assert_eq!(byid["src/f.ts.fn"], true);
        assert_eq!(byid["src/f.ts.T"], false);
        assert_eq!(r["composed_files"][0]["path"], json!("src/f.ts"));
        assert!(r["composed_files"][0]["source"].as_str().unwrap().contains("export function"));
    }

    #[test]
    fn planned_graph_delta_and_design_digest() {
        let c = db();
        c.execute("INSERT INTO contracts (slug, card_slug, acceptance_json) VALUES ('CT-D','CARD-D', ?1)", [json!([{ "id": "ac1" }, { "id": "ac2" }]).to_string()]).unwrap();
        let bp = json!({
            "blocks": [{ "id": "src/f.js", "type": "file", "path": "src/f.js", "intent": "the file", "children": [
                { "id": "src/f.js.fn", "type": "function", "intent": "filter the users", "children": [] },
            ] }],
            "bindings": [],
            "coverage": [{ "acceptance_id": "ac1", "maps_to_blocks": ["src/f.js"] }],
            "planned_nodes": [
                { "id": "src/f.js", "type": "file", "name": "src/f.js", "path": "src/f.js" },
                { "id": "src/f.js#fn", "type": "function", "name": "fn", "path": "src/f.js", "signature": "(users)" },
            ],
            "planned_edges": [{ "from_node": "src/f.js#fn", "to_node": "src/f.js", "type": "declares", "extra": "dropme" }],
            "delta": { "by_id": { "src/f.js": "unchanged", "src/f.js#fn": "added" }, "counts": { "added": 1, "changed": 0, "unchanged": 1, "removed": 0 } },
            "stub_files": [{ "path": "src/f.js", "source": "export function fn(){}\n" }],
        });
        c.execute("INSERT INTO blueprints (contract_slug, version, blueprint_json, lint_passed) VALUES ('CT-D', 1, ?1, 1)", [bp.to_string()]).unwrap();

        let r = blueprintGet(&json!({ "contract_slug": "CT-D" }), &c).unwrap();
        assert_eq!(r["planned_nodes"].as_array().unwrap().len(), 2);
        assert_eq!(r["planned_edges"][0]["type"], json!("declares"));
        assert!(r["planned_edges"][0].get("extra").is_none());
        assert_eq!(r["delta"]["counts"]["added"], json!(1));
        assert_eq!(r["stub_files"][0]["path"], json!("src/f.js"));
        assert_eq!(r["design_digest"]["files"], json!(1));
        assert_eq!(r["design_digest"]["symbols"], json!(1));
        assert_eq!(r["design_digest"]["by_kind"]["function"], json!(1));
        assert_eq!(r["design_digest"]["acceptance_total"], json!(2));
        assert_eq!(r["design_digest"]["acceptance_covered"], json!(1));
        assert_eq!(r["design_digest"]["coverage_pct"], json!(0.5));
        let doc = &r["design_docs"][0];
        assert_eq!(doc["name"], json!("fn"));
        assert_eq!(doc["signature"], json!("(users)"));
        assert_eq!(doc["status"], json!("added"));
        assert_eq!(doc["intent"], json!("filter the users"));
        assert_eq!(doc["covers"], json!(["ac1"]));
    }

    #[test]
    fn pre_stub_blueprint_has_empty_planned_and_null_digest() {
        let c = db();
        c.execute("INSERT INTO contracts (slug, card_slug) VALUES ('CT-M','CARD-M')", []).unwrap();
        c.execute("INSERT INTO blueprints (contract_slug, version, blueprint_json, lint_passed) VALUES ('CT-M', 1, '{}', 1)", []).unwrap();
        let r = blueprintGet(&json!({ "contract_slug": "CT-M" }), &c).unwrap();
        assert_eq!(r["planned_nodes"], json!([]));
        assert_eq!(r["planned_edges"], json!([]));
        assert_eq!(r["delta"], Value::Null);
        assert_eq!(r["design_digest"], Value::Null);
        assert_eq!(r["design_docs"], json!([]));
    }

    #[test]
    fn judgment_verdict_evidence_and_items() {
        let c = db();
        c.execute("INSERT INTO contracts (slug, card_slug) VALUES ('CT-M','CARD-M')", []).unwrap();
        c.execute("INSERT INTO blueprints (contract_slug, version, blueprint_json, lint_passed) VALUES ('CT-M', 1, '{}', 1)", []).unwrap();
        c.execute(
            "INSERT INTO judgments.judgment_cases (judgment_id, verdict, goal_line, n_items, defender_trues, prosecutor_trues, defender_accuracy, prosecutor_accuracy) VALUES ('CARD-M::blueprint:design-v1','pass','goal',3,2,1,0.9,0.1)",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO judgments.court_evidence (judgment_id, n_blocks, n_acceptance, has_card, has_blueprint, has_screening, evidence_md) VALUES ('CARD-M::blueprint:design-v1',1,2,1,1,0,'EVIDENCE')",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO judgments.judgment_items (judgment_id, side, item_id, statement, grounded, judge_value, judge_accuracy, judge_why) VALUES ('CARD-M::blueprint:design-v1','prosecutor','p1','no evidence',1,1,0.9,'a real defect')",
            [],
        )
        .unwrap();
        let r = blueprintGet(&json!({ "contract_slug": "CT-M" }), &c).unwrap();
        let d = &r["judgments"]["design"];
        assert_eq!(d["verdict"], json!("pass"));
        assert_eq!(d["goal_line"], json!("goal"));
        assert_eq!(d["defender_accuracy"], json!(0.9));
        assert_eq!(d["evidence"]["n_blocks"], json!(1));
        assert_eq!(d["evidence"]["has_card"], json!(true));
        assert_eq!(d["evidence"]["has_screening"], json!(false));
        assert_eq!(d["evidence"]["evidence_md"], json!("EVIDENCE"));
        assert_eq!(d["items"]["prosecutor"][0]["statement"], json!("no evidence"));
        assert_eq!(d["items"]["prosecutor"][0]["value"], json!(true));
        assert_eq!(d["items"]["prosecutor"][0]["why"], json!("a real defect"));
        assert_eq!(r["judgments"]["code"], Value::Null);
    }

    #[test]
    fn judgment_with_only_gathered_evidence_still_surfaces() {
        let c = db();
        c.execute("INSERT INTO contracts (slug, card_slug) VALUES ('CT-M','CARD-M')", []).unwrap();
        c.execute("INSERT INTO blueprints (contract_slug, version, blueprint_json, lint_passed) VALUES ('CT-M', 1, '{}', 1)", []).unwrap();
        c.execute("INSERT INTO judgments.court_evidence (judgment_id, n_blocks) VALUES ('CARD-M::blueprint:design-v1',1)", []).unwrap();
        let r = blueprintGet(&json!({ "contract_slug": "CT-M" }), &c).unwrap();
        assert_eq!(r["judgments"]["design"]["verdict"], Value::Null);
        assert_eq!(r["judgments"]["design"]["evidence"]["n_blocks"], json!(1));
    }

    #[test]
    fn no_card_slug_means_no_judgments_at_all() {
        let c = db();
        c.execute("INSERT INTO contracts (slug, card_slug) VALUES ('CT-X', NULL)", []).unwrap();
        c.execute("INSERT INTO blueprints (contract_slug, version, blueprint_json, lint_passed) VALUES ('CT-X', 1, '{}', 1)", []).unwrap();
        let r = blueprintGet(&json!({ "contract_slug": "CT-X" }), &c).unwrap();
        assert_eq!(r["judgments"]["design"], Value::Null);
        assert_eq!(r["judgments"]["code"], Value::Null);
    }

    #[test]
    fn change_description_surfaces_when_present() {
        let c = db();
        c.execute("INSERT INTO contracts (slug, card_slug) VALUES ('CT-M','CARD-M')", []).unwrap();
        c.execute("INSERT INTO blueprints (contract_slug, version, blueprint_json, lint_passed) VALUES ('CT-M', 1, '{}', 1)", []).unwrap();
        c.execute("INSERT INTO code_descriptions (origin, path, summary_md) VALUES ('blueprint:CT-M', '(change)', 'renamed the filter fn')", []).unwrap();
        let r = blueprintGet(&json!({ "contract_slug": "CT-M" }), &c).unwrap();
        assert_eq!(r["change_description"], json!("renamed the filter fn"));
    }
}

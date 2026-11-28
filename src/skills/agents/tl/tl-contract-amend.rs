//! tl-contract-amend.rs — twin of skills/agents/tl/tl-contract-amend.mjs (db WRITE lane). NAMES ARE
//! CONTRACT: `tlContractAmend` (default). TL applies the first unconsumed contract-amendment microtask
//! from the latest failed qa_run: mutate one contract column per amendment_type, bump amendments_applied,
//! stamp the suggestion consumed. Idempotent (consumed_at skips on re-run). frozen-clock updated_at.
// QUALITY: 4 amendment types · Set-dedupe widen · undefined-hint omission · no-op returns pass amendments_applied through
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
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }

// JS safeArray(json): falsy json (null/'') → []; JSON.parse then Array?arr:[]; parse error → [].
fn safe_array(json: Option<&Value>) -> Vec<Value> {
    match json {
        Some(Value::String(s)) if !s.is_empty() => match serde_json::from_str::<Value>(s) {
            Ok(Value::Array(a)) => a,
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

struct Amendment { files: Value, acceptance: Value, goal: Value, microtasks: Value, changes: Value }

// Mirror JS applyAmendment(contract, microtask): returns None for unknown/invalid (→ caller throws).
fn apply_amendment(contract: &Value, microtask: &Value) -> Option<Amendment> {
    // `next` seeded from the contract's current column values (unchanged columns written back as-is).
    let mut next = Amendment {
        files: contract.get("files_touched_json").cloned().unwrap_or(Value::Null),
        acceptance: contract.get("acceptance_json").cloned().unwrap_or(Value::Null),
        goal: contract.get("goal_md").cloned().unwrap_or(Value::Null),
        microtasks: contract.get("microtasks_json").cloned().unwrap_or(Value::Null),
        changes: Value::Null,
    };
    let payload = nz(microtask.get("amendment_payload")).cloned().unwrap_or(json!({}));

    match microtask.get("amendment_type").and_then(|v| v.as_str()) {
        Some("widen-files-touched") => {
            let current = safe_array(contract.get("files_touched_json"));
            let incoming: Vec<Value> = payload.get("paths").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            // merged = [...new Set([...current, ...incoming])] — first-occurrence order, dedupe by value.
            let mut merged: Vec<Value> = Vec::new();
            for v in current.iter().chain(incoming.iter()) {
                if !merged.contains(v) { merged.push(v.clone()); }
            }
            next.files = json!(serde_json::to_string(&Value::Array(merged)).ok()?);
            let widened: Vec<Value> = incoming.iter().filter(|p| !current.contains(*p)).cloned().collect();
            next.changes = json!({ "widened": widened });
            Some(next)
        }
        Some("add-acceptance-criterion") => {
            if !truthy(payload.get("text")) { return None; }
            let text = payload.get("text").cloned().unwrap_or(Value::Null);
            let mut current = safe_array(contract.get("acceptance_json"));
            let mut item = Map::new();
            item.insert("kind".into(), nz(payload.get("ac_kind")).cloned().unwrap_or(json!("behavioral")));
            item.insert("text".into(), text.clone());
            current.push(Value::Object(item));
            next.acceptance = json!(serde_json::to_string(&Value::Array(current)).ok()?);
            next.changes = json!({ "added_acceptance": text });
            Some(next)
        }
        Some("refine-goal") => {
            if !truthy(payload.get("new_goal_md")) { return None; }
            next.goal = payload.get("new_goal_md").cloned().unwrap_or(Value::Null);
            next.changes = json!({ "goal_replaced": true });
            Some(next)
        }
        Some("add-microtask-step") => {
            let m = payload.get("microtask");
            if m.is_none() || !truthy(m.and_then(|x| x.get("intent"))) { return None; }
            let m = m.unwrap();
            let mut current = safe_array(contract.get("microtasks_json"));
            let mut step = Map::new();
            step.insert(
                "microtask_id".into(),
                nz(m.get("microtask_id")).cloned().unwrap_or_else(|| json!(format!("amendment-{}", current.len() + 1))),
            );
            step.insert("intent".into(), m.get("intent").cloned().unwrap_or(Value::Null));
            step.insert("kind".into(), nz(m.get("kind")).cloned().unwrap_or(json!("code")));
            // hint: `hint: m.hint` — undefined/absent is dropped by JSON.stringify; explicit null is kept.
            if let Some(hint) = m.get("hint") { step.insert("hint".into(), hint.clone()); }
            current.push(Value::Object(step));
            next.microtasks = json!(serde_json::to_string(&Value::Array(current)).ok()?);
            let added = nz(m.get("microtask_id")).cloned()
                .unwrap_or_else(|| m.get("intent").cloned().unwrap_or(Value::Null));
            next.changes = json!({ "added_microtask": added });
            Some(next)
        }
        _ => None,
    }
}

pub fn tlContractAmend(input: &Value, db: &Connection) -> Result<Value, String> {
    // guard-throws (contract_slug missing / not found / wrong status / malformed suggestions / unknown
    // amendment_type): mirrored as Err(String); only the success + no-op paths are fixtured.
    let contract_slug = match input.get("contract_slug") {
        Some(v) if truthy(Some(v)) => v.clone(),
        _ => return Err("tl-contract-amend: contract_slug is required".into()),
    };

    let contract_rows = crate::db_json::query_rows(
        db,
        "SELECT slug, status, files_touched_json, acceptance_json, goal_md, microtasks_json, amendments_applied FROM contracts WHERE slug = ?",
        &[contract_slug.clone()],
    )?;
    let contract = match contract_rows.as_array().and_then(|a| a.first()) {
        Some(c) => c.clone(),
        None => return Err(format!("contract '{}' not found", contract_slug.as_str().unwrap_or(""))),
    };
    if contract.get("status").and_then(|v| v.as_str()) != Some("impl") {
        return Err("tl-contract-amend: amendments only apply during impl".into());
    }

    let amendments_applied = nz(contract.get("amendments_applied")).cloned().unwrap_or(json!(0));

    let qa_rows = crate::db_json::query_rows(
        db,
        "SELECT id, suggested_microtasks_json FROM qa_runs WHERE contract_slug = ? AND all_passed = 0 ORDER BY id DESC LIMIT 1",
        &[contract_slug.clone()],
    )?;
    let qa_rows = qa_rows.as_array().cloned().unwrap_or_default();
    let qa_run = qa_rows.first();
    let sug_json = qa_run.and_then(|r| r.get("suggested_microtasks_json"));

    // !qaRun?.suggested_microtasks_json → nothing to amend.
    if !truthy(sug_json) {
        let mut out = Map::new();
        out.insert("contract_slug".into(), contract_slug.clone());
        out.insert("applied".into(), json!(false));
        out.insert("reason".into(), json!("no failed qa_run with suggested_microtasks_json — nothing to amend"));
        out.insert("amendments_applied".into(), amendments_applied.clone());
        return Ok(Value::Object(out));
    }
    let qa_run = qa_run.unwrap();
    let qa_id = qa_run.get("id").cloned().unwrap_or(Value::Null);

    let mut suggestions: Vec<Value> = match sug_json.and_then(|v| v.as_str()).map(serde_json::from_str::<Value>) {
        Some(Ok(Value::Array(a))) => a,
        Some(Ok(_)) => return Err(format!("qa_run {} suggested_microtasks_json is not an array", qa_id)),
        _ => return Err(format!("qa_run {} has malformed suggested_microtasks_json", qa_id)),
    };

    // first unconsumed contract-amendment microtask
    let idx = suggestions.iter().position(|m| {
        truthy(Some(m))
            && m.get("kind").and_then(|v| v.as_str()) == Some("contract-amendment")
            && !truthy(m.get("consumed_at"))
    });
    let idx = match idx {
        Some(i) => i,
        None => {
            let mut out = Map::new();
            out.insert("contract_slug".into(), contract_slug.clone());
            out.insert("applied".into(), json!(false));
            out.insert("reason".into(), json!("no unconsumed contract-amendment microtask in latest failed qa_run"));
            out.insert("amendments_applied".into(), amendments_applied.clone());
            return Ok(Value::Object(out));
        }
    };
    let microtask = suggestions[idx].clone();

    let updates = match apply_amendment(&contract, &microtask) {
        Some(u) => u,
        None => return Err(format!(
            "unknown amendment_type '{}' — supported: widen-files-touched, add-acceptance-criterion, refine-goal, add-microtask-step",
            microtask.get("amendment_type").and_then(|v| v.as_str()).unwrap_or("")
        )),
    };

    crate::db_json::run_write(
        db,
        &format!("UPDATE contracts SET files_touched_json = ?, acceptance_json = ?, goal_md = ?, microtasks_json = ?, amendments_applied = amendments_applied + 1, updated_at = {} WHERE slug = ?", crate::clock::now_sql()),
        &[updates.files, updates.acceptance, updates.goal, updates.microtasks, contract_slug.clone()],
    )?;

    // { ...microtask, consumed_at }: clone the map (keeps key order), insert consumed_at (updates in
    // place if present, appends otherwise) — exactly JS object-spread + reassignment semantics.
    let mut stamped = microtask.as_object().cloned().unwrap_or_default();
    stamped.insert("consumed_at".into(), json!(crate::clock::now_iso_nodb()));
    suggestions[idx] = Value::Object(stamped);
    let suggestions_json = serde_json::to_string(&Value::Array(suggestions)).map_err(|e| e.to_string())?;
    crate::db_json::run_write(
        db,
        "UPDATE qa_runs SET suggested_microtasks_json = ? WHERE id = ?",
        &[json!(suggestions_json), qa_id.clone()],
    )?;

    let new_count = nz(Some(&amendments_applied)).and_then(|v| v.as_i64()).unwrap_or(0) + 1;
    let mut out = Map::new();
    out.insert("contract_slug".into(), contract_slug);
    out.insert("applied".into(), json!(true));
    if let Some(v) = microtask.get("amendment_type") { out.insert("amendment_type".into(), v.clone()); }
    if let Some(v) = microtask.get("microtask_id") { out.insert("microtask_id".into(), v.clone()); }
    out.insert("qa_run_id".into(), qa_id);
    out.insert("amendments_applied".into(), json!(new_count));
    out.insert("changes".into(), updates.changes);
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE contracts (slug TEXT PRIMARY KEY, status TEXT, files_touched_json TEXT, acceptance_json TEXT, goal_md TEXT, microtasks_json TEXT, amendments_applied INTEGER, updated_at TEXT); \
             CREATE TABLE qa_runs (id INTEGER PRIMARY KEY, contract_slug TEXT, all_passed INTEGER, suggested_microtasks_json TEXT);",
        ).unwrap();
        c
    }
    fn contract(c: &Connection, slug: &str, files: &str, accept: &str, goal: &str, micros: &str) {
        c.execute(
            "INSERT INTO contracts (slug, status, files_touched_json, acceptance_json, goal_md, microtasks_json, amendments_applied, updated_at) VALUES (?1,'impl',?2,?3,?4,?5,0,'2025-12-01 00:00:00')",
            params![slug, files, accept, goal, micros],
        ).unwrap();
    }
    fn qa(c: &Connection, id: i64, slug: &str, sug: &str) {
        c.execute(
            "INSERT INTO qa_runs (id, contract_slug, all_passed, suggested_microtasks_json) VALUES (?1,?2,0,?3)",
            params![id, slug, sug],
        ).unwrap();
    }

    #[test]
    fn widen_dedupes_and_bumps() {
        let c = db();
        contract(&c, "C1", "[\"src/a.ts\",\"src/b.ts\"]", "[]", "g", "[]");
        qa(&c, 1, "C1", "[{\"microtask_id\":\"w\",\"kind\":\"contract-amendment\",\"amendment_type\":\"widen-files-touched\",\"amendment_payload\":{\"paths\":[\"src/b.ts\",\"src/c.ts\"]}}]");
        let r = tlContractAmend(&json!({ "contract_slug": "C1" }), &c).unwrap();
        assert_eq!(r["applied"], json!(true));
        assert_eq!(r["amendment_type"], json!("widen-files-touched"));
        assert_eq!(r["amendments_applied"], json!(1));
        assert_eq!(r["changes"], json!({ "widened": ["src/c.ts"] }));
        let stored: String = c.query_row("SELECT files_touched_json FROM contracts WHERE slug='C1'", [], |r| r.get(0)).unwrap();
        assert_eq!(stored, "[\"src/a.ts\",\"src/b.ts\",\"src/c.ts\"]");
        let bumped: i64 = c.query_row("SELECT amendments_applied FROM contracts WHERE slug='C1'", [], |r| r.get(0)).unwrap();
        assert_eq!(bumped, 1);
        // suggestion stamped consumed
        let sug: String = c.query_row("SELECT suggested_microtasks_json FROM qa_runs WHERE id=1", [], |r| r.get(0)).unwrap();
        assert!(sug.contains("\"consumed_at\":\"2026-01-01T00:00:00.000Z\""));
    }

    #[test]
    fn add_microtask_defaults_id_and_omits_hint() {
        let c = db();
        contract(&c, "C2", "[]", "[]", "g", "[{\"microtask_id\":\"m1\",\"intent\":\"pre\",\"kind\":\"code\"}]");
        qa(&c, 2, "C2", "[{\"microtask_id\":\"s\",\"kind\":\"contract-amendment\",\"amendment_type\":\"add-microtask-step\",\"amendment_payload\":{\"microtask\":{\"intent\":\"auto step\"}}}]");
        let r = tlContractAmend(&json!({ "contract_slug": "C2" }), &c).unwrap();
        assert_eq!(r["applied"], json!(true));
        assert_eq!(r["changes"], json!({ "added_microtask": "auto step" }));
        let stored: String = c.query_row("SELECT microtasks_json FROM contracts WHERE slug='C2'", [], |r| r.get(0)).unwrap();
        assert_eq!(stored, "[{\"microtask_id\":\"m1\",\"intent\":\"pre\",\"kind\":\"code\"},{\"microtask_id\":\"amendment-2\",\"intent\":\"auto step\",\"kind\":\"code\"}]");
    }

    #[test]
    fn no_amendment_is_noop() {
        let c = db();
        contract(&c, "C3", "[]", "[]", "g", "[]");
        qa(&c, 3, "C3", "[{\"microtask_id\":\"x\",\"intent\":\"add tests\",\"kind\":\"test\"}]");
        let r = tlContractAmend(&json!({ "contract_slug": "C3" }), &c).unwrap();
        assert_eq!(r["applied"], json!(false));
        assert_eq!(r["amendments_applied"], json!(0));
        assert!(r["reason"].as_str().unwrap().contains("no unconsumed contract-amendment"));
    }

    #[test]
    fn no_qa_run_is_noop() {
        let c = db();
        contract(&c, "C4", "[]", "[]", "g", "[]");
        let r = tlContractAmend(&json!({ "contract_slug": "C4" }), &c).unwrap();
        assert_eq!(r["applied"], json!(false));
        assert!(r["reason"].as_str().unwrap().contains("no failed qa_run"));
    }
}

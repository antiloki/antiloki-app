//! heimdall-tag-add.rs — twin of skills/heimdall/heimdall-tag-add.mjs (db lane, cross-db ATTACH:
//! brain, async-over-sync). NAMES ARE CONTRACT: `heimdallTagAdd` (default). HUMAN TAGGING (plan
//! §0 #13): one click = one tag row + one evidence row riding the call's casting (provenance
//! 'human', trust 1.0 in the ledger blend). skip=true records a TRAINING-tab pass with no ledger
//! effect. Re-tagging the same call REPLACES the prior tag + its evidence (latest word wins). A
//! human verdict also VOIDS any AI-judge/AI-panel evidence on the call and flags the ai_tags row
//! as superseded (best-effort — an absent ai_tags table is tolerated, mirroring the JS try/catch).
// QUALITY: n/a — straight-line CRUD: skip-insert, replace-prior, supersede-ai, insert tag, ride-casting evidence

use rusqlite::Connection;
use serde_json::{json, Value};

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) } },
        Some(_) => f64::NAN,
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
// JS `String(v)` where v may be `undefined` (a missing key) — used for the surface enum check.
fn string_of(v: Option<&Value>) -> String {
    match v { None => "undefined".into(), Some(x) => js_string(x) }
}
// JS number → JSON: integer-valued numbers render without a trailing ".0" (gotcha #2).
fn num_json(f: f64) -> Value {
    if f.is_finite() && f.fract() == 0.0 && f.abs() < 9e15 { json!(f as i64) } else { json!(f) }
}

const SURFACES: [&str; 5] = ["chat", "card", "pr", "playground", "castings"];

pub fn heimdallTagAdd(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const { db } = context ?? {}; if (!db) throw err.contextMissing(...)` — the db lane
    // always injects a db, so this guard-throw is unreachable and not transcribed (gotcha 9).
    let ai_call_id_num = js_number(input.get("ai_call_id"));
    if !ai_call_id_num.is_finite() || ai_call_id_num <= 0.0 {
        return Err("heimdall-tag-add: ai_call_id is required".into()); // guard-throw, not fixtured
    }
    let ai_call_id_i = ai_call_id_num as i64;
    let ai_call_id_v = num_json(ai_call_id_num);

    // TRAINING-tab skip: strict `=== true`, not truthy — persistent, no ledger effect.
    if input.get("skip") == Some(&Value::Bool(true)) {
        crate::db_json::run_write(
            db,
            "INSERT OR IGNORE INTO brain.heimdall_tag_skips (ai_call_id) VALUES (?1)",
            &[json!(ai_call_id_i)],
        )?;
        return Ok(json!({ "ok": true, "skipped": true, "ai_call_id": ai_call_id_v }));
    }

    let verdict = js_number(input.get("verdict"));
    if ![-2.0, -1.0, 1.0, 2.0].contains(&verdict) {
        return Err("heimdall-tag-add: verdict must be one of: -2, -1, 1, 2".into()); // not fixtured
    }
    let verdict_v = num_json(verdict);

    // `SURFACES.has(String(input?.surface))` — String(), not `??`; undefined → "undefined" (miss).
    let surface_str = string_of(input.get("surface"));
    let surface = if SURFACES.contains(&surface_str.as_str()) { surface_str } else { "castings".to_string() };

    // `input?.reason_md ? String(input.reason_md).slice(0, 2000) : null` — truthiness, not `??`.
    let reason_md: Value = if truthy(input.get("reason_md")) {
        let s = js_string(input.get("reason_md").unwrap());
        json!(s.chars().take(2000).collect::<String>())
    } else {
        Value::Null
    };

    // latest word wins: replace any prior tag (and its evidence) for this call
    let prior_rows = crate::db_json::query_rows(
        db,
        "SELECT id FROM brain.heimdall_human_tags WHERE ai_call_id = ? ORDER BY id DESC LIMIT 1",
        &[json!(ai_call_id_i)],
    )?;
    if let Some(prior_id) = prior_rows.as_array().and_then(|a| a.first()).and_then(|r| r.get("id")).cloned() {
        crate::db_json::run_write(db, "DELETE FROM brain.heimdall_evidence WHERE human_tag_id = ?1", &[prior_id.clone()])?;
        crate::db_json::run_write(db, "DELETE FROM brain.heimdall_human_tags WHERE id = ?1", &[prior_id])?;
    }

    // HUMAN > AI: a human verdict SUPERSEDES any AI tag on this call — void the ai_judge/ai_panel
    // evidence (no double-count) but KEEP the ai_tags record (AI-tagging tab still measures
    // agreement). Idempotent + best-effort: an absent ai_tags table is tolerated (JS try/catch).
    let _ = crate::db_json::run_write(
        db,
        "DELETE FROM brain.heimdall_evidence WHERE ai_call_id = ?1 AND provenance IN ('ai_judge','ai_panel')",
        &[json!(ai_call_id_i)],
    );
    let _ = crate::db_json::run_write(
        db,
        "UPDATE brain.heimdall_ai_tags SET superseded_by_human = 1 WHERE target_ai_call_id = ?1",
        &[json!(ai_call_id_i)],
    );

    let (_changes, tag_id) = crate::db_json::run_write(
        db,
        "INSERT INTO brain.heimdall_human_tags (verdict, reason_md, surface, ai_call_id) VALUES (?1, ?2, ?3, ?4)",
        &[verdict_v.clone(), reason_md, json!(surface), json!(ai_call_id_i)],
    )?;

    // evidence rides the call's casting (model/category/template of the actual resolution)
    let mut evidence_written = false;
    let casting_rows = crate::db_json::query_rows(
        db,
        "SELECT category, template_base, effective_model FROM brain.heimdall_castings WHERE ai_call_id = ? ORDER BY id DESC LIMIT 1",
        &[json!(ai_call_id_i)],
    )?;
    if let Some(casting) = casting_rows.as_array().and_then(|a| a.first()) {
        if truthy(casting.get("effective_model")) {
            let outcome = if verdict > 0.0 { "clean" } else { "fail" };
            crate::db_json::run_write(
                db,
                "INSERT INTO brain.heimdall_evidence (model_slug, category, template_base, provenance, outcome, ai_call_id, human_tag_id) VALUES (?1, ?2, ?3, 'human', ?4, ?5, ?6)",
                &[
                    casting.get("effective_model").cloned().unwrap_or(Value::Null),
                    casting.get("category").cloned().unwrap_or(Value::Null),
                    casting.get("template_base").cloned().unwrap_or(Value::Null),
                    json!(outcome),
                    json!(ai_call_id_i),
                    json!(tag_id),
                ],
            )?;
            evidence_written = true;
        }
    }

    Ok(json!({ "ok": true, "tag_id": tag_id, "verdict": verdict_v, "evidence_written": evidence_written }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema(db: &Connection) {
        db.execute_batch("ATTACH ':memory:' AS brain;").unwrap();
        db.execute_batch(
            "CREATE TABLE brain.heimdall_tag_skips (ai_call_id INTEGER PRIMARY KEY);
             CREATE TABLE brain.heimdall_human_tags (id INTEGER PRIMARY KEY AUTOINCREMENT, verdict INTEGER, reason_md TEXT, surface TEXT, ai_call_id INTEGER);
             CREATE TABLE brain.heimdall_evidence (id INTEGER PRIMARY KEY AUTOINCREMENT, model_slug TEXT, category TEXT, template_base TEXT, provenance TEXT, outcome TEXT, ai_call_id INTEGER, human_tag_id INTEGER);
             CREATE TABLE brain.heimdall_castings (id INTEGER PRIMARY KEY AUTOINCREMENT, ai_call_id INTEGER, category TEXT, template_base TEXT, effective_model TEXT);
             CREATE TABLE brain.heimdall_ai_tags (id INTEGER PRIMARY KEY AUTOINCREMENT, target_ai_call_id INTEGER, superseded_by_human INTEGER NOT NULL DEFAULT 0);",
        ).unwrap();
    }
    fn seeded_db() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        schema(&db);
        db
    }

    #[test]
    fn skip_inserts_and_is_idempotent() {
        let db = seeded_db();
        let r = heimdallTagAdd(&json!({ "ai_call_id": 501, "skip": true }), &db).unwrap();
        assert_eq!(r, json!({ "ok": true, "skipped": true, "ai_call_id": 501 }));
        let n: i64 = db.query_row("SELECT COUNT(*) FROM brain.heimdall_tag_skips", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        // INSERT OR IGNORE: re-skipping the same call is a no-op
        let r2 = heimdallTagAdd(&json!({ "ai_call_id": 501, "skip": true }), &db).unwrap();
        assert_eq!(r2, json!({ "ok": true, "skipped": true, "ai_call_id": 501 }));
        let n2: i64 = db.query_row("SELECT COUNT(*) FROM brain.heimdall_tag_skips", [], |r| r.get(0)).unwrap();
        assert_eq!(n2, 1);
    }

    #[test]
    fn positive_verdict_writes_clean_evidence_riding_casting() {
        let db = seeded_db();
        db.execute("INSERT INTO brain.heimdall_castings (ai_call_id, category, template_base, effective_model) VALUES (10, 'conversational', 'chat', 'model-a')", []).unwrap();
        let r = heimdallTagAdd(&json!({ "ai_call_id": 10, "verdict": 2, "reason_md": "great answer", "surface": "chat" }), &db).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["verdict"], json!(2));
        assert_eq!(r["evidence_written"], json!(true));
        let tag_id = r["tag_id"].as_i64().unwrap();
        assert_eq!(tag_id, 1);
        let (rmd, surf): (String, String) = db.query_row("SELECT reason_md, surface FROM brain.heimdall_human_tags WHERE id=?1", [tag_id], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
        assert_eq!(rmd, "great answer");
        assert_eq!(surf, "chat");
        let (model, cat, base, prov, outcome, human_tag_id): (String, String, String, String, String, i64) = db.query_row(
            "SELECT model_slug, category, template_base, provenance, outcome, human_tag_id FROM brain.heimdall_evidence WHERE ai_call_id=10",
            [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
        ).unwrap();
        assert_eq!((model.as_str(), cat.as_str(), base.as_str(), prov.as_str(), outcome.as_str(), human_tag_id), ("model-a", "conversational", "chat", "human", "clean", tag_id));
    }

    #[test]
    fn negative_verdict_writes_fail_evidence_falsy_reason_and_bad_surface_default() {
        let db = seeded_db();
        db.execute("INSERT INTO brain.heimdall_castings (ai_call_id, category, template_base, effective_model) VALUES (20, 'writing', 'summarize', 'model-b')", []).unwrap();
        let r = heimdallTagAdd(&json!({ "ai_call_id": "20", "verdict": -1, "reason_md": "", "surface": "bogus" }), &db).unwrap();
        assert_eq!(r["verdict"], json!(-1));
        assert_eq!(r["evidence_written"], json!(true));
        let tag_id = r["tag_id"].as_i64().unwrap();
        let (rmd, surf): (Option<String>, String) = db.query_row("SELECT reason_md, surface FROM brain.heimdall_human_tags WHERE id=?1", [tag_id], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
        assert_eq!(rmd, None); // "" is falsy → null
        assert_eq!(surf, "castings"); // invalid surface → default
        let outcome: String = db.query_row("SELECT outcome FROM brain.heimdall_evidence WHERE ai_call_id=20", [], |row| row.get(0)).unwrap();
        assert_eq!(outcome, "fail");
    }

    #[test]
    fn no_casting_row_skips_evidence() {
        let db = seeded_db();
        let r = heimdallTagAdd(&json!({ "ai_call_id": 30, "verdict": 1 }), &db).unwrap();
        assert_eq!(r["evidence_written"], json!(false));
        let n: i64 = db.query_row("SELECT COUNT(*) FROM brain.heimdall_evidence", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn null_effective_model_skips_evidence() {
        let db = seeded_db();
        db.execute("INSERT INTO brain.heimdall_castings (ai_call_id, category, template_base, effective_model) VALUES (40, 'writing', 'summarize', NULL)", []).unwrap();
        let r = heimdallTagAdd(&json!({ "ai_call_id": 40, "verdict": 1 }), &db).unwrap();
        assert_eq!(r["evidence_written"], json!(false));
    }

    #[test]
    fn retagging_replaces_prior_tag_and_its_evidence() {
        let db = seeded_db();
        db.execute("INSERT INTO brain.heimdall_castings (ai_call_id, category, template_base, effective_model) VALUES (50, 'conversational', 'chat', 'model-a')", []).unwrap();
        let first = heimdallTagAdd(&json!({ "ai_call_id": 50, "verdict": 2 }), &db).unwrap();
        let first_tag_id = first["tag_id"].as_i64().unwrap();
        let second = heimdallTagAdd(&json!({ "ai_call_id": 50, "verdict": -2 }), &db).unwrap();
        let second_tag_id = second["tag_id"].as_i64().unwrap();
        assert_ne!(first_tag_id, second_tag_id);
        let tags_n: i64 = db.query_row("SELECT COUNT(*) FROM brain.heimdall_human_tags WHERE ai_call_id=50", [], |r| r.get(0)).unwrap();
        assert_eq!(tags_n, 1); // prior replaced, not stacked
        let ev_n: i64 = db.query_row("SELECT COUNT(*) FROM brain.heimdall_evidence WHERE ai_call_id=50", [], |r| r.get(0)).unwrap();
        assert_eq!(ev_n, 1);
        let outcome: String = db.query_row("SELECT outcome FROM brain.heimdall_evidence WHERE ai_call_id=50", [], |r| r.get(0)).unwrap();
        assert_eq!(outcome, "fail"); // reflects the SECOND (latest) verdict
    }

    #[test]
    fn human_verdict_supersedes_ai_evidence_and_flags_ai_tag() {
        let db = seeded_db();
        // The human-evidence insert is gated on a heimdall_castings row existing for this
        // ai_call_id (both JS and Rust: `if (casting?.effective_model)`) — without it, the human
        // verdict still supersedes AI evidence/flags the tag, but writes NO evidence row of its
        // own. Seed one so this test's real intent (verify the 'human' row gets written) holds.
        db.execute("INSERT INTO brain.heimdall_castings (ai_call_id, category, template_base, effective_model) VALUES (60, 'conversational', 'chat', 'model-a')", []).unwrap();
        db.execute("INSERT INTO brain.heimdall_evidence (model_slug, category, template_base, provenance, outcome, ai_call_id) VALUES ('model-a','conversational','chat','ai_judge','fail',60)", []).unwrap();
        db.execute("INSERT INTO brain.heimdall_evidence (model_slug, category, template_base, provenance, outcome, ai_call_id) VALUES ('model-a','conversational','chat','prod_gate','clean',60)", []).unwrap();
        db.execute("INSERT INTO brain.heimdall_ai_tags (target_ai_call_id, superseded_by_human) VALUES (60, 0)", []).unwrap();
        heimdallTagAdd(&json!({ "ai_call_id": 60, "verdict": 1 }), &db).unwrap();
        let remaining: Vec<String> = {
            let mut stmt = db.prepare("SELECT provenance FROM brain.heimdall_evidence WHERE ai_call_id=60 ORDER BY provenance").unwrap();
            stmt.query_map([], |r| r.get(0)).unwrap().collect::<Result<Vec<_>, _>>().unwrap()
        };
        assert_eq!(remaining, vec!["human".to_string(), "prod_gate".to_string()]); // ai_judge voided, prod_gate kept
        let superseded: i64 = db.query_row("SELECT superseded_by_human FROM brain.heimdall_ai_tags WHERE target_ai_call_id=60", [], |r| r.get(0)).unwrap();
        assert_eq!(superseded, 1);
    }

    #[test]
    fn reason_md_slices_to_2000_chars() {
        let db = seeded_db();
        let long = "x".repeat(2005);
        let r = heimdallTagAdd(&json!({ "ai_call_id": 70, "verdict": 1, "reason_md": long }), &db).unwrap();
        let tag_id = r["tag_id"].as_i64().unwrap();
        let stored: String = db.query_row("SELECT reason_md FROM brain.heimdall_human_tags WHERE id=?1", [tag_id], |row| row.get(0)).unwrap();
        assert_eq!(stored.len(), 2000);
    }
}

//! constitution-paper.rs — twin of skills/constitution/constitution-paper.mjs (db lane; cross-db via
//! ATTACH to laws.sqlite + judgments.sqlite through the ensure-twin). NAMES ARE CONTRACT:
//! `resolvePaperSource` (named, db-first helper) + `constitutionPaper` (default). THE PAPER HISTORY
//! (operator 2026-07-19): every generated study is a row in judgments.review_papers — paper_json
//! holds the full session, markdown_md the rendered study. resolvePaperSource is the artifact
//! skills' ONLY legal source (a saved paper by slug or id) — raw markdown/reviews are not sources.
// QUALITY: 78/100 — op=save's ON CONFLICT(slug) DO UPDATE deliberately leaves swarm_id/snapshot_id
// untouched on re-save (only the JS INSERT column list, not the twin, decides that); markdown_md
// COALESCEs so a re-save without markdown never blanks a previously-saved one.
use rusqlite::Connection;
use serde_json::{json, Value};
use std::sync::OnceLock;

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } } // = JS String(v ?? '')
fn js_string(v:&Value)->String{ match v{Value::Null=>"null".into(),Value::String(s)=>s.clone(),Value::Number(n)=>n.to_string(),Value::Bool(b)=>b.to_string(),Value::Array(a)=>a.iter().map(js_string).collect::<Vec<_>>().join(","),Value::Object(_)=>"[object Object]".into()} } // = JS String(v)
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand: nz(a).or(nz(b))
fn js_number(v: Option<&Value>) -> f64 { match v { None=>f64::NAN, Some(Value::Null)=>0.0, Some(Value::Bool(b))=>if *b {1.0} else {0.0}, Some(Value::Number(n))=>n.as_f64().unwrap_or(f64::NAN), Some(Value::String(s))=>{let t=s.trim(); if t.is_empty(){0.0}else{t.parse().unwrap_or(f64::NAN)}}, Some(_)=>f64::NAN } } // = JS Number(v)
fn take_chars(s: &str, n: usize) -> String { s.chars().take(n).collect() } // JS .slice(0,n) (BMP-equivalent)

// JS `${n}` for a Number built via Number(...): an integer-valued float renders with no decimal
// point (fixtures keep ids integral; the fractional fallback is Rust's default, unexercised here).
fn num_tmpl(n: f64) -> String { if n.is_finite() && n.fract() == 0.0 { format!("{}", n as i64) } else { n.to_string() } }

// JS `parse = (t, fb) => { try { return JSON.parse(t ?? ''); } catch { return fb; } }`. t is always
// a DB text column (String or SQL NULL); JSON.parse('') throws (empty isn't valid JSON) -> fb.
fn parse(t: Option<&Value>, fb: Value) -> Value {
    let s = match t { Some(Value::String(s)) => s.clone(), _ => String::new() };
    serde_json::from_str(&s).unwrap_or(fb)
}

fn dash_re() -> &'static regex::Regex { static R: OnceLock<regex::Regex> = OnceLock::new(); R.get_or_init(|| regex::Regex::new(r"[^a-z0-9]+").unwrap()) }

// PAPER-ONLY enforcement (operator 2026-07-20): the artifact skills resolve their ONLY legal source
// here — a SAVED paper by slug or id. `id`/`slug` truthiness mirrors JS falsiness (0/NaN/'' all
// count as absent), and the not-found error's `${slug ?? id}` picks slug whenever it's a string at
// all (even ''), never falling to id unless slug is genuinely JS `null` (paper_slug was absent/null).
pub fn resolvePaperSource(db: &Connection, input: &Value) -> Value {
    crate::constitution__helpers_schema::ensureConstitutionSchema(db);

    let id_present = input.get("paper_id").map_or(false, |v| !v.is_null());
    let id_num = if id_present { Some(js_number(input.get("paper_id"))) } else { None };
    let id_truthy = id_num.map_or(false, |n| n != 0.0 && !n.is_nan());

    let slug_present = input.get("paper_slug").map_or(false, |v| !v.is_null());
    let slug_str: Option<String> = if slug_present { Some(js_string(input.get("paper_slug").unwrap()).trim().to_string()) } else { None };
    let slug_truthy = slug_str.as_deref().map_or(false, |s| !s.is_empty());

    if !id_truthy && !slug_truthy {
        return json!({ "ok": false, "error": "a saved paper is required (paper_slug or paper_id) — this artifact projects a paper: pick one on the shelf or create one via the Paper wizard" });
    }

    let row = if id_truthy {
        crate::db_json::query_rows(db, "SELECT * FROM judgments.review_papers WHERE id = ?", &[json!(id_num.unwrap())])
    } else {
        crate::db_json::query_rows(db, "SELECT * FROM judgments.review_papers WHERE slug = ?", &[json!(slug_str.clone().unwrap())])
    }.ok().and_then(|r| r.as_array().and_then(|a| a.first().cloned()));

    let row = match row {
        Some(r) => r,
        None => {
            let display = match &slug_str { Some(s) => s.clone(), None => num_tmpl(id_num.unwrap()) };
            return json!({ "ok": false, "error": format!("no such paper ({display})") });
        }
    };

    if !truthy(row.get("markdown_md")) {
        return json!({ "ok": false, "error": "this paper carries no markdown — reopen it once to save one" });
    }

    json!({
        "ok": true,
        "id": row.get("id").cloned().unwrap_or(Value::Null),
        "slug": row.get("slug").cloned().unwrap_or(Value::Null),
        "title": row.get("title").cloned().unwrap_or(Value::Null),
        "swarm_id": row.get("swarm_id").cloned().unwrap_or(Value::Null),
        "markdown": row.get("markdown_md").cloned().unwrap_or(Value::Null),
    })
}

pub fn constitutionPaper(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const db = context?.db; if (!db) throw err.required(...)` — this harness's db lane always
    // supplies context.db, so that guard never fires here (kept only as a doc note, not code).
    crate::constitution__helpers_schema::ensureConstitutionSchema(db);
    let op = nz(input.get("op")).map(js_string).unwrap_or_else(|| "list".into());

    if op == "save" {
        let title = str_qq(input.get("title")).trim().to_string();
        if title.is_empty() { return Err("constitution-paper: title is required".into()); }
        if input.get("paper").map_or(true, |v| v.is_null()) { return Err("constitution-paper: paper is required".into()); }
        let paper_v = input.get("paper").unwrap();
        let paper_json_str = serde_json::to_string(paper_v).unwrap_or_else(|_| "null".into());

        let swarm_id: Option<String> = if input.get("swarm_id").map_or(false, |v| !v.is_null()) {
            Some(take_chars(&js_string(input.get("swarm_id").unwrap()), 80))
        } else { None };

        let fallback = {
            let lower = title.to_lowercase();
            let dashed = dash_re().replace_all(&lower, "-").to_string();
            format!("paper-{}-{}", take_chars(&dashed, 40), paper_json_str.chars().count())
        };
        let slug_source: Value = if input.get("slug").map_or(false, |v| !v.is_null()) {
            input.get("slug").cloned().unwrap()
        } else if let Some(s) = &swarm_id {
            json!(s)
        } else {
            json!(fallback)
        };
        let slug = take_chars(&js_string(&slug_source), 100);

        let snap = crate::db_json::query_rows(db, "SELECT id FROM judgments.review_snapshots WHERE status='done' ORDER BY id DESC LIMIT 1", &[])?;
        let snap_id = snap.as_array().and_then(|a| a.first()).and_then(|r| r.get("id")).cloned().unwrap_or(Value::Null);

        let label = if input.get("label").map_or(false, |v| !v.is_null()) { json!(take_chars(&js_string(input.get("label").unwrap()), 200)) } else { Value::Null };
        let discovery = if input.get("discovery").map_or(false, |v| !v.is_null()) { json!(take_chars(&js_string(input.get("discovery").unwrap()), 400)) } else { Value::Null };
        let markdown = if input.get("markdown").map_or(false, |v| !v.is_null()) { json!(js_string(input.get("markdown").unwrap())) } else { Value::Null };
        let cost_usd = nz(input.get("cost_usd")).map(|v| js_number(Some(v))).unwrap_or(0.0);
        let files_v = nz(input.get("files")).cloned().unwrap_or(json!([]));
        let agents_v = nz(input.get("agents")).cloned().unwrap_or(json!([]));
        let files_json = serde_json::to_string(&files_v).unwrap_or_else(|_| "[]".into());
        let agents_json = serde_json::to_string(&agents_v).unwrap_or_else(|_| "[]".into());

        // ON CONFLICT deliberately omits swarm_id/snapshot_id from SET — a re-save never overwrites
        // either (only a fresh insert's VALUES() sets them), and markdown_md COALESCEs so an
        // update without a markdown payload keeps whatever was saved before.
        crate::db_json::run_write(db, &format!("
            INSERT INTO judgments.review_papers (slug, title, label, discovery, swarm_id, snapshot_id, files_json, agents_json, paper_json, markdown_md, cost_usd)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(slug) DO UPDATE SET
              title = excluded.title, label = excluded.label, discovery = excluded.discovery,
              files_json = excluded.files_json, agents_json = excluded.agents_json,
              paper_json = excluded.paper_json, markdown_md = COALESCE(excluded.markdown_md, review_papers.markdown_md),
              cost_usd = excluded.cost_usd, updated_at = {}
        ", crate::clock::now_sql()), &[
            json!(slug.clone()), json!(take_chars(&title, 200)), label, discovery,
            swarm_id.map(|s| json!(s)).unwrap_or(Value::Null), snap_id,
            json!(files_json), json!(agents_json), json!(paper_json_str), markdown, json!(cost_usd),
        ])?;

        let row = crate::db_json::query_rows(db, "SELECT id, slug FROM judgments.review_papers WHERE slug = ?", &[json!(slug.clone())])?;
        let id_out = row.as_array().and_then(|a| a.first()).and_then(|r| r.get("id")).cloned().unwrap_or(Value::Null);
        return Ok(json!({ "ok": true, "id": id_out, "slug": slug }));
    }

    if op == "get" {
        let id_present = input.get("id").map_or(false, |v| !v.is_null());
        let rows = if id_present {
            crate::db_json::query_rows(db, "SELECT * FROM judgments.review_papers WHERE id = ?", &[json!(js_number(input.get("id")))])?
        } else {
            let slug = nz(input.get("slug")).map(js_string).unwrap_or_default();
            crate::db_json::query_rows(db, "SELECT * FROM judgments.review_papers WHERE slug = ?", &[json!(slug)])?
        };
        let row = match rows.as_array().and_then(|a| a.first().cloned()) {
            Some(r) => r,
            None => return Ok(json!({ "ok": false, "error": "no such paper" })),
        };
        let get = |k: &str| row.get(k).cloned().unwrap_or(Value::Null);
        let paper = json!({
            "id": get("id"), "slug": get("slug"), "title": get("title"), "label": get("label"), "discovery": get("discovery"),
            "swarm_id": get("swarm_id"), "snapshot_id": get("snapshot_id"),
            "files": parse(row.get("files_json"), json!([])), "agents": parse(row.get("agents_json"), json!([])),
            "session": parse(row.get("paper_json"), Value::Null), "markdown": get("markdown_md"),
            "cost_usd": get("cost_usd"), "created_at": get("created_at"), "updated_at": get("updated_at"),
        });
        return Ok(json!({ "ok": true, "paper": paper }));
    }

    let limit_raw = nz(input.get("limit")).map(|v| js_number(Some(v))).unwrap_or(40.0);
    let limit = limit_raw.min(100.0).max(1.0);
    let rows = crate::db_json::query_rows(db, "
        SELECT id, slug, title, label, discovery, swarm_id, snapshot_id, files_json, agents_json, cost_usd, created_at, updated_at
        FROM judgments.review_papers ORDER BY id DESC LIMIT ?
    ", &[json!(limit as i64)])?;
    let papers: Vec<Value> = rows.as_array().cloned().unwrap_or_default().into_iter().map(|r| {
        let get = |k: &str| r.get(k).cloned().unwrap_or(Value::Null);
        let files_len = parse(r.get("files_json"), json!([])).as_array().map(|a| a.len() as i64).unwrap_or(0);
        let agents_len = parse(r.get("agents_json"), json!([])).as_array().map(|a| a.len() as i64).unwrap_or(0);
        json!({
            "id": get("id"), "slug": get("slug"), "title": get("title"), "label": get("label"), "discovery": get("discovery"),
            "swarm_id": get("swarm_id"), "snapshot_id": get("snapshot_id"),
            "files": files_len, "agents": agents_len,
            "cost_usd": get("cost_usd"), "created_at": get("created_at"), "updated_at": get("updated_at"),
        })
    }).collect();
    Ok(json!({ "ok": true, "papers": papers }))
}

#[cfg(test)]
mod tests {
    use super::*;

    // review_papers carries FROZEN literal defaults ('2026-01-01 00:00:00', not datetime('now'))
    // so a fresh INSERT that omits created_at/updated_at (op=save always does) stays deterministic
    // — mirrors the notebook-store precedent (self-heals via the ensure-twin; pre-creating with a
    // static default means the ensure-twin's own CREATE IF NOT EXISTS is a harmless no-op here).
    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS laws; ATTACH ':memory:' AS judgments;
          CREATE TABLE judgments.review_papers (id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT UNIQUE, title TEXT NOT NULL, label TEXT, discovery TEXT, swarm_id TEXT, snapshot_id INTEGER, files_json TEXT NOT NULL DEFAULT '[]', agents_json TEXT NOT NULL DEFAULT '[]', paper_json TEXT NOT NULL, markdown_md TEXT, cost_usd REAL NOT NULL DEFAULT 0, created_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00', updated_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00');
          CREATE TABLE judgments.review_snapshots (id INTEGER PRIMARY KEY AUTOINCREMENT, status TEXT);").unwrap();
        c
    }

    #[test]
    fn resolve_paper_source_branches() {
        let c = db();
        assert_eq!(resolvePaperSource(&c, &json!({})), json!({ "ok": false, "error": "a saved paper is required (paper_slug or paper_id) — this artifact projects a paper: pick one on the shelf or create one via the Paper wizard" }));

        c.execute("INSERT INTO judgments.review_papers (id, slug, title, paper_json, markdown_md) VALUES (1, 'paper-one', 'Paper One', '{}', '# md')", []).unwrap();
        let r = resolvePaperSource(&c, &json!({ "paper_id": 1 }));
        assert_eq!(r, json!({ "ok": true, "id": 1, "slug": "paper-one", "title": "Paper One", "swarm_id": Value::Null, "markdown": "# md" }));
        assert_eq!(resolvePaperSource(&c, &json!({ "paper_slug": "paper-one" }))["ok"], json!(true));
        assert_eq!(resolvePaperSource(&c, &json!({ "paper_id": 0, "paper_slug": "paper-one" }))["ok"], json!(true)); // id=0 falsy → routes via slug

        assert_eq!(resolvePaperSource(&c, &json!({ "paper_id": 999 })), json!({ "ok": false, "error": "no such paper (999)" }));
        assert_eq!(resolvePaperSource(&c, &json!({ "paper_slug": "nope" })), json!({ "ok": false, "error": "no such paper (nope)" }));

        c.execute("INSERT INTO judgments.review_papers (id, slug, title, paper_json, markdown_md) VALUES (2, 'no-md', 'No Md', '{}', NULL)", []).unwrap();
        assert_eq!(resolvePaperSource(&c, &json!({ "paper_id": 2 })), json!({ "ok": false, "error": "this paper carries no markdown — reopen it once to save one" }));
    }

    #[test]
    fn save_fresh_derives_slug_and_defaults() {
        let c = db();
        let r = constitutionPaper(&json!({ "op": "save", "title": "My Paper", "paper": { "a": 1 } }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["slug"], json!("paper-my-paper-7")); // '{"a":1}'.length == 7
        let row = crate::db_json::query_rows(&c, "SELECT * FROM judgments.review_papers WHERE slug = 'paper-my-paper-7'", &[]).unwrap();
        let row = &row[0];
        assert_eq!(row["files_json"], json!("[]"));
        assert_eq!(row["cost_usd"], json!(0.0));
        assert_eq!(row["markdown_md"], Value::Null);
        assert_eq!(row["created_at"], json!("2026-01-01 00:00:00")); // table default (frozen)
    }

    #[test]
    fn save_conflict_update_leaves_swarm_and_snapshot_untouched() {
        let c = db();
        c.execute("INSERT INTO judgments.review_papers (id, slug, title, label, discovery, swarm_id, snapshot_id, files_json, agents_json, paper_json, markdown_md, cost_usd, created_at, updated_at) VALUES (5, 'existing-slug', 'Old Title', 'OldLabel', 'OldDisc', 'old-swarm', NULL, '[]', '[]', '{}', 'old md', 2, '2025-01-01 00:00:00', '2025-01-01 00:00:00')", []).unwrap();
        let r = constitutionPaper(&json!({
            "op": "save", "slug": "existing-slug", "title": "New Title", "label": "NewLabel", "discovery": "NewDisc",
            "swarm_id": "new-swarm-id", "files": ["a.ts"], "agents": ["agent1"], "paper": { "b": 2 }, "markdown": "new md", "cost_usd": 9.5,
        }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "id": 5, "slug": "existing-slug" }));
        let row = crate::db_json::query_rows(&c, "SELECT * FROM judgments.review_papers WHERE id = 5", &[]).unwrap();
        let row = &row[0];
        assert_eq!(row["title"], json!("New Title"));
        assert_eq!(row["swarm_id"], json!("old-swarm")); // untouched by ON CONFLICT
        assert_eq!(row["snapshot_id"], Value::Null); // untouched
        assert_eq!(row["files_json"], json!("[\"a.ts\"]"));
        assert_eq!(row["markdown_md"], json!("new md"));
        assert_eq!(row["updated_at"], json!("2026-01-01 00:00:00")); // frozen literal
        assert_eq!(row["created_at"], json!("2025-01-01 00:00:00")); // not in SET clause

        // markdown omitted on a second save → COALESCE keeps the prior value
        constitutionPaper(&json!({ "op": "save", "slug": "existing-slug", "title": "T3", "paper": {} }), &c).unwrap();
        let row = crate::db_json::query_rows(&c, "SELECT markdown_md FROM judgments.review_papers WHERE id = 5", &[]).unwrap();
        assert_eq!(row[0]["markdown_md"], json!("new md"));
    }

    #[test]
    fn save_picks_latest_done_snapshot() {
        let c = db();
        c.execute_batch("INSERT INTO judgments.review_snapshots (id, status) VALUES (2,'done'),(8,'done'),(5,'running');").unwrap();
        constitutionPaper(&json!({ "op": "save", "title": "Snap Paper", "paper": { "x": true } }), &c).unwrap();
        let row = crate::db_json::query_rows(&c, "SELECT snapshot_id FROM judgments.review_papers", &[]).unwrap();
        assert_eq!(row[0]["snapshot_id"], json!(8));
    }

    #[test]
    fn get_by_id_and_by_slug_and_not_found() {
        let c = db();
        c.execute("INSERT INTO judgments.review_papers (id, slug, title, label, discovery, swarm_id, snapshot_id, files_json, agents_json, paper_json, markdown_md, cost_usd, created_at, updated_at) VALUES (11, 'get-slug', 'Get Title', 'L', 'D', 'SW', 42, '[\"f1\",\"f2\"]', '[\"ag1\"]', '{\"k\":\"v\"}', '# md', 3.25, '2026-02-01 00:00:00', '2026-02-02 00:00:00')", []).unwrap();
        let r = constitutionPaper(&json!({ "op": "get", "id": 11 }), &c).unwrap();
        assert_eq!(r["paper"]["files"], json!(["f1", "f2"]));
        assert_eq!(r["paper"]["session"], json!({ "k": "v" }));
        assert_eq!(r["paper"]["cost_usd"], json!(3.25));
        let keys: Vec<&str> = r["paper"].as_object().unwrap().keys().map(|k| k.as_str()).collect();
        assert_eq!(keys, vec!["id", "slug", "title", "label", "discovery", "swarm_id", "snapshot_id", "files", "agents", "session", "markdown", "cost_usd", "created_at", "updated_at"]);

        let r2 = constitutionPaper(&json!({ "op": "get", "slug": "get-slug" }), &c).unwrap();
        assert_eq!(r2["paper"]["id"], json!(11));

        assert_eq!(constitutionPaper(&json!({ "op": "get", "slug": "nope" }), &c).unwrap(), json!({ "ok": false, "error": "no such paper" }));
    }

    #[test]
    fn get_malformed_files_json_falls_back_to_empty_array() {
        let c = db();
        c.execute("INSERT INTO judgments.review_papers (id, slug, title, files_json, agents_json, paper_json, markdown_md) VALUES (3, 'bad-json', 'Bad', 'not json', '[]', '{}', NULL)", []).unwrap();
        let r = constitutionPaper(&json!({ "op": "get", "id": 3 }), &c).unwrap();
        assert_eq!(r["paper"]["files"], json!([]));
        assert_eq!(r["paper"]["agents"], json!([]));
        assert_eq!(r["paper"]["markdown"], Value::Null);
    }

    #[test]
    fn list_orders_desc_counts_files_and_clamps_limit() {
        let c = db();
        c.execute_batch("
            INSERT INTO judgments.review_papers (id, slug, title, files_json, agents_json, paper_json, cost_usd) VALUES
              (1, 's1', 'T1', '[]', '[]', '{}', 0),
              (2, 's2', 'T2', '[\"a\",\"b\"]', '[\"x\"]', '{}', 1),
              (3, 's3', 'T3', '[]', '[]', '{}', 2);
        ").unwrap();
        let r = constitutionPaper(&json!({ "op": "list" }), &c).unwrap();
        let ids: Vec<i64> = r["papers"].as_array().unwrap().iter().map(|p| p["id"].as_i64().unwrap()).collect();
        assert_eq!(ids, vec![3, 2, 1]); // DESC
        assert_eq!(r["papers"][1]["files"], json!(2));

        // op absent → defaults to list
        let r2 = constitutionPaper(&json!({}), &c).unwrap();
        assert_eq!(r2["papers"].as_array().unwrap().len(), 3);

        // limit 0 clamps to 1
        let r3 = constitutionPaper(&json!({ "op": "list", "limit": 0 }), &c).unwrap();
        assert_eq!(r3["papers"].as_array().unwrap().len(), 1);
        assert_eq!(r3["papers"][0]["id"], json!(3));
    }
}

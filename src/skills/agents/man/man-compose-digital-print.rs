//! man-compose-digital-print.rs — twin of skills/agents/man/man-compose-digital-print.mjs (db WRITE
//! lane, attached `memory` + `brain` dbs). NAMES ARE CONTRACT: `manComposeDigitalPrint` (default).
//! Composes the post-QA "digital print" for a done contract: a rich git-commit subject/body
//! (stashed on the contract row for the next git-commit step), a per-agent AI cost breakdown from
//! brain.ai_calls joined through sacred_timeline, and three best-effort memory.memories entries
//! (tl/dev/qa contributions).
// QUALITY: cross-db cost rollup (COALESCE/SUBSTR/INSTR agent-name split) + best-effort sub-calls
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
// JS `${v}` interpolation (string/number/bool/null/absent).
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }
fn take_chars(s: &str, n: usize) -> String { s.chars().take(n).collect() } // JS .slice(0,n) (BMP)
fn to_fixed4(x: f64) -> String { format!("{:.4}", x) } // JS Number.prototype.toFixed(4)
// JS JSON-number rendering: a whole-valued float collapses to an integer (JSON.stringify(5.0) is "5").
fn js_num(n: f64) -> Value { if n.is_finite() && n.fract() == 0.0 && n.abs() < 9e15 { json!(n as i64) } else { json!(n) } }

// `(() => { try { return JSON.parse(x); } catch { return default; } })()`. Playbook gotcha #6:
// JSON.parse(null) coerces to the string "null" and returns JS `null` WITHOUT throwing — so an
// absent/SQL-NULL column yields Value::Null, NOT `default`. A String is parsed or falls back.
fn json_parse_or(v: Option<&Value>, default: Value) -> Value {
    match v {
        None | Some(Value::Null) => Value::Null,
        Some(Value::String(s)) => serde_json::from_str::<Value>(s).unwrap_or(default),
        Some(other) => other.clone(),
    }
}

pub fn manComposeDigitalPrint(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS throws err.contextMissing('man-compose-digital-print','db') when no db — the db lane
    // always injects one, so that guard is unreachable here.
    let contract_slug = input.get("contract_slug").cloned().unwrap_or(Value::Null);
    if !truthy(Some(&contract_slug)) {
        return Err("man-compose-digital-print: contract_slug is required".into());
    }

    let rows = crate::db_json::query_rows(
        db,
        "SELECT slug, card_slug, title, status, briefing_md, tl_briefing_md,\n           microtasks_json, files_touched_json, current_attempt\n    FROM contracts WHERE slug = ?",
        &[contract_slug.clone()],
    )?;
    let contract = match rows.as_array().and_then(|a| a.first()) {
        Some(c) => c.clone(),
        None => return Err(format!("man-compose-digital-print: contract '{}' not found", tmpl(Some(&contract_slug)))),
    };
    let status = contract.get("status").and_then(|v| v.as_str()).unwrap_or("");
    if status != "done" {
        return Err(format!(
            "man-compose-digital-print: contract '{}' status='{}', expected 'done' (post-QA pipeline only)",
            tmpl(Some(&contract_slug)),
            status
        ));
    }

    // Fetched but never used by the JS source either (dead read) — transcribed for fidelity: a
    // missing `cards` table would throw there too (not wrapped in try/catch), so we propagate the
    // same way via `?` instead of silently ignoring the Result.
    let _card = crate::db_json::query_rows(
        db,
        "SELECT slug, title, body_json FROM cards WHERE slug = ?",
        &[contract.get("card_slug").cloned().unwrap_or(Value::Null)],
    )?;

    let microtasks = json_parse_or(contract.get("microtasks_json"), json!([]));
    let files_touched = json_parse_or(contract.get("files_touched_json"), json!([]));
    // Both columns are NOT NULL DEFAULT '[]' in production — .as_array() always succeeds there.
    // A stray non-array shape degrades to length 0 rather than mirroring JS's crash-on-null
    // `.length` read; unreachable in practice, and per playbook rule 9 guard-throws / crash paths
    // on malformed data are not exercised.
    let microtasks_arr = microtasks.as_array().cloned().unwrap_or_default();
    let files_touched_arr = files_touched.as_array().cloned().unwrap_or_default();

    // Per-agent AI cost breakdown from brain.ai_calls, joined via sacred_timeline linkage. Any
    // query failure (older contracts whose parent_event_id doesn't link this way) is swallowed —
    // its output is identical to the "zero matching rows" case, so both collapse to costs=0.
    let mut pl = 0.0f64;
    let mut tl = 0.0f64;
    let mut dev = 0.0f64;
    let mut qa = 0.0f64;
    let mut man = 0.0f64;
    let mut total = 0.0f64;
    if let Ok(cost_rows) = crate::db_json::query_rows(
        db,
        "SELECT COALESCE(SUBSTR(p.name, 1, INSTR(p.name, '.') - 1), 'system') AS agent,\n             SUM(a.cost_usd_actual) AS cost\n      FROM brain.ai_calls a\n      LEFT JOIN brain.prompt_templates p ON p.id = a.prompt_template_id\n      WHERE a.parent_event_id IN (\n        SELECT id FROM sacred_timeline WHERE entity_type='contract' AND entity_id = ?\n      )\n      GROUP BY agent",
        &[contract.get("slug").cloned().unwrap_or(Value::Null)],
    ) {
        for r in cost_rows.as_array().cloned().unwrap_or_default() {
            let agent = r.get("agent").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let c = r.get("cost").and_then(|v| v.as_f64()).unwrap_or(0.0); // Number(cost ?? 0)
            // Faithful to `if (costs[r.agent] != null) costs[r.agent] = c;` — SETs (not adds) only
            // for a recognized key (the object's own 'total' key included, however improbable).
            match agent.as_str() {
                "pl" => pl = c,
                "tl" => tl = c,
                "dev" => dev = c,
                "qa" => qa = c,
                "man" => man = c,
                "total" => total = c,
                _ => {}
            }
            total += c; // `costs.total += c;` — unconditional, every row.
        }
    }

    let card_slug_lower = match contract.get("card_slug") {
        Some(Value::String(s)) => s.to_lowercase(),
        _ => "card".to_string(),
    };
    let title = nz(contract.get("title")).or_else(|| nz(contract.get("slug"))).cloned().unwrap_or(Value::Null);
    let subject = format!("feat({}): {}", card_slug_lower, take_chars(&tmpl(Some(&title)), 60));

    let briefing_line = if truthy(contract.get("briefing_md")) {
        contract.get("briefing_md").and_then(|v| v.as_str()).unwrap_or("").split('\n').next().unwrap_or("").to_string()
    } else {
        "(no briefing)".to_string()
    };
    let current_attempt = nz(contract.get("current_attempt")).cloned().unwrap_or(json!(1));
    let files_line = if !files_touched_arr.is_empty() {
        // Array.prototype.join(', ') — elements here are always plain path strings in production.
        let head: Vec<String> = files_touched_arr.iter().take(10).map(|v| tmpl(Some(v))).collect();
        let suffix = if files_touched_arr.len() > 10 { format!(" (+{})", files_touched_arr.len() - 10) } else { String::new() };
        format!("Files: {}{}", head.join(", "), suffix)
    } else {
        String::new()
    };
    let microtasks_line = if !microtasks_arr.is_empty() { format!("Microtasks: {} executed", microtasks_arr.len()) } else { String::new() };
    let cost_line = format!(
        "AI cost breakdown: pl=${} tl=${} dev=${} qa=${} man=${} total=${}",
        to_fixed4(pl),
        to_fixed4(tl),
        to_fixed4(dev),
        to_fixed4(qa),
        to_fixed4(man),
        to_fixed4(total)
    );

    // `.filter(Boolean)` — only the empty-string entries (the unconditional '' spacer line, plus
    // Files/Microtasks when their source array is empty) ever get dropped; every other line is
    // guaranteed non-empty.
    let body_lines: Vec<String> = [
        briefing_line,
        String::new(),
        format!("Contract: {} (attempt {})", tmpl(contract.get("slug")), tmpl(Some(&current_attempt))),
        format!("Card: {}", tmpl(contract.get("card_slug"))),
        files_line,
        microtasks_line,
        cost_line,
    ]
    .into_iter()
    .filter(|l| !l.is_empty())
    .collect();
    let body = body_lines.join("\n");

    // Per-agent memory entries — best-effort: a failed memoryAdd is simply not appended (matches
    // the JS try/catch wrapping each call).
    let mut memory_entry_ids: Vec<Value> = Vec::new();
    let tl_briefing_len = contract.get("tl_briefing_md").and_then(|v| v.as_str()).map(|s| s.chars().count()).unwrap_or(0);
    let slug_tmpl = tmpl(contract.get("slug"));
    let card_slug_tmpl = tmpl(contract.get("card_slug"));
    for (agent, summary) in [
        ("tl", format!("TL signed contract {} with senior briefing ({} chars).", slug_tmpl, tl_briefing_len)),
        ("dev", format!("DEV executed {} microtask(s) across {} file(s).", microtasks_arr.len(), files_touched_arr.len())),
        ("qa", format!("QA passed contract {} (deterministic + senior review).", slug_tmpl)),
    ] {
        let mem_input = json!({
            "type": "lesson",
            "title": format!("{}-contribution: {}", agent, slug_tmpl),
            "situation": format!("Contract {} on card {}", slug_tmpl, card_slug_tmpl),
            "guidance": summary,
            "tags": [format!("agent:{}", agent), "digital-print", format!("contract:{}", slug_tmpl), format!("card:{}", card_slug_tmpl)],
            "confidence": 0.85,
            "source_card": contract.get("card_slug").cloned().unwrap_or(Value::Null),
            "created_by": format!("agent:{}", agent),
        });
        if let Ok(mem) = crate::entities_memory_add::memoryAdd(&mem_input, db) {
            let mut obj = Map::new();
            obj.insert("agent".into(), json!(agent));
            obj.insert("id".into(), mem.get("id").cloned().unwrap_or(Value::Null));
            memory_entry_ids.push(Value::Object(obj));
        }
    }

    // Stash the composed commit message for the next git-commit step. Columns may not exist on a
    // legacy DB — swallow ONLY "no such column"; any other write failure propagates.
    match crate::db_json::run_write(
        db,
        &format!("UPDATE contracts\n      SET digital_print_subject = ?, digital_print_body = ?, updated_at = {}\n      WHERE slug = ?", crate::clock::now_sql()),
        &[json!(subject.clone()), json!(body.clone()), contract.get("slug").cloned().unwrap_or(Value::Null)],
    ) {
        Ok(_) => {}
        Err(e) => {
            if !e.contains("no such column") {
                return Err(e);
            }
        }
    }

    let mut cost_breakdown = Map::new();
    cost_breakdown.insert("pl".into(), js_num(pl));
    cost_breakdown.insert("tl".into(), js_num(tl));
    cost_breakdown.insert("dev".into(), js_num(dev));
    cost_breakdown.insert("qa".into(), js_num(qa));
    cost_breakdown.insert("man".into(), js_num(man));
    cost_breakdown.insert("total".into(), js_num(total));

    let mut out = Map::new();
    out.insert("contract_slug".into(), contract_slug);
    out.insert("git_commit_subject".into(), json!(subject));
    out.insert("git_commit_body".into(), json!(body));
    out.insert("memory_entry_ids".into(), json!(memory_entry_ids));
    out.insert("cost_breakdown".into(), Value::Object(cost_breakdown));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMA: &str = "
        CREATE TABLE contracts (
          slug TEXT PRIMARY KEY, card_slug TEXT, title TEXT, status TEXT,
          briefing_md TEXT, tl_briefing_md TEXT,
          microtasks_json TEXT NOT NULL DEFAULT '[]', files_touched_json TEXT NOT NULL DEFAULT '[]',
          current_attempt INTEGER NOT NULL DEFAULT 1,
          digital_print_subject TEXT, digital_print_body TEXT, updated_at TEXT
        );
        CREATE TABLE cards (slug TEXT PRIMARY KEY, title TEXT, body_json TEXT);
        CREATE TABLE sacred_timeline (id INTEGER PRIMARY KEY, entity_type TEXT, entity_id TEXT);
        CREATE TABLE memory.memories (
          id              INTEGER PRIMARY KEY AUTOINCREMENT,
          type            TEXT    NOT NULL,
          title           TEXT    NOT NULL,
          situation       TEXT    NOT NULL,
          guidance        TEXT    NOT NULL,
          naive_response  TEXT,
          rationale       TEXT,
          tags_json       TEXT    NOT NULL DEFAULT '[]',
          source_card     TEXT,
          source_event    INTEGER,
          source_session  TEXT,
          confidence      REAL    NOT NULL DEFAULT 0.5,
          validated       INTEGER NOT NULL DEFAULT 0,
          superseded_by   INTEGER,
          agent_slug      TEXT,
          created_at      TEXT    NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')),
          created_by      TEXT    NOT NULL
        );
        CREATE TABLE brain.prompt_templates (id INTEGER PRIMARY KEY, name TEXT);
        CREATE TABLE brain.ai_calls (id INTEGER PRIMARY KEY, prompt_template_id INTEGER, cost_usd_actual REAL, parent_event_id INTEGER);
    ";

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS memory; ATTACH ':memory:' AS brain;").unwrap();
        c.execute_batch(SCHEMA).unwrap();
        c
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_contract(
        c: &Connection,
        slug: &str,
        card_slug: Option<&str>,
        title: &str,
        status: &str,
        briefing_md: Option<&str>,
        tl_briefing_md: Option<&str>,
        microtasks_json: &str,
        files_touched_json: &str,
        current_attempt: i64,
    ) {
        c.execute(
            "INSERT INTO contracts (slug, card_slug, title, status, briefing_md, tl_briefing_md, microtasks_json, files_touched_json, current_attempt) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            rusqlite::params![slug, card_slug, title, status, briefing_md, tl_briefing_md, microtasks_json, files_touched_json, current_attempt],
        )
        .unwrap();
    }

    #[test]
    fn requires_contract_slug() {
        let c = db();
        assert!(manComposeDigitalPrint(&json!({}), &c).is_err());
    }

    #[test]
    fn not_found_when_missing() {
        let c = db();
        assert!(manComposeDigitalPrint(&json!({ "contract_slug": "GHOST" }), &c).is_err());
    }

    #[test]
    fn rejects_non_done_status() {
        let c = db();
        insert_contract(&c, "CT-X", Some("CARD-X"), "t", "impl", None, None, "[]", "[]", 1);
        assert!(manComposeDigitalPrint(&json!({ "contract_slug": "CT-X" }), &c).is_err());
    }

    #[test]
    fn minimal_contract_zero_costs_null_card_and_omitted_lines() {
        let c = db();
        insert_contract(&c, "CT-MIN", None, "short title", "done", None, None, "[]", "[]", 1);
        let r = manComposeDigitalPrint(&json!({ "contract_slug": "CT-MIN" }), &c).unwrap();
        assert_eq!(r["contract_slug"], json!("CT-MIN"));
        assert_eq!(r["git_commit_subject"], json!("feat(card): short title"));
        assert_eq!(
            r["git_commit_body"],
            json!("(no briefing)\nContract: CT-MIN (attempt 1)\nCard: null\nAI cost breakdown: pl=$0.0000 tl=$0.0000 dev=$0.0000 qa=$0.0000 man=$0.0000 total=$0.0000")
        );
        assert_eq!(r["cost_breakdown"], json!({ "pl": 0, "tl": 0, "dev": 0, "qa": 0, "man": 0, "total": 0 }));
        assert_eq!(r["memory_entry_ids"].as_array().unwrap().len(), 3);

        let row_subject: String = c.query_row("SELECT digital_print_subject FROM contracts WHERE slug='CT-MIN'", [], |row| row.get(0)).unwrap();
        assert_eq!(row_subject, "feat(card): short title");
        let mem_count: i64 = c.query_row("SELECT COUNT(*) FROM memory.memories", [], |row| row.get(0)).unwrap();
        assert_eq!(mem_count, 3);
    }

    #[test]
    fn full_contract_cost_rollup_file_truncation_and_memory_entries() {
        let c = db();
        let title70 = "0123456789".repeat(7);
        let title60 = "0123456789".repeat(6);
        insert_contract(
            &c,
            "CT-FULL",
            Some("CARD-A"),
            &title70,
            "done",
            Some("First line here\nSecond line ignored"),
            Some("hello"),
            "[{\"id\":1},{\"id\":2},{\"id\":3}]",
            "[\"f1.js\",\"f2.js\",\"f3.js\",\"f4.js\",\"f5.js\",\"f6.js\",\"f7.js\",\"f8.js\",\"f9.js\",\"f10.js\",\"f11.js\",\"f12.js\"]",
            2,
        );
        c.execute_batch(
            "INSERT INTO sacred_timeline (id, entity_type, entity_id) VALUES (1,'contract','CT-FULL'),(2,'contract','CT-OTHER');
             INSERT INTO brain.prompt_templates (id, name) VALUES (1,'pl.plan'),(2,'tl.brief'),(3,'dev.impl'),(4,'qa.review'),(5,'man.compose');
             INSERT INTO brain.ai_calls (id, prompt_template_id, cost_usd_actual, parent_event_id) VALUES
               (1,1,1.5,1),(2,2,0.75,1),(3,3,2.25,1),(4,4,0.5,1),(5,5,0.25,1),(6,999,4.0,1),(7,1,0.25,1),(8,1,99.0,2);",
        )
        .unwrap();

        let r = manComposeDigitalPrint(&json!({ "contract_slug": "CT-FULL" }), &c).unwrap();
        // pl = 1.5 + 0.25 = 1.75 (two ai_calls rows via the 'pl.plan' template); tl/dev/qa/man from
        // one row each; the unmatched prompt_template_id=999 falls into 'system' (excluded from the
        // named buckets but counted in total); the parent_event_id=2 row (99.0) is excluded entirely.
        assert_eq!(r["git_commit_subject"], json!(format!("feat(card-a): {}", title60)));
        let body = r["git_commit_body"].as_str().unwrap();
        assert!(body.starts_with("First line here\n"));
        assert!(body.contains("Contract: CT-FULL (attempt 2)"));
        assert!(body.contains("Card: CARD-A"));
        assert!(body.contains("Files: f1.js, f2.js, f3.js, f4.js, f5.js, f6.js, f7.js, f8.js, f9.js, f10.js (+2)"));
        assert!(body.contains("Microtasks: 3 executed"));
        assert!(body.contains("AI cost breakdown: pl=$1.7500 tl=$0.7500 dev=$2.2500 qa=$0.5000 man=$0.2500 total=$9.5000"));
        assert_eq!(r["cost_breakdown"], json!({ "pl": 1.75, "tl": 0.75, "dev": 2.25, "qa": 0.5, "man": 0.25, "total": 9.5 }));

        let ids = r["memory_entry_ids"].as_array().unwrap();
        assert_eq!(ids.len(), 3);
        assert_eq!(ids.iter().map(|m| m["agent"].as_str().unwrap()).collect::<Vec<_>>(), vec!["tl", "dev", "qa"]);
        assert!(ids.iter().all(|m| m["id"].is_number()));

        let (subj, upd): (String, String) = c
            .query_row("SELECT digital_print_subject, updated_at FROM contracts WHERE slug='CT-FULL'", [], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap();
        assert_eq!(subj, r["git_commit_subject"].as_str().unwrap());
        assert_eq!(upd, "2026-01-01 00:00:00");
    }

    #[test]
    fn boundary_ten_files_no_suffix_and_empty_string_briefing_falls_back() {
        let c = db();
        insert_contract(
            &c,
            "CT-BOUND",
            Some("CARD-B"),
            "boundary case",
            "done",
            Some(""), // falsy — same fallback as NULL
            Some(""), // 0 chars
            "[{\"id\":1}]",
            "[\"a.js\",\"b.js\",\"c.js\",\"d.js\",\"e.js\",\"f.js\",\"g.js\",\"h.js\",\"i.js\",\"j.js\"]",
            5,
        );
        let r = manComposeDigitalPrint(&json!({ "contract_slug": "CT-BOUND" }), &c).unwrap();
        let body = r["git_commit_body"].as_str().unwrap();
        assert!(body.starts_with("(no briefing)\n"));
        assert!(body.contains("Files: a.js, b.js, c.js, d.js, e.js, f.js, g.js, h.js, i.js, j.js"));
        assert!(!body.contains("(+")); // exactly 10 — no truncation suffix
        assert!(body.contains("Microtasks: 1 executed"));
        assert!(body.contains("Contract: CT-BOUND (attempt 5)"));
    }
}

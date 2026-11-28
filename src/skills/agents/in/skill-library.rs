//! skill-library.rs — twin of skills/agents/in/skill-library.mjs (db WRITE+READ lane, cross-db via
//! ATTACH knowledge). NAMES ARE CONTRACT: `skillLibrary` (default). CRUD over
//! knowledge.skill_library — op-based (list/get/add/update/toggle/remove/seed), like
//! constitution.law. `seed` inserts the fixed "ANTILOKI PACK" of 8 skills (idempotent by slug);
//! its body_md text embeds `process.env.ANTILOKI_BE_PORT` — the conform harness never sets that
//! var, so the twin hardcodes the JS default (41931), same convention as with-audit.rs.
use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Value};

// JS String(v ?? '')
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
// JS String(v)
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
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

// slugify: String(s ?? '').toLowerCase().replace(/[^\w]+/g,'-').replace(/^-+|-+$/g,'').slice(0,60)
// JS \w (no /u flag) is ASCII-only [A-Za-z0-9_].
fn slugify(s: Option<&Value>) -> String {
    let lower = str_qq(s).to_lowercase();
    let re = Regex::new(r"[^A-Za-z0-9_]+").unwrap();
    let hyphenated = re.replace_all(&lower, "-").to_string();
    hyphenated.trim_matches('-').chars().take(60).collect()
}

// safe(s, fb): try { return JSON.parse(s ?? 'null') ?? fb } catch { return fb }
fn safe(s: Option<&Value>, fb: Value) -> Value {
    let text = match s {
        None | Some(Value::Null) => "null".to_string(),
        Some(Value::String(x)) => x.clone(),
        Some(other) => other.to_string(),
    };
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Null) => fb,
        Ok(v) => v,
        Err(_) => fb,
    }
}

// row2skill(r): { ...r, assets: safe(r.assets_json, {}), applies: safe(r.applies_json, {}) }
fn row_to_skill(r: &Value) -> Value {
    let mut out = r.as_object().cloned().unwrap_or_default();
    let assets = safe(r.get("assets_json"), json!({}));
    let applies = safe(r.get("applies_json"), json!({}));
    out.insert("assets".into(), assets);
    out.insert("applies".into(), applies);
    Value::Object(out)
}

fn not_found(slug: &str) -> String { format!("skill-library: skill '{}' not found", slug) }

pub fn skillLibrary(input: &Value, db: &Connection) -> Result<Value, String> {
    let op = match nz(input.get("op")) { Some(v) => js_string(v), None => "list".to_string() };

    if op == "list" {
        let rows = crate::db_json::query_rows(db, "SELECT * FROM knowledge.skill_library ORDER BY enabled DESC, name ASC", &[])?;
        let skills: Vec<Value> = rows.as_array().cloned().unwrap_or_default().iter().map(row_to_skill).collect();
        return Ok(json!({ "ok": true, "skills": skills }));
    }

    if op == "get" {
        let slug = str_qq(input.get("slug"));
        let rows = crate::db_json::query_rows(db, "SELECT * FROM knowledge.skill_library WHERE slug = ?", &[json!(slug)])?;
        let r = rows.as_array().and_then(|a| a.first());
        let r = match r { Some(v) => v, None => return Err(not_found(&slug)) };
        return Ok(json!({ "ok": true, "skill": row_to_skill(r) }));
    }

    if op == "add" {
        let name = str_qq(input.get("name")).trim().to_string();
        if name.is_empty() { return Err("skill-library: name is required".into()); }
        let description = str_qq(input.get("description")).trim().to_string();
        if description.is_empty() { return Err("skill-library: description (the WHEN-TO-USE line) is required".into()); }
        let slug_source: Value = nz(input.get("slug")).cloned().unwrap_or(json!(name));
        let slug = slugify(Some(&slug_source));
        if slug.is_empty() { return Err("skill-library: slug is required".into()); }
        let description_v: String = description.chars().take(500).collect();
        let body_md: String = str_qq(input.get("body_md")).chars().take(60000).collect();
        let assets_v = nz(input.get("assets")).cloned().unwrap_or(json!({}));
        let assets_json: String = serde_json::to_string(&assets_v).map_err(|e| e.to_string())?.chars().take(200000).collect();
        let applies_v = nz(input.get("applies")).cloned().unwrap_or(json!({}));
        let applies_json = serde_json::to_string(&applies_v).map_err(|e| e.to_string())?;
        let verify_cmd: Value = if truthy(input.get("verify_cmd")) {
            json!(js_string(input.get("verify_cmd").unwrap()).chars().take(400).collect::<String>())
        } else {
            Value::Null
        };
        const ALLOWED_PROVENANCE: [&str; 4] = ["authored", "minted", "imported", "promoted"];
        let provenance = match input.get("provenance").and_then(|v| v.as_str()) {
            Some(s) if ALLOWED_PROVENANCE.contains(&s) => s.to_string(),
            _ => "authored".to_string(),
        };
        crate::db_json::run_write(
            db,
            "INSERT INTO knowledge.skill_library (slug, name, description, body_md, assets_json, applies_json, verify_cmd, provenance) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            &[json!(slug), json!(name), json!(description_v), json!(body_md), json!(assets_json), json!(applies_json), verify_cmd, json!(provenance)],
        )?;
        let rows = crate::db_json::query_rows(db, "SELECT * FROM knowledge.skill_library WHERE slug = ?", &[json!(slug)])?;
        let r = rows.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null);
        return Ok(json!({ "ok": true, "skill": row_to_skill(&r) }));
    }

    if op == "update" {
        let slug = str_qq(input.get("slug"));
        let existing = crate::db_json::query_rows(db, "SELECT * FROM knowledge.skill_library WHERE slug = ?", &[json!(slug.clone())])?;
        let r = existing.as_array().and_then(|a| a.first()).cloned();
        let r = match r { Some(v) => v, None => return Err(not_found(&slug)) };

        let mut sets: Vec<String> = Vec::new();
        let mut vals: Vec<Value> = Vec::new();
        // JS `input.name !== undefined` — checks KEY PRESENCE (a present-but-null value still
        // counts), which `Map::get().is_some()` mirrors exactly (absent key → None).
        let has = |k: &str| input.get(k).is_some();
        if has("name") {
            sets.push("name = ?".into());
            vals.push(json!(js_string(input.get("name").unwrap()).chars().take(120).collect::<String>()));
        }
        if has("description") {
            sets.push("description = ?".into());
            vals.push(json!(js_string(input.get("description").unwrap()).chars().take(500).collect::<String>()));
        }
        if has("body_md") {
            sets.push("body_md = ?".into());
            vals.push(json!(js_string(input.get("body_md").unwrap()).chars().take(60000).collect::<String>()));
        }
        if has("assets") {
            let v = nz(input.get("assets")).cloned().unwrap_or(json!({}));
            let s: String = serde_json::to_string(&v).map_err(|e| e.to_string())?.chars().take(200000).collect();
            sets.push("assets_json = ?".into());
            vals.push(json!(s));
        }
        if has("applies") {
            let v = nz(input.get("applies")).cloned().unwrap_or(json!({}));
            let s = serde_json::to_string(&v).map_err(|e| e.to_string())?;
            sets.push("applies_json = ?".into());
            vals.push(json!(s));
        }
        if has("verify_cmd") {
            let v = if truthy(input.get("verify_cmd")) {
                json!(js_string(input.get("verify_cmd").unwrap()).chars().take(400).collect::<String>())
            } else {
                Value::Null
            };
            sets.push("verify_cmd = ?".into());
            vals.push(v);
        }
        if sets.is_empty() {
            return Ok(json!({ "ok": true, "skill": row_to_skill(&r) }));
        }
        sets.push("version = version + 1".into());
        sets.push(format!("updated_at = {}", crate::clock::now_sql()));
        let sql = format!("UPDATE knowledge.skill_library SET {} WHERE slug = ?", sets.join(", "));
        let mut params = vals;
        params.push(json!(slug.clone()));
        crate::db_json::run_write(db, &sql, &params)?;
        let after = crate::db_json::query_rows(db, "SELECT * FROM knowledge.skill_library WHERE slug = ?", &[json!(slug)])?;
        let r2 = after.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null);
        return Ok(json!({ "ok": true, "skill": row_to_skill(&r2) }));
    }

    if op == "toggle" {
        let slug = str_qq(input.get("slug"));
        crate::db_json::run_write(
            db,
            &format!("UPDATE knowledge.skill_library SET enabled = 1 - enabled, updated_at = {} WHERE slug = ?", crate::clock::now_sql()),
            &[json!(slug.clone())],
        )?;
        let rows = crate::db_json::query_rows(db, "SELECT * FROM knowledge.skill_library WHERE slug = ?", &[json!(slug.clone())])?;
        let r = rows.as_array().and_then(|a| a.first());
        let r = match r { Some(v) => v, None => return Err(not_found(&slug)) };
        return Ok(json!({ "ok": true, "skill": row_to_skill(r) }));
    }

    if op == "remove" {
        let slug = str_qq(input.get("slug"));
        crate::db_json::run_write(db, "DELETE FROM knowledge.skill_library WHERE slug = ?", &[json!(slug.clone())])?;
        return Ok(json!({ "ok": true, "slug": slug }));
    }

    if op == "seed" {
        // the ANTILOKI PACK (operator 2026-07-25) — verbatim from the JS PACK array, with
        // ${BE}/${H} resolved (BE = http://127.0.0.1: + (env ANTILOKI_BE_PORT ?? 41931)).
        const BE: &str = "http://127.0.0.1:41931";
        const H: &str = "-H \"Content-Type: application/json\"";
        let pack: Vec<(&str, &str, &str, &str, String)> = vec![
            ("substrate intel", "antiloki-file-review", "Deterministic file review",
             "BEFORE editing or refactoring any file — pull its court-grade review (health, axes, findings) instead of guessing.",
             format!("Run:\n```\ncurl -s {BE}/api/v1/codebase/describe/get {H} -d '{{\"path\":\"<FILE>\"}}'\n```\nRead: health (<50 = fragile — smallest possible diffs), violations[] (fix or avoid worsening each law_key you touch), fan_in (high = your change ripples; check dependents first).")),
            ("substrate intel", "antiloki-xray-first", "X-ray before planning",
             "WHEN starting any multi-file task — get the codebase x-ray (riskiest files, smells, health) to plan around danger.",
             format!("Run:\n```\ncurl -s {BE}/api/v1/codebase/report/latest {H} -d '{{}}'\n```\nRead: top_risk[] = the files where a change hurts most (uncovered + high fan_in = add a test FIRST); smells[] = pre-existing debt — don't add to a kind that's already listed.")),
            ("substrate intel", "antiloki-blast-radius", "Blast radius before changing a surface",
             "BEFORE renaming/changing any exported symbol or shared file — measure who depends on it.",
             format!("Run:\n```\ncurl -s {BE}/api/v1/codebase/blast {H} -d '{{\"node_ids\":[\"<FILE>\"]}}'\n```\nEvery file returned must compile after your change. >5 dependents → prefer an additive change (new export, deprecate old) over an in-place break.")),
            ("substrate intel", "antiloki-constitution-check", "Obey the constitution",
             "WHEN unsure if a pattern is allowed here (imports, layering, style) — the repo has LAWS; check them, never guess.",
             format!("Run:\n```\ncurl -s {BE}/api/v1/dispatch {H} -d '{{\"action_name\":\"system.constitution.report\",\"input\":{{}}}}'\n```\nby_law[] = what this repo already breaks most (don't add instances); the AGENTS.md in this worktree carries the binding rules. A law you'd violate knowingly → stop and say so instead.")),
            ("substrate intel", "antiloki-docs-lookup", "Self-generated docs lookup",
             "WHEN you need to understand a folder/file you have not read — pull its generated docs page before opening 20 files.",
             format!("Run:\n```\ncurl -s {BE}/api/v1/codebase/describe {H} -d '{{\"path\":\"<FOLDER-OR-FILE>\"}}'\n```\nThe docs are rebuilt from the code (no drift). Use them to pick the 2-3 files actually worth reading.")),
            ("discipline", "antiloki-scoped-diffs", "Surgical scoped diffs",
             "ALWAYS when editing in an antiloki pane — your writes are scope-whitelisted and merge-gated.",
             "Rules:\n1. Check AGENTS.md → SCOPE. Edits outside it are BLOCKED at integration — don't waste turns.\n2. Every changed line traces to the task (SURGICAL_EDITS law). No drive-by refactors.\n3. Minimum viable change (MINIMUM_VIABLE_CHANGE law): if 200 lines could be 50, write 50.\n4. Before finishing: git diff --stat — anything you can't justify, revert.".to_string()),
            ("discipline", "antiloki-verify-claims", "Claims need verification commands",
             "WHEN reporting work done — every \"I built X\" must ship with a command that proves it (house law).",
             "CLAIM_REQUIRES_VERIFICATION is a law here. End your report with:\n- the exact command(s) to verify (build/test/curl) and their expected output\n- what you did NOT verify, plainly.\nRun the verification yourself first when possible; paste real output, never predicted output.".to_string()),
            ("discipline", "antiloki-test-after-task", "Test after each macro task",
             "AFTER completing any coherent unit of work — run the relevant tests before declaring it done.",
             "TESTS_AFTER_MACRO_TASK is a law here. Run the narrowest relevant suite first (fast feedback), then the full suite for substrate/engine changes. A red test = fix forward now, not \"known issue\". Include the pass/fail counts in your report.".to_string()),
        ];
        let total = pack.len() as i64;
        let mut seeded: i64 = 0;
        for (cat, slug, name, description, body_md) in &pack {
            let exists = crate::db_json::query_rows(db, "SELECT 1 FROM knowledge.skill_library WHERE slug = ?", &[json!(slug)])?;
            if exists.as_array().map_or(false, |a| !a.is_empty()) { continue; }
            let applies_json = serde_json::to_string(&json!({ "category": cat })).map_err(|e| e.to_string())?;
            crate::db_json::run_write(
                db,
                "INSERT INTO knowledge.skill_library (slug, name, description, body_md, applies_json, provenance) VALUES (?, ?, ?, ?, ?, 'authored')",
                &[json!(slug), json!(name), json!(description), json!(body_md), json!(applies_json)],
            )?;
            seeded += 1;
        }
        return Ok(json!({ "ok": true, "seeded": seeded, "total": total }));
    }

    Err(format!(
        "skill-library: op must be one of: list, get, add, update, toggle, remove, seed (got '{}')",
        op
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS knowledge;
            CREATE TABLE knowledge.skill_library (
              id INTEGER PRIMARY KEY AUTOINCREMENT,
              slug TEXT NOT NULL UNIQUE,
              name TEXT NOT NULL,
              description TEXT NOT NULL,
              body_md TEXT NOT NULL DEFAULT '',
              assets_json TEXT NOT NULL DEFAULT '{}',
              applies_json TEXT NOT NULL DEFAULT '{}',
              verify_cmd TEXT,
              enabled INTEGER NOT NULL DEFAULT 1,
              version INTEGER NOT NULL DEFAULT 1,
              provenance TEXT NOT NULL DEFAULT 'authored',
              created_at TEXT NOT NULL DEFAULT (datetime('now')),
              updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );",
        )
        .unwrap();
        c
    }

    #[test]
    fn add_slugifies_and_defaults() {
        let c = fresh();
        let r = skillLibrary(&json!({ "op": "add", "name": "Fetch & Store!!", "description": "when to use it" }), &c).unwrap();
        assert_eq!(r["skill"]["slug"], json!("fetch-store"));
        assert_eq!(r["skill"]["assets"], json!({}));
        assert_eq!(r["skill"]["applies"], json!({}));
        assert_eq!(r["skill"]["verify_cmd"], Value::Null);
        assert_eq!(r["skill"]["provenance"], json!("authored"));
    }

    #[test]
    fn add_full_fields_and_invalid_provenance_falls_back() {
        let c = fresh();
        let r = skillLibrary(&json!({
            "op": "add", "name": "Paper Press", "slug": "Paper Press!", "description": "when to press",
            "body_md": "do the thing", "assets": {"a": 1}, "applies": {"engines": ["claude"]},
            "verify_cmd": "npm test", "provenance": "bogus"
        }), &c).unwrap();
        assert_eq!(r["skill"]["slug"], json!("paper-press"));
        assert_eq!(r["skill"]["assets"], json!({"a": 1}));
        assert_eq!(r["skill"]["assets_json"], json!("{\"a\":1}"));
        assert_eq!(r["skill"]["verify_cmd"], json!("npm test"));
        assert_eq!(r["skill"]["provenance"], json!("authored")); // "bogus" not in allowlist
    }

    #[test]
    fn update_partial_then_noop_then_null_value() {
        let c = fresh();
        skillLibrary(&json!({ "op": "add", "name": "X", "description": "d" }), &c).unwrap();
        let u = skillLibrary(&json!({ "op": "update", "slug": "x", "name": "Y" }), &c).unwrap();
        assert_eq!(u["skill"]["name"], json!("Y"));
        assert_eq!(u["skill"]["version"], json!(2));
        // no fields changed -> no version bump
        let noop = skillLibrary(&json!({ "op": "update", "slug": "x" }), &c).unwrap();
        assert_eq!(noop["skill"]["version"], json!(2));
        // explicit null value: key present -> String(null) = "null"
        let n = skillLibrary(&json!({ "op": "update", "slug": "x", "description": null }), &c).unwrap();
        assert_eq!(n["skill"]["description"], json!("null"));
        assert_eq!(n["skill"]["version"], json!(3));
    }

    #[test]
    fn toggle_flips_enabled() {
        let c = fresh();
        skillLibrary(&json!({ "op": "add", "name": "T", "description": "d" }), &c).unwrap();
        let t1 = skillLibrary(&json!({ "op": "toggle", "slug": "t" }), &c).unwrap();
        assert_eq!(t1["skill"]["enabled"], json!(0));
        let t2 = skillLibrary(&json!({ "op": "toggle", "slug": "t" }), &c).unwrap();
        assert_eq!(t2["skill"]["enabled"], json!(1));
    }

    #[test]
    fn remove_existing_and_missing_both_ok() {
        let c = fresh();
        skillLibrary(&json!({ "op": "add", "name": "R", "description": "d" }), &c).unwrap();
        let r1 = skillLibrary(&json!({ "op": "remove", "slug": "r" }), &c).unwrap();
        assert_eq!(r1, json!({ "ok": true, "slug": "r" }));
        let r2 = skillLibrary(&json!({ "op": "remove", "slug": "does-not-exist" }), &c).unwrap();
        assert_eq!(r2, json!({ "ok": true, "slug": "does-not-exist" }));
    }

    #[test]
    fn list_orders_by_enabled_desc_name_asc() {
        let c = fresh();
        skillLibrary(&json!({ "op": "add", "name": "Bravo", "description": "d" }), &c).unwrap();
        skillLibrary(&json!({ "op": "add", "name": "Alpha", "description": "d" }), &c).unwrap();
        skillLibrary(&json!({ "op": "add", "name": "Zulu", "description": "d" }), &c).unwrap();
        skillLibrary(&json!({ "op": "toggle", "slug": "zulu" }), &c).unwrap(); // disable zulu
        let l = skillLibrary(&json!({ "op": "list" }), &c).unwrap();
        let names: Vec<String> = l["skills"].as_array().unwrap().iter().map(|s| s["name"].as_str().unwrap().to_string()).collect();
        assert_eq!(names, vec!["Alpha", "Bravo", "Zulu"]); // enabled(1) alpha/bravo before disabled zulu
    }

    #[test]
    fn get_safe_defends_malformed_json_columns() {
        let c = fresh();
        c.execute_batch(
            "INSERT INTO knowledge.skill_library (slug, name, description, assets_json, applies_json) VALUES ('broken','Broken','d','not json','null');",
        )
        .unwrap();
        let r = skillLibrary(&json!({ "op": "get", "slug": "broken" }), &c).unwrap();
        assert_eq!(r["skill"]["assets"], json!({})); // parse failure -> fallback
        assert_eq!(r["skill"]["applies"], json!({})); // parsed to null -> `?? fb` -> fallback
    }

    #[test]
    fn seed_is_idempotent_by_slug() {
        let c = fresh();
        let r1 = skillLibrary(&json!({ "op": "seed" }), &c).unwrap();
        assert_eq!(r1, json!({ "ok": true, "seeded": 8, "total": 8 }));
        let r2 = skillLibrary(&json!({ "op": "seed" }), &c).unwrap();
        assert_eq!(r2, json!({ "ok": true, "seeded": 0, "total": 8 }));
        let n: i64 = c.query_row("SELECT COUNT(*) FROM knowledge.skill_library", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 8);
    }
}

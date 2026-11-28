//! magazine-press.rs — twin of skills/codebase/magazine-press.mjs (expeditions v4 faculty
//! magazine PRESS: one operator-clicked AI call per issue). NAMES ARE CONTRACT for the JS
//! *exports*: `MAGS` (ported as a function returning the same map, Rust has no module-scope
//! object-literal export) and `magazinePress` (default). The module-private JS helpers
//! `ensurePrompt` / `pick` / `compactObservation` / `parseDepartments` are ported too (renamed to
//! snake_case since they are not JS exports, hence not name-contract-bound) because they are
//! fully deterministic and independently testable.
//!
//! `context.db` missing → JS throws `err.contextMissing('magazine-press', 'db')`. Unreachable
//! here: `db: &Connection` is a required, non-nullable reference (same convention as every
//! sibling db-lane twin).
//!
//! PARITY-DEFERRED — TWO boundaries, not the usual one:
//! (1) `magazineObserve` (skills/codebase/magazine-observe.mjs). Per magazine-observe.rs's own
//!     module doc, its ENTIRE orchestrator — every department builder plus the top-level function
//!     itself — composes `cachedFetch` (skills/codebase/_helpers/web-cache.mjs) and has NO Rust
//!     body by design. `magazine-press.mjs` calls it un-guarded (`const obs = await
//!     magazineObserve(...)`, no try/catch) — a failure propagates as a thrown error. This twin
//!     models that exact call as `deferred_magazine_observe`, a local stub mirroring the shape of
//!     `crate::ai_ai_call::aiCall` (always `Err`), invoked with `?` so `magazinePress` propagates
//!     it exactly like the uncaught JS `await`. This is the reason the deterministic surface BELOW
//!     this call (`ensure_prompt`, the `aiCall` try/catch, `build_editorial`, persistence) is
//!     unreachable from `magazinePress` itself in a test — each such helper is ported as its own
//!     pure function and unit-tested directly on constructed inputs, exactly like
//!     atlas-insights-review.rs's `build_evidence`/`pick_review`.
//! (2) the editorial AI call, dispatched through `crate::ai_ai_call::aiCall` (also always `Err`).
//!     UNLIKE (1), the JS wraps this one in try/catch and returns a STATUS object
//!     (`{ok:false, reason:'press-failed', error}`) instead of throwing — mirrored here as
//!     `Ok(status_value)`, never `Err`, for this specific failure.
// QUALITY: deterministic surface (compact_observation, parse_departments, pick, ensure_prompt,
// build_editorial, persistence assembly) fully ported + unit-tested standalone; the top-level
// `magazinePress` reaches only the FIRST deferred boundary (magazineObserve) because that
// dependency itself has no Rust body — documented above, not a shortcut taken by this file.
use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::sync::OnceLock;

// String(v ?? '')
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null())
}
// JS truthy() — used only for `if (departments.product)`.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
fn take_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
} // JS .slice(0,n) (BMP-equivalent, disclosed approximation — codebase-wide convention)

const SKILL: &str = "magazine-press";
const PROMPT: &str = "research.magazine.press.v1";

#[allow(non_snake_case)]
pub fn MAGS() -> &'static Value {
    static M: OnceLock<Value> = OnceLock::new();
    M.get_or_init(|| {
        json!({
            "arch": { "name": "THE STACK GAZETTE", "title": "Architecture & stack", "motto": "read the building, know the bricks" },
            "product": { "name": "THE MARKET HERALD", "title": "Product & market", "motto": "the code is the how; this is the why — and the world decides" },
        })
    })
}

const SYSTEM: &str = "You are the editor of a small, sharp trade periodical about ONE software project. Dry wit is allowed; hedging and invented facts are not. Every claim must trace to the observation you are given — never invent numbers, names, links, or events.";
const TEMPLATE: &str = "You edit «{{magazine}}», the {{faculty_title}} faculty's periodical about this project (\"{{motto}}\"). Below is this issue's OBSERVATION — deterministic facts from the project's own substrate plus cached keyless web sources — and what changed since the last issue. Write the issue's editorial layer, grounded STRICTLY in the observation.\n\nSINCE THE LAST ISSUE: {{delta}}\nPREVIOUS EDITOR'S LETTER (for continuity, may be empty): {{prev_letter}}\nOBSERVATION (JSON):\n{{observation}}\n\nRules:\n- When the observation carries product.dossier — the operator's DECLARED intention — the cover story's job is DRIFT: compare the intention against the field's evidence (dispatches, rivals, the product's own claims) and name the gap or the confirmation. Never invent a positioning the dossier doesn't state.\n\nFill:\n- cover_headline — the single most important development, as a headline (max 90 chars)\n- cover_standfirst — one sentence under the headline (max 160 chars)\n- cover_department — the department key the cover story comes from (one of the observation's top-level keys)\n- letter_md — the editor's letter: 3–6 sentences of plain prose that cross-reference at least two departments\n- departments_json — ONLY a JSON array with one entry per department key present in the observation: [{\"key\":\"…\",\"headline\":\"max 70 chars\",\"caption\":\"one line, max 140 chars\"}]";

fn ensure_prompt(db: &Connection) -> Result<(), String> {
    let existing = crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT }), db).ok();
    if let Some(e) = &existing {
        if e.get("template_md").and_then(Value::as_str) == Some(TEMPLATE) {
            return Ok(());
        }
    }
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": PROMPT, "template_md": TEMPLATE, "system_md": SYSTEM,
            "default_tier": "mid", "default_temperature": "warm", "phase_flow": "single_only",
            "max_output_tokens": 1200,
            "output_fields": [
                { "field_name": "cover_headline", "field_type": "string" },
                { "field_name": "cover_standfirst", "field_type": "string" },
                { "field_name": "cover_department", "field_type": "string" },
                { "field_name": "letter_md", "field_type": "string" },
                { "field_name": "departments_json", "field_type": "string" },
            ],
        }),
        db,
    )?;
    Ok(())
}

// `const pick = (res, field) => res?.output_kv?.find((o) => o.field_name === field && o.parent_output_id == null)?.value_text ?? null;`
fn pick(res: &Value, field: &str) -> Option<String> {
    let rows = res.get("output_kv")?.as_array()?;
    let row = rows.iter().find(|o| {
        o.get("field_name").and_then(Value::as_str) == Some(field)
            && o.get("parent_output_id").map_or(true, |v| v.is_null())
    })?;
    row.get("value_text").and_then(Value::as_str).map(|s| s.to_string())
}

// only what the editor needs — top rows per department, ids over prose
fn compact_observation(faculty: &str, departments: &Value) -> Value {
    let mut c = Map::new();
    let get = |k: &str| departments.get(k);

    // the operator's own desk material grounds every edition
    if let Some(items) = get("desk").and_then(|d| d.get("items")).and_then(Value::as_array) {
        if !items.is_empty() {
            let mapped: Vec<Value> = items
                .iter()
                .map(|d| {
                    let body = str_qq(d.get("body"));
                    json!({ "label": d.get("label").cloned().unwrap_or(Value::Null), "body": take_chars(&body, 400) })
                })
                .collect();
            c.insert("desk".into(), json!({ "items": mapped }));
        }
    }

    if faculty == "arch" {
        let ledger = get("ledger");
        let packages = ledger.and_then(|l| l.get("packages")).cloned().unwrap_or_else(|| json!([]));
        let deps_raw: Vec<Value> = ledger.and_then(|l| l.get("deps")).and_then(Value::as_array).cloned().unwrap_or_default();
        let deps: Vec<Value> = deps_raw
            .iter()
            .take(14)
            .map(|d| {
                let advisories: Vec<Value> = d
                    .get("advisories")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .map(|a| a.get("id").cloned().unwrap_or(Value::Null))
                    .collect();
                json!({
                    "name": d.get("name").cloned().unwrap_or(Value::Null),
                    "current": d.get("current").cloned().unwrap_or(Value::Null),
                    "latest": d.get("latest").cloned().unwrap_or(Value::Null),
                    "behind": d.get("behind").cloned().unwrap_or(Value::Null),
                    "dev": d.get("dev").cloned().unwrap_or(Value::Null),
                    "advisories": advisories,
                })
            })
            .collect();
        c.insert("ledger".into(), json!({ "packages": packages, "deps": deps }));
        c.insert("horizon".into(), get("horizon").cloned().unwrap_or(Value::Null));

        let wire_items: Vec<Value> = get("wire").and_then(|w| w.get("items")).and_then(Value::as_array).cloned().unwrap_or_default();
        let wire: Vec<Value> = wire_items
            .iter()
            .take(12)
            .map(|it| json!({ "title": it.get("title").cloned().unwrap_or(Value::Null), "source": it.get("source").cloned().unwrap_or(Value::Null), "date": it.get("date").cloned().unwrap_or(Value::Null) }))
            .collect();
        c.insert("wire".into(), json!({ "items": wire }));

        // the observatory numbers — the world's stats on this stack; the editor's
        // departments_json entry for 'stats' is the data spread's AI review
        let press_items: Vec<Value> = get("press").and_then(|w| w.get("items")).and_then(Value::as_array).cloned().unwrap_or_default();
        let press: Vec<Value> = press_items
            .iter()
            .take(8)
            .map(|it| json!({ "title": it.get("title").cloned().unwrap_or(Value::Null), "source": it.get("source").cloned().unwrap_or(Value::Null), "date": it.get("date").cloned().unwrap_or(Value::Null) }))
            .collect();
        c.insert("press".into(), json!({ "items": press }));

        let techs: Vec<Value> = get("stats").and_then(|s| s.get("techs")).and_then(Value::as_array).cloned().unwrap_or_default();
        let stats_techs: Vec<Value> = techs
            .iter()
            .map(|t| {
                json!({
                    "name": t.get("name").cloned().unwrap_or(Value::Null),
                    "weekly_downloads": t.get("weekly").cloned().unwrap_or(Value::Null),
                    "stars": t.get("stars").cloned().unwrap_or(Value::Null),
                    "releases_90d": t.get("releases_90d").cloned().unwrap_or(Value::Null),
                    "runway_days": t.get("runway_days").cloned().unwrap_or(Value::Null),
                })
            })
            .collect();
        c.insert("stats".into(), json!({ "techs": stats_techs }));
        c.insert("notices".into(), get("notices").cloned().unwrap_or(Value::Null));
    } else {
        // the intention side of the comparator: what the code proves + what the
        // operator declares + what the product's own site claims
        if truthy(get("product")) {
            let product = get("product").unwrap();
            let features: Vec<Value> = product.get("features").and_then(Value::as_array).cloned().unwrap_or_default().into_iter().take(10).collect();
            let self_v = product.get("self");
            let self_out = if truthy(self_v) {
                let sv = self_v.unwrap();
                let claims: Vec<Value> = sv.get("claims").and_then(Value::as_array).cloned().unwrap_or_default().into_iter().take(5).collect();
                json!({ "title": sv.get("title").cloned().unwrap_or(Value::Null), "claims": claims })
            } else {
                Value::Null
            };
            c.insert(
                "product".into(),
                json!({
                    "dossier": product.get("dossier").cloned().unwrap_or(Value::Null),
                    "features": features,
                    "self": self_out,
                }),
            );
        }
        let rivals_raw: Vec<Value> = get("rivals").and_then(|r| r.get("rivals")).and_then(Value::as_array).cloned().unwrap_or_default();
        let rivals: Vec<Value> = rivals_raw
            .iter()
            .map(|r| {
                let desc = str_qq(r.get("description"));
                let claims: Vec<Value> = r.get("claims").and_then(Value::as_array).cloned().unwrap_or_default().into_iter().take(6).collect();
                json!({
                    "name": r.get("name").cloned().unwrap_or(Value::Null),
                    "ok": r.get("ok").cloned().unwrap_or(Value::Null),
                    "title": r.get("title").cloned().unwrap_or(Value::Null),
                    "description": take_chars(&desc, 140),
                    "claims": claims,
                })
            })
            .collect();
        c.insert("rivals".into(), json!({ "rivals": rivals }));

        let dispatch_items: Vec<Value> = get("dispatches").and_then(|d| d.get("items")).and_then(Value::as_array).cloned().unwrap_or_default();
        let dispatches_items: Vec<Value> = dispatch_items
            .iter()
            .take(12)
            .map(|it| {
                json!({
                    "title": it.get("title").cloned().unwrap_or(Value::Null),
                    "source": it.get("source").cloned().unwrap_or(Value::Null),
                    "date": it.get("date").cloned().unwrap_or(Value::Null),
                    "points": it.get("points").cloned().unwrap_or(Value::Null),
                })
            })
            .collect();
        c.insert(
            "dispatches".into(),
            json!({
                "keywords": get("dispatches").and_then(|d| d.get("keywords")).cloned().unwrap_or_else(|| json!([])),
                "items": dispatches_items,
            }),
        );
    }
    Value::Object(c)
}

fn fence_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"```[a-z]*\n?|```").unwrap())
}
fn bracket_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"\[[\s\S]*\]").unwrap())
}

fn parse_departments(text: Option<&str>) -> Value {
    let t = text.unwrap_or("");
    let cleaned = fence_re().replace_all(t, "").to_string();
    let bracket = bracket_re().find(&cleaned).map(|m| m.as_str().to_string());
    let arr_val: Value = match bracket {
        Some(s) => match serde_json::from_str(&s) {
            Ok(v) => v,
            Err(_) => return json!([]),
        },
        None => json!([]),
    };
    let arr = match arr_val.as_array() {
        Some(a) => a,
        None => return json!([]),
    };
    let out: Vec<Value> = arr
        .iter()
        .filter(|e| e.get("key").and_then(Value::as_str).is_some())
        .take(8)
        .map(|e| {
            let key = e.get("key").and_then(Value::as_str).unwrap_or("");
            let headline = str_qq(e.get("headline"));
            let caption = str_qq(e.get("caption"));
            json!({ "key": take_chars(key, 24), "headline": take_chars(&headline, 90), "caption": take_chars(&caption, 180) })
        })
        .collect();
    json!(out)
}

// `const cover_headline = pick(...); const letter_md = pick(...); if (!headline && !letter) return
// press-failed status; else build the editorial object.` — the empty-editorial guard + assembly,
// ported standalone since `magazinePress` cannot reach it (see module doc, boundary #1).
fn build_editorial(res: &Value) -> Option<Value> {
    let cover_headline = pick(res, "cover_headline");
    let letter_md = pick(res, "letter_md");
    let has_headline = cover_headline.as_ref().map(|s| !s.trim().is_empty()).unwrap_or(false);
    let has_letter = letter_md.as_ref().map(|s| !s.trim().is_empty()).unwrap_or(false);
    if !has_headline && !has_letter {
        return None;
    }
    let cover_standfirst = pick(res, "cover_standfirst").unwrap_or_default();
    let cover_department = pick(res, "cover_department").unwrap_or_default();
    let departments = parse_departments(pick(res, "departments_json").as_deref());
    Some(json!({
        "cover": {
            "headline": take_chars(&cover_headline.unwrap_or_default(), 120),
            "standfirst": take_chars(&cover_standfirst, 200),
            "department": take_chars(&cover_department, 24),
        },
        "letter_md": letter_md.unwrap_or_default(),
        "departments": departments,
    }))
}

// PARITY-DEFERRED boundary #1 (see module doc): `magazineObserve`'s entire orchestrator composes
// `cachedFetch` for every department builder — it has no Rust body. JS does not wrap this call in
// try/catch, so a failure propagates as a thrown error; mirrored here via `?` at the call site.
fn deferred_magazine_observe(_faculty: &str, _input: &Value) -> Result<Value, String> {
    Err("magazine-observe: PARITY-DEFERRED — magazineObserve's department builders depend on cachedFetch (network), no Rust body".to_string())
}

pub fn magazinePress(input: &Value, db: &Connection) -> Result<Value, String> {
    let faculty = str_qq(input.get("faculty")).trim().to_string();
    if !crate::codebase_magazine_observe::FACULTIES.contains(&faculty.as_str()) {
        return Err(format!(
            "{SKILL}: 'faculty' must be one of {:?}, got '{faculty}'",
            crate::codebase_magazine_observe::FACULTIES
        ));
    }
    crate::codebase__helpers_magazine_db::ensureMagazineIssues(db);

    let obs = deferred_magazine_observe(&faculty, input)?;

    let prev = crate::codebase__helpers_magazine_db::latestIssue(db, &json!(faculty.clone()));
    let force = input.get("force").and_then(Value::as_bool).unwrap_or(false);
    if !prev.is_null() {
        let prev_hash = prev.get("observation_hash").cloned().unwrap_or(Value::Null);
        let obs_hash = obs.get("hash").cloned().unwrap_or(Value::Null);
        if prev_hash == obs_hash && !force {
            return Ok(json!({
                "ok": false, "reason": "nothing-new",
                "issue": {
                    "issue_no": prev.get("issue_no").cloned().unwrap_or(Value::Null),
                    "pressed_at": prev.get("pressed_at").cloned().unwrap_or(Value::Null),
                    "observation_hash": prev.get("observation_hash").cloned().unwrap_or(Value::Null),
                    "editorial": prev.get("editorial").cloned().unwrap_or(Value::Null),
                },
            }));
        }
    }

    ensure_prompt(db)?;
    let mag = MAGS().get(faculty.as_str()).cloned().unwrap_or(Value::Null);
    let delta_arr: Vec<String> = obs.get("delta").and_then(Value::as_array).cloned().unwrap_or_default().iter().map(|v| str_qq(Some(v))).collect();
    let delta_str = if !delta_arr.is_empty() { delta_arr.join("; ") } else { "(first issue — no previous edition)".to_string() };
    let prev_letter_raw = take_chars(&str_qq(prev.get("editorial").and_then(|e| e.get("letter_md"))), 400);
    let prev_letter = if prev_letter_raw.is_empty() { "(none)".to_string() } else { prev_letter_raw };
    let observation_json = serde_json::to_string(&compact_observation(&faculty, obs.get("departments").unwrap_or(&Value::Null))).unwrap_or_else(|_| "{}".into());
    let observation_sliced = take_chars(&observation_json, 9000);

    let ai_input = json!({
        "prompt_name": PROMPT,
        "input_kv": [
            { "key": "magazine", "value": mag.get("name").cloned().unwrap_or(Value::Null) },
            { "key": "faculty_title", "value": mag.get("title").cloned().unwrap_or(Value::Null) },
            { "key": "motto", "value": mag.get("motto").cloned().unwrap_or(Value::Null) },
            { "key": "delta", "value": delta_str },
            { "key": "prev_letter", "value": prev_letter },
            { "key": "observation", "value": observation_sliced },
        ],
        "tier": "mid", "temperature": "warm", "phase_flow": "single_only", "provider_fn": Value::Null,
    });

    // ── boundary #2: the editorial AI call — JS catches this one and returns a status object.
    let res = match crate::ai_ai_call::aiCall(&ai_input, db) {
        Ok(r) => r,
        Err(e) => {
            let msg: String = e.chars().take(300).collect();
            return Ok(json!({ "ok": false, "reason": "press-failed", "error": msg }));
        }
    };

    let editorial = match build_editorial(&res) {
        Some(e) => e,
        None => return Ok(json!({ "ok": false, "reason": "press-failed", "error": "the editor returned an empty editorial" })),
    };

    let row = crate::codebase__helpers_magazine_db::appendIssue(
        db,
        &json!({
            "faculty": &faculty,
            "observation_hash": obs.get("hash").cloned().unwrap_or(Value::Null),
            "observation": obs.get("departments").cloned().unwrap_or_else(|| json!({})),
            "editorial": &editorial,
            "model_id": nz(res.get("model_id")).cloned().unwrap_or(Value::Null),
        }),
    );

    Ok(json!({
        "ok": true,
        "issue": {
            "issue_no": row.get("issue_no").cloned().unwrap_or(Value::Null),
            "pressed_at": row.get("pressed_at").cloned().unwrap_or(Value::Null),
            "observation_hash": row.get("observation_hash").cloned().unwrap_or(Value::Null),
            "editorial": editorial,
        },
        "ai": crate::codebase__helpers_magazine_db::aiMeta(db, &res),
        "model_id": nz(res.get("model_id")).cloned().unwrap_or(Value::Null),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS knowledge; ATTACH ':memory:' AS brain;").unwrap();
        c.execute_batch(
            "CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        ).unwrap();
        c
    }

    #[test]
    fn mags_has_both_faculties_with_exact_copy() {
        let m = MAGS();
        assert_eq!(m["arch"]["name"], json!("THE STACK GAZETTE"));
        assert_eq!(m["arch"]["motto"], json!("read the building, know the bricks"));
        assert_eq!(m["product"]["name"], json!("THE MARKET HERALD"));
    }

    #[test]
    fn invalid_faculty_is_rejected_before_any_db_or_deferred_call() {
        let c = fresh();
        let err = magazinePress(&json!({ "faculty": "nope" }), &c).unwrap_err();
        assert!(err.contains("faculty"));
        // guard fires before ensureMagazineIssues — table must NOT exist
        let n: i64 = c.query_row("SELECT COUNT(*) FROM knowledge.sqlite_master WHERE name='magazine_issues'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn valid_faculty_ensures_table_then_propagates_the_deferred_observe_error() {
        let c = fresh();
        let err = magazinePress(&json!({ "faculty": "arch" }), &c).unwrap_err();
        // the boundary here is cachedFetch's deferred stub, NOT aiCall — magazinePress never
        // reaches an AI call, so this assertion must stay on the PARITY-DEFERRED wording.
        assert!(err.contains("PARITY-DEFERRED"), "unexpected error: {err}");
        assert!(err.contains("cachedFetch"));
        // ensureMagazineIssues ran BEFORE the deferred call (matches JS statement order)
        let n: i64 = c.query_row("SELECT COUNT(*) FROM knowledge.sqlite_master WHERE name='magazine_issues'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn pick_finds_top_level_field_ignoring_nested() {
        let res = json!({ "output_kv": [
            { "field_name": "other", "value_text": "x", "parent_output_id": null },
            { "field_name": "cover_headline", "value_text": "  H  ", "parent_output_id": null },
            { "field_name": "cover_headline", "value_text": "nested", "parent_output_id": 5 },
        ] });
        assert_eq!(pick(&res, "cover_headline"), Some("  H  ".to_string()));
        assert_eq!(pick(&res, "missing"), None);
        assert_eq!(pick(&json!({}), "x"), None);
    }

    #[test]
    fn compact_observation_arch_trims_and_defaults() {
        let departments = json!({
            "desk": { "items": [{ "label": "L1", "body": "b".repeat(500) }] },
            "ledger": {
                "deps": (0..20).map(|i| json!({ "name": format!("pkg{i}"), "current": "1.0.0", "latest": "2.0.0", "behind": 1, "dev": false, "advisories": [{ "id": "GHSA-X" }] })).collect::<Vec<_>>(),
            },
            "horizon": { "products": [{ "product": "node" }] },
            "wire": { "items": (0..20).map(|i| json!({ "title": format!("w{i}"), "source": "s", "date": "d" })).collect::<Vec<_>>() },
            "press": { "items": (0..20).map(|i| json!({ "title": format!("p{i}"), "source": "s", "date": "d" })).collect::<Vec<_>>() },
            "stats": { "techs": [{ "name": "vite", "weekly": 100, "stars": 5, "releases_90d": 2, "runway_days": 10 }] },
            "notices": { "n": 1 },
        });
        let c = compact_observation("arch", &departments);
        assert_eq!(c["desk"]["items"][0]["body"].as_str().unwrap().len(), 400);
        assert_eq!(c["ledger"]["deps"].as_array().unwrap().len(), 14);
        assert_eq!(c["ledger"]["deps"][0]["advisories"], json!(["GHSA-X"]));
        assert_eq!(c["ledger"]["packages"], json!([])); // absent -> default []
        assert_eq!(c["horizon"], json!({ "products": [{ "product": "node" }] }));
        assert_eq!(c["wire"]["items"].as_array().unwrap().len(), 12);
        assert_eq!(c["press"]["items"].as_array().unwrap().len(), 8);
        assert_eq!(c["stats"]["techs"][0]["weekly_downloads"], json!(100));
        assert_eq!(c["notices"], json!({ "n": 1 }));
        assert!(c.get("product").is_none()); // arch branch never sets product/rivals/dispatches
    }

    #[test]
    fn compact_observation_product_handles_dossier_self_and_falsy_product() {
        let departments = json!({
            "product": { "dossier": null, "features": (0..15).map(|i| json!(format!("f{i}"))).collect::<Vec<_>>(), "self": { "title": "T", "claims": (0..8).map(|i| json!(format!("c{i}"))).collect::<Vec<_>>() } },
            "rivals": { "rivals": [{ "name": "R", "ok": true, "title": "riv", "description": "d".repeat(200), "claims": (0..10).map(|i| json!(format!("c{i}"))).collect::<Vec<_>>() }] },
            "dispatches": { "keywords": ["k1"], "items": (0..15).map(|i| json!({ "title": format!("d{i}"), "source": "s", "date": "d", "points": i })).collect::<Vec<_>>() },
        });
        let c = compact_observation("product", &departments);
        assert_eq!(c["product"]["dossier"], Value::Null);
        assert_eq!(c["product"]["features"].as_array().unwrap().len(), 10);
        assert_eq!(c["product"]["self"]["claims"].as_array().unwrap().len(), 5);
        assert_eq!(c["rivals"]["rivals"][0]["description"].as_str().unwrap().len(), 140);
        assert_eq!(c["rivals"]["rivals"][0]["claims"].as_array().unwrap().len(), 6);
        assert_eq!(c["dispatches"]["keywords"], json!(["k1"]));
        assert_eq!(c["dispatches"]["items"].as_array().unwrap().len(), 12);

        // product key falsy (null) -> not set at all
        let no_product = compact_observation("product", &json!({ "product": null, "rivals": {}, "dispatches": {} }));
        assert!(no_product.get("product").is_none());
        assert_eq!(no_product["rivals"], json!({ "rivals": [] }));
        assert_eq!(no_product["dispatches"], json!({ "keywords": [], "items": [] }));
    }

    #[test]
    fn parse_departments_extracts_fenced_json_filters_and_truncates() {
        let text = "```json\n[{\"key\":\"ledger\",\"headline\":\"H\",\"caption\":\"C\"},{\"key\":123},{\"nope\":true}]\n```";
        let out = parse_departments(Some(text));
        assert_eq!(out.as_array().unwrap().len(), 1); // only string-key entry survives
        assert_eq!(out[0], json!({ "key": "ledger", "headline": "H", "caption": "C" }));

        assert_eq!(parse_departments(Some("no brackets here")), json!([]));
        assert_eq!(parse_departments(Some("[not valid json")), json!([]));
        assert_eq!(parse_departments(None), json!([]));
        assert_eq!(parse_departments(Some("{\"key\":\"not-an-array\"}")), json!([])); // object, not array -> []

        // > 8 entries truncated to 8, and each field sliced
        let many: Vec<Value> = (0..12).map(|i| json!({ "key": format!("k{i}"), "headline": "h".repeat(100), "caption": "c".repeat(200) })).collect();
        let text2 = serde_json::to_string(&many).unwrap();
        let out2 = parse_departments(Some(&text2));
        assert_eq!(out2.as_array().unwrap().len(), 8);
        assert_eq!(out2[0]["headline"].as_str().unwrap().len(), 90);
        assert_eq!(out2[0]["caption"].as_str().unwrap().len(), 180);
    }

    #[test]
    fn build_editorial_empty_both_fields_is_none() {
        let res = json!({ "output_kv": [{ "field_name": "cover_headline", "value_text": "   ", "parent_output_id": null }] });
        assert_eq!(build_editorial(&res), None);
        assert_eq!(build_editorial(&json!({})), None);
    }

    #[test]
    fn build_editorial_headline_only_builds_full_object() {
        let res = json!({ "output_kv": [
            { "field_name": "cover_headline", "value_text": "H".repeat(150), "parent_output_id": null },
            { "field_name": "cover_standfirst", "value_text": "S".repeat(300), "parent_output_id": null },
            { "field_name": "cover_department", "value_text": "ledger-department-too-long", "parent_output_id": null },
            { "field_name": "departments_json", "value_text": "[{\"key\":\"ledger\",\"headline\":\"h\",\"caption\":\"c\"}]", "parent_output_id": null },
        ] });
        let e = build_editorial(&res).unwrap();
        assert_eq!(e["cover"]["headline"].as_str().unwrap().len(), 120);
        assert_eq!(e["cover"]["standfirst"].as_str().unwrap().len(), 200);
        assert_eq!(e["cover"]["department"].as_str().unwrap().len(), 24);
        assert_eq!(e["letter_md"], json!("")); // absent field -> unwrap_or_default, NOT sliced
        assert_eq!(e["departments"], json!([{ "key": "ledger", "headline": "h", "caption": "c" }]));
    }

    #[test]
    fn ensure_prompt_is_idempotent_on_unchanged_template() {
        let c = fresh();
        ensure_prompt(&c).unwrap();
        let n1: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = ?", [PROMPT], |r| r.get(0)).unwrap();
        assert_eq!(n1, 1);
        ensure_prompt(&c).unwrap(); // same template_md -> no second insert
        let n2: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = ?", [PROMPT], |r| r.get(0)).unwrap();
        assert_eq!(n2, 1);
    }
}

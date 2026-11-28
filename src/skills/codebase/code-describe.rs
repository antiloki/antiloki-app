//! code-describe.rs — twin of skills/codebase/code-describe.mjs (db, LLM-partial). NAMES ARE
//! CONTRACT: `codeDescribe` (default). STEP 2 of the per-file description (LOW-AI ESSAY,
//! on-demand + cached): reads the file's deterministic facts (gathering them via the already-
//! converted `codebase_code_describe_gather::gatherFacts` twin when missing/stale) and asks a LOW
//! model to narrate the file's role, grounded strictly in those facts. Cached by content_hash: an
//! unchanged file returns the stored essay; a changed file (or none) regenerates and re-persists.
//!
//! PARITY-DEFERRED: the LOW-AI narration call goes through `crate::ai_ai_call::aiCall` (the
//! deferred network-AI stub — see ai-call.rs's module doc), which always returns `Err`. The JS
//! source wraps that call in a `try/catch` that swallows ANY failure (network error alike) and
//! falls back to `summary_md = null` — so this twin's `Err` branch is not an approximation of the
//! JS behavior, it faithfully reproduces the real catch-and-null-out path. Everything else —
//! guards, the cache/staleness check, the fact-gather + UPSERT, prompt-template ensure (reusing
//! the already-converted `ai_prompt_find`/`ai_prompt_add` twins), the `fmtSyms`/join formatting
//! of the prompt's input_kv, the post-call persist, and the output shape — is ported byte-for-byte
//! and is the deterministic surface this file's tests exercise.
use rusqlite::Connection;
use serde_json::{json, Value};

// ── standard helpers (playbook) ─────────────────────────────────────────────────────────────
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
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(x) => str_qq(Some(x)),
    }
}
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }

const PROMPT: &str = "code.describe.v1";
const SYSTEM: &str = "You are a precise software archivist. Given STRUCTURED FACTS about one source file (its symbols, what it imports, who depends on it, its role), you write a SHORT plain-English narrative of what the file does conceptually and its place in the architecture. State ONLY what the facts support — never invent behaviour. No code, no headers.";
/// The description prompt's text, for the batch describer's price estimate.
pub const TEMPLATE_TEXT: &str = TEMPLATE;
const TEMPLATE: &str = "Write a 2–4 sentence conceptual description of this file — what it does and its role in the system — grounded ONLY in the facts below. Lead with its purpose, then how it fits (what it builds on / what relies on it). Plain prose, no bullet points, no code.\n\nFILE: {{path}}  (role: {{role}})\nDEFINES: {{symbols}}\nIMPORTS (depends on): {{imports}}\nDEPENDED ON BY: {{dependents}}\n\nPut the description into summary_md.";

// try promptFind; on miss OR stale template, promptAdd (bumps version) — mirrors JS ensurePrompt's
// try/catch(existing = null) + `if (existing && existing.template_md === TEMPLATE) return;`.
fn ensure_prompt(db: &Connection) -> Result<(), String> {
    let existing = crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT }), db).ok();
    if let Some(e) = &existing {
        if e.get("template_md").and_then(|v| v.as_str()) == Some(TEMPLATE) {
            return Ok(());
        }
    }
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": PROMPT,
            "template_md": TEMPLATE,
            "system_md": SYSTEM,
            "default_tier": "low",
            "default_temperature": "cold",
            "phase_flow": "single_only",
            "max_output_tokens": 600,
            "output_fields": [{ "field_name": "summary_md", "field_type": "string" }],
        }),
        db,
    )?;
    Ok(())
}

// `res?.output_kv?.find((o) => o.field_name === 'summary_md' && o.parent_output_id == null)?.value_text ?? null`
fn pick(res: &Value) -> Option<String> {
    let arr = res.get("output_kv")?.as_array()?;
    let hit = arr.iter().find(|o| {
        o.get("field_name").and_then(|v| v.as_str()) == Some("summary_md")
            && o.get("parent_output_id").map_or(true, |v| v.is_null())
    })?;
    hit.get("value_text").and_then(|v| v.as_str()).map(|s| s.to_string())
}

// `(syms ?? []).slice(0, 24).map((s) => \`${s.name ?? '?'}${s.signature ?? ''}${s.summary ? \` — ${s.summary}\` : (s.kind ? \` (${s.kind})\` : '')}\`).join('; ') || '(none)'`
fn fmt_syms(syms: Option<&Value>) -> String {
    let arr = nz(syms).and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let parts: Vec<String> = arr
        .iter()
        .take(24)
        .map(|s| {
            let name = match nz(s.get("name")) { Some(v) => tmpl(Some(v)), None => "?".to_string() };
            let sig = match nz(s.get("signature")) { Some(v) => tmpl(Some(v)), None => String::new() };
            let suffix = if truthy(s.get("summary")) {
                format!(" — {}", tmpl(s.get("summary")))
            } else if truthy(s.get("kind")) {
                format!(" ({})", tmpl(s.get("kind")))
            } else {
                String::new()
            };
            format!("{name}{sig}{suffix}")
        })
        .collect();
    let joined = parts.join("; ");
    if joined.is_empty() { "(none)".to_string() } else { joined }
}

// `(arr ?? []).slice(0, 30).join(', ') || '(none)'`
fn join_or_none(v: Option<&Value>) -> String {
    let arr = nz(v).and_then(|x| x.as_array()).cloned().unwrap_or_default();
    let joined = arr.iter().take(30).map(|x| tmpl(Some(x))).collect::<Vec<_>>().join(", ");
    if joined.is_empty() { "(none)".to_string() } else { joined }
}

fn select_row(db: &Connection, origin: &Value, path: &Value) -> Result<Option<Value>, String> {
    let rows = crate::db_json::query_rows(
        db,
        "SELECT facts_json, summary_md, content_hash, model_id FROM code_descriptions WHERE origin = ? AND path = ?",
        &[origin.clone(), path.clone()],
    )?;
    Ok(rows.as_array().and_then(|a| a.first()).cloned())
}

pub fn codeDescribe(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const { db, provider_fn = null } = context ?? {}; if (!db) throw err.contextMissing(...)`
    // — unreachable here (Rust's `db: &Connection` is never absent). provider_fn is a JS function
    // reference threaded opaquely into aiCall's input; there is no Rust equivalent to carry, so it
    // is passed as `null` below (the deferred aiCall boundary never inspects it either way).
    let path = input.get("path");
    if !truthy(path) {
        return Err("code-describe: path is required".to_string());
    }
    let path = path.unwrap().clone();
    let origin = nz(input.get("origin")).cloned().unwrap_or(json!("atlas"));

    let mut row = select_row(db, &origin, &path)?;
    let mut facts: Option<Value> = row
        .as_ref()
        .and_then(|r| r.get("facts_json"))
        .and_then(|v| v.as_str())
        .and_then(|s| serde_json::from_str::<Value>(s).ok());

    let cur_rows = crate::db_json::query_rows(
        db,
        "SELECT content_hash FROM code_nodes WHERE origin = ? AND path = ? AND type = 'file' LIMIT 1",
        &[origin.clone(), path.clone()],
    )?;
    let cur = cur_rows
        .as_array()
        .and_then(|a| a.first())
        .and_then(|r| r.get("content_hash").cloned())
        .unwrap_or(Value::Null);
    let row_content_hash = row.as_ref().and_then(|r| r.get("content_hash").cloned()).unwrap_or(Value::Null);

    // gather if missing OR stale → and drop the now-stale essay
    if facts.is_none() || row_content_hash != cur {
        let f = crate::codebase_code_describe_gather::gatherFacts(db, &origin, &path);
        let facts_json = serde_json::to_string(&f).map_err(|e| e.to_string())?;
        let content_hash = f.get("content_hash").cloned().unwrap_or(Value::Null);
        crate::db_json::run_write(
            db,
            &format!("INSERT INTO code_descriptions (origin, path, facts_json, content_hash, gathered_at) \
             VALUES (?,?,?,?,{}) \
             ON CONFLICT(origin, path) DO UPDATE SET facts_json=excluded.facts_json, content_hash=excluded.content_hash, \
               gathered_at={}, summary_md=NULL, generated_at=NULL", crate::clock::now_sql(), crate::clock::now_sql()),
            &[origin.clone(), path.clone(), json!(facts_json), content_hash],
        )?;
        facts = Some(f);
        row = select_row(db, &origin, &path)?;
    }
    let facts = facts.unwrap_or(json!({}));

    // fresh cached essay → return as-is unless forced
    if !truthy(input.get("force")) {
        if let Some(r) = &row {
            if truthy(r.get("summary_md")) {
                let summary_md = r.get("summary_md").cloned().unwrap_or(Value::Null);
                let model_id = nz(r.get("model_id")).cloned().unwrap_or(Value::Null);
                return Ok(json!({
                    "path": path, "origin": origin, "summary_md": summary_md,
                    "facts": facts, "cached": true, "model_id": model_id,
                }));
            }
        }
    }

    ensure_prompt(db)?;

    let ai_input = build_ai_input(&path, &facts, input.get("model_id").and_then(Value::as_str));

    // `try { ... } catch { summary_md = null; }` — a real network error and the deferred stub's
    // Err are handled identically: swallowed, summary_md stays null, model_id stays null.
    let (summary_md, model_id): (Value, Value) = match crate::ai_ai_call::aiCall(&ai_input, db) {
        Ok(res) => {
            let s = pick(&res).map(|x| json!(x)).unwrap_or(Value::Null);
            let m = res.get("model_id").cloned().unwrap_or(Value::Null);
            (s, m)
        }
        Err(_) => (Value::Null, Value::Null),
    };

    // `if (summary_md && summary_md.trim())`
    if let Some(s) = summary_md.as_str() {
        if !s.is_empty() && !s.trim().is_empty() {
            crate::db_json::run_write(
                db,
                &format!("UPDATE code_descriptions SET summary_md = ?, model_id = ?, generated_at = {} WHERE origin = ? AND path = ?", crate::clock::now_sql()),
                &[summary_md.clone(), model_id.clone(), origin.clone(), path.clone()],
            )?;
        }
    }

    Ok(json!({
        "path": path, "origin": origin, "summary_md": summary_md,
        "facts": facts, "cached": false, "model_id": model_id,
    }))
}

/// The aiCall input for one file. `pin` is the seat the caller chose (`cli:<engine>`, 2026-10-01: the batch describer picks
/// the cheapest usable seat and pins it - without a pin the tier's own provider row decides, and on a box with Claude
/// switched off that row is a seat that cannot answer). Anything that is not a `cli:` pin is ignored. Pure - tested.
fn build_ai_input(path: &Value, facts: &Value, pin: Option<&str>) -> Value {
    let role = match nz(facts.get("role")) { Some(v) => tmpl(Some(v)), None => "logic".to_string() };
    let mut ai_input = json!({
        "prompt_name": PROMPT,
        "input_kv": [
            { "key": "path", "value": path },
            { "key": "role", "value": role },
            { "key": "symbols", "value": fmt_syms(facts.get("symbols")) },
            { "key": "imports", "value": join_or_none(facts.get("imports")) },
            { "key": "dependents", "value": join_or_none(facts.get("dependents")) },
        ],
        "tier": "low", "temperature": "cold", "phase_flow": "single_only", "provider_fn": Value::Null,
    });
    if let Some(pin) = pin.map(str::trim).filter(|m| m.starts_with("cli:") && m.len() > 4) {
        ai_input["model_id"] = json!(pin);
    }
    ai_input
}

// ── HOW MUCH OF THE REPO HAS PROSE, AND WHICH FILES SHOULD GET IT NEXT (2026-10-01) ───────────────────────────────
//
// "X-ray summaries empty (0% prose)": every file review showed its deterministic facts and no sentence about what the
// file is FOR. Not broken, not unwired - never asked. `codeDescribe` writes one essay for ONE file, on demand, from a
// button in the atlas panel; onboarding gathers the FACTS of every file (`code-describe-gather`) and stops there, and
// its `summarize` flag builds the architecture digest (one call), not file descriptions. So a fresh onboarding is 0%
// prose by construction, and nothing said so. These are the pure halves of the fix: what counts as prose, which files
// are worth a call, and the honest sentence. The seat, the price and the loop live in antiloki-serve (`describe.rs`).

/// One file the describer could spend a call on, with what ranks it.
#[derive(Clone, Debug, PartialEq)]
pub struct DescribeCandidate {
    pub path: String,
    /// files that import it - the file's weight in the graph
    pub dependents: i64,
    pub symbols: i64,
    pub role: String,
    /// the size of the facts the prompt would carry
    pub facts_chars: usize,
    /// an essay exists and was written for the file as it is now
    pub fresh: bool,
}

/// A path worth a sentence: not a test, a fixture, a snapshot, a build output, a vendored copy or a lockfile - those get
/// described by what they test or generate, and a budget spent on them is a budget not spent on the code. Pure.
pub fn is_describable_path(path: &str) -> bool {
    let p = path.replace('\\', "/").to_ascii_lowercase();
    let segs: Vec<&str> = p.split('/').collect();
    let dirs = &segs[..segs.len().saturating_sub(1)];
    const SKIP_DIRS: &[&str] = &["test", "tests", "__tests__", "__mocks__", "__fixtures__", "__snapshots__", "fixtures", "fixture", "e2e", "spec", "specs",
        "dist", "build", "out", "vendor", "third_party", "node_modules", "generated", "coverage", ".next", "target"];
    if dirs.iter().any(|d| SKIP_DIRS.contains(d)) { return false; }
    let name = segs.last().copied().unwrap_or("");
    const SKIP_PARTS: &[&str] = &[".test.", ".spec.", ".stories.", ".min.", ".generated.", ".snap", ".d.ts", ".lock", "-lock.", "_test.go", "_test.rs"];
    if SKIP_PARTS.iter().any(|k| name.contains(k)) { return false; }
    !(name.starts_with("test_") && name.ends_with(".py"))
}

/// The files to describe first: most depended-on, then richest in symbols, then by path so the choice is stable; none
/// that already hold a fresh essay, none a path rule rejects, none with nothing to say (no symbols and nobody imports
/// it). `budget` bounds the call count. Pure - tested.
pub fn pick_for_description(cands: &[DescribeCandidate], budget: usize) -> Vec<DescribeCandidate> {
    let mut v: Vec<DescribeCandidate> = cands.iter().filter(|c| !c.fresh && is_describable_path(&c.path) && (c.symbols > 0 || c.dependents > 0)).cloned().collect();
    v.sort_by(|a, b| b.dependents.cmp(&a.dependents).then(b.symbols.cmp(&a.symbols)).then(a.path.cmp(&b.path)));
    v.truncate(budget);
    v
}

/// Every file of the origin the describer knows facts for, with its staleness resolved against the file node's current hash.
pub fn describe_candidates(db: &Connection, origin: &str) -> Result<Vec<DescribeCandidate>, String> {
    let rows = crate::db_json::query_rows(
        db,
        "SELECT d.path AS path, d.facts_json AS facts, d.summary_md AS summary, d.content_hash AS hash, n.content_hash AS cur \
         FROM code_descriptions d LEFT JOIN code_nodes n ON n.origin = d.origin AND n.path = d.path AND n.type = 'file' WHERE d.origin = ?",
        &[json!(origin)],
    )?;
    Ok(rows.as_array().cloned().unwrap_or_default().iter().map(|r| {
        let facts_raw = str_qq(r.get("facts"));
        let facts: Value = serde_json::from_str(&facts_raw).unwrap_or(Value::Null);
        let has_essay = r.get("summary").and_then(Value::as_str).map_or(false, |t| !t.trim().is_empty());
        let same = nz(r.get("cur")).is_some() && r.get("hash") == r.get("cur");
        DescribeCandidate {
            path: str_qq(r.get("path")),
            dependents: facts.get("n_dependents").and_then(Value::as_i64).unwrap_or(0),
            symbols: facts.get("n_symbols").and_then(Value::as_i64).unwrap_or(0),
            role: facts.get("role").and_then(Value::as_str).unwrap_or("").to_string(),
            facts_chars: facts_raw.len(),
            fresh: has_essay && same,
        }
    }).collect())
}

/// HOW MANY FILES HAVE PROSE - the number onboarding reports (it used to report none, so an empty column read as "not
/// implemented"). `describable` is the denominator that matters: the files a sentence is worth writing for.
pub fn describeCoverage(db: &Connection, origin: &str) -> Result<Value, String> {
    let cands = describe_candidates(db, origin)?;
    let files = crate::db_json::query_rows(db, "SELECT COUNT(*) AS n FROM code_nodes WHERE origin = ? AND type = 'file'", &[json!(origin)])?
        .as_array().and_then(|a| a.first()).and_then(|r| r.get("n")).and_then(Value::as_i64).unwrap_or(0);
    let worth = |c: &&DescribeCandidate| is_describable_path(&c.path) && (c.symbols > 0 || c.dependents > 0);
    let describable = cands.iter().filter(worth).count() as i64;
    let with_prose = cands.iter().filter(worth).filter(|c| c.fresh).count() as i64;
    let stale = crate::db_json::query_rows(db, "SELECT COUNT(*) AS n FROM code_descriptions d JOIN code_nodes n ON n.origin = d.origin AND n.path = d.path AND n.type = 'file' \
        WHERE d.origin = ? AND d.summary_md IS NOT NULL AND TRIM(d.summary_md) != '' AND d.content_hash IS NOT n.content_hash", &[json!(origin)])?
        .as_array().and_then(|a| a.first()).and_then(|r| r.get("n")).and_then(Value::as_i64).unwrap_or(0);
    Ok(coverage_json(origin, files, cands.len() as i64, describable, with_prose, stale))
}

/// The coverage object and its sentence. Pure - tested.
pub fn coverage_json(origin: &str, files: i64, facts: i64, describable: i64, with_prose: i64, stale: i64) -> Value {
    let pct = if describable > 0 { (with_prose as f64 * 1000.0 / describable as f64).round() / 10.0 } else { 0.0 };
    let note = if describable == 0 {
        "no file has facts to describe yet - the index has not gathered them".to_string()
    } else if with_prose == 0 {
        format!("0 of {describable} describable files have a description (0%). Onboarding gathers facts, never prose, unless asked: POST /api/v1/codebase/describe/plan prices the first files before any call is made.")
    } else if with_prose < describable {
        format!("{with_prose} of {describable} describable files have a description ({pct}%){}.", if stale > 0 { format!("; {stale} are out of date with their file") } else { String::new() })
    } else {
        format!("all {describable} describable files have a description{}.", if stale > 0 { format!("; {stale} are out of date with their file") } else { String::new() })
    };
    json!({ "origin": origin, "files": files, "facts": facts, "describable": describable, "with_prose": with_prose, "stale": stale, "pct_of_describable": pct, "note": note })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(path: &str, dependents: i64, symbols: i64, fresh: bool) -> DescribeCandidate {
        DescribeCandidate { path: path.into(), dependents, symbols, role: "logic".into(), facts_chars: 400, fresh }
    }

    #[test]
    fn tests_fixtures_and_build_output_are_not_worth_a_call() {
        for no in ["src/__tests__/a.js", "src/a.test.ts", "tests/b.py", "pkg/test_x.py", "dist/app.js", "vendor/lib/x.go", "web/a.stories.tsx", "types/x.d.ts", "fixtures/fx-react/src/App.tsx", "a/b_test.go", "static/app.min.js", "node_modules/x/i.js", "src\\__mocks__\\m.js"] {
            assert!(!is_describable_path(no), "{no}");
        }
        for yes in ["src/cart.ts", "packages/react-reconciler/src/ReactChildFiber.js", "src/contest.ts", "crates/core/src/extract.rs", "app/testing_tools.py"] {
            assert!(is_describable_path(yes), "{yes}");
        }
    }

    #[test]
    fn the_budget_goes_to_the_most_depended_on_files_first_and_never_to_a_fresh_one() {
        let v = vec![cand("a.ts", 1, 3, false), cand("hub.ts", 40, 9, false), cand("done.ts", 90, 9, true), cand("t/x.test.ts", 70, 5, false), cand("empty.ts", 0, 0, false), cand("b.ts", 1, 7, false), cand("c.ts", 1, 7, false)];
        let got: Vec<String> = pick_for_description(&v, 3).into_iter().map(|c| c.path).collect();
        assert_eq!(got, vec!["hub.ts", "b.ts", "c.ts"], "hub first; then the richer of the one-importer files, ties by path; the fresh, the test and the empty one never");
        assert_eq!(pick_for_description(&v, 0).len(), 0, "a budget of 0 is a plan with no calls");
        assert_eq!(pick_for_description(&v, 99).len(), 4);
    }

    #[test]
    fn coverage_says_zero_plainly_and_says_what_to_do() {
        let c = coverage_json("atlas", 4579, 2109, 1800, 0, 0);
        assert_eq!(c["pct_of_describable"], json!(0.0));
        assert!(c["note"].as_str().unwrap().starts_with("0 of 1800 describable files have a description (0%)"), "{c}");
        assert!(c["note"].as_str().unwrap().contains("describe/plan"));
        let part = coverage_json("atlas", 10, 10, 8, 2, 1);
        assert_eq!(part["pct_of_describable"], json!(25.0));
        assert!(part["note"].as_str().unwrap().contains("1 are out of date"), "{part}");
        assert!(coverage_json("atlas", 3, 3, 3, 3, 0)["note"].as_str().unwrap().starts_with("all 3"));
        assert!(coverage_json("atlas", 0, 0, 0, 0, 0)["note"].as_str().unwrap().contains("not gathered"));
    }

    #[test]
    fn coverage_counts_only_essays_written_for_the_file_as_it_is_now() {
        let c = seeded();
        c.execute("INSERT INTO code_nodes (id,origin,name,path,type,content_hash) VALUES ('f2','atlas','util','src/util.js','file','hashU'), ('f3','atlas','t','src/a.test.js','file','hashT')", []).unwrap();
        c.execute_batch(
            "INSERT INTO code_descriptions (origin,path,facts_json,content_hash,summary_md) VALUES \
             ('atlas','src/components/Widget.jsx','{\"n_symbols\":2,\"n_dependents\":1,\"role\":\"ui\"}','hashW','It renders a widget.'), \
             ('atlas','src/util.js','{\"n_symbols\":1,\"n_dependents\":0}','OLD','An old essay.'), \
             ('atlas','src/a.test.js','{\"n_symbols\":4,\"n_dependents\":0}','hashT','A test.');").unwrap();
        let cov = describeCoverage(&c, "atlas").unwrap();
        assert_eq!(cov["files"], json!(3));
        assert_eq!(cov["describable"], json!(2), "the test file is not worth a sentence");
        assert_eq!(cov["with_prose"], json!(1), "util's essay was written for another version of the file");
        assert_eq!(cov["stale"], json!(1));
        let c2 = describe_candidates(&c, "atlas").unwrap();
        assert_eq!(pick_for_description(&c2, 10).iter().map(|x| x.path.as_str()).collect::<Vec<_>>(), vec!["src/util.js"], "the stale one is next in line");
    }

    #[test]
    fn a_pinned_seat_rides_into_the_ai_call_and_nothing_else_does() {
        let facts = json!({ "role": "ui", "symbols": [{ "name": "Widget", "kind": "component" }], "imports": ["react"], "dependents": ["App.tsx"] });
        let pinned = build_ai_input(&json!("src/W.jsx"), &facts, Some("cli:codex"));
        assert_eq!(pinned["model_id"], json!("cli:codex"));
        assert_eq!(pinned["tier"], json!("low"));
        for no in [None, Some(""), Some("cli:"), Some("openrouter/x"), Some("codex")] {
            assert!(build_ai_input(&json!("src/W.jsx"), &facts, no).get("model_id").is_none(), "{no:?}");
        }
        assert_eq!(pinned["input_kv"][1], json!({ "key": "role", "value": "ui" }));
    }

    fn seeded() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain;").unwrap();
        c.execute_batch(
            "CREATE TABLE code_nodes (id TEXT, origin TEXT, name TEXT, path TEXT, type TEXT, signature TEXT, content_hash TEXT); \
             CREATE TABLE code_edges (origin TEXT, from_node TEXT, to_node TEXT, type TEXT, kind TEXT); \
             CREATE TABLE code_decorators (origin TEXT, path TEXT, name TEXT, kind TEXT, role TEXT, signature TEXT, summary TEXT, line INTEGER); \
             CREATE TABLE code_descriptions (origin TEXT, path TEXT, facts_json TEXT, content_hash TEXT, summary_md TEXT, model_id TEXT, generated_at TEXT, gathered_at TEXT, PRIMARY KEY (origin, path)); \
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('now')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) ); \
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) ); \
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) ); \
             INSERT INTO code_nodes (id,origin,name,path,type,content_hash) VALUES ('f1','atlas','Widget','src/components/Widget.jsx','file','hashW'); \
             INSERT INTO code_decorators (origin,path,name,kind,role,signature,summary,line) VALUES \
               ('atlas','src/components/Widget.jsx','Widget','component','ui','() => JSX','renders',1);",
        ).unwrap();
        c
    }

    #[test]
    fn path_is_required() {
        let c = seeded();
        assert!(codeDescribe(&json!({}), &c).unwrap_err().contains("path is required"));
    }

    #[test]
    fn gathers_facts_then_reaches_the_parity_deferred_aicall_boundary_with_null_summary() {
        // proves the deterministic surface (cache-miss → gather → persist → prompt-ensure) ran
        // cleanly and the swallowed aiCall Err (not an earlier bug) is why summary_md is null.
        let c = seeded();
        let r = codeDescribe(&json!({ "path": "src/components/Widget.jsx" }), &c).unwrap();
        assert_eq!(r["path"], json!("src/components/Widget.jsx"));
        assert_eq!(r["origin"], json!("atlas"));
        assert_eq!(r["cached"], json!(false));
        assert_eq!(r["summary_md"], Value::Null);
        assert_eq!(r["model_id"], Value::Null);
        assert_eq!(r["facts"]["role"], json!("ui"));
        // facts were persisted (gather ran) and the prompt template was ensured
        let hash: String = c.query_row("SELECT content_hash FROM code_descriptions WHERE path='src/components/Widget.jsx'", [], |row| row.get(0)).unwrap();
        assert_eq!(hash, "hashW");
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'code.describe.v1'", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 1);
        // no cached essay landed (the deferred call never produced one)
        let summary: Option<String> = c.query_row("SELECT summary_md FROM code_descriptions WHERE path='src/components/Widget.jsx'", [], |row| row.get(0)).unwrap();
        assert_eq!(summary, None);
    }

    #[test]
    fn fresh_cached_essay_short_circuits_before_any_aicall() {
        let c = seeded();
        c.execute(
            "INSERT INTO code_descriptions (origin, path, facts_json, content_hash, summary_md, model_id) VALUES ('atlas','src/components/Widget.jsx','{\"role\":\"ui\"}','hashW','It renders a widget.','low-model')",
            [],
        ).unwrap();
        let r = codeDescribe(&json!({ "path": "src/components/Widget.jsx" }), &c).unwrap();
        assert_eq!(r["cached"], json!(true));
        assert_eq!(r["summary_md"], json!("It renders a widget."));
        assert_eq!(r["model_id"], json!("low-model"));
        // no prompt template was ensured — cache hit short-circuited before that step
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'code.describe.v1'", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn stale_content_hash_regathers_and_drops_the_cached_essay() {
        let c = seeded();
        c.execute(
            "INSERT INTO code_descriptions (origin, path, facts_json, content_hash, summary_md, model_id) VALUES ('atlas','src/components/Widget.jsx','{\"role\":\"stale\"}','oldhash','stale essay','old-model')",
            [],
        ).unwrap();
        let r = codeDescribe(&json!({ "path": "src/components/Widget.jsx" }), &c).unwrap();
        assert_eq!(r["cached"], json!(false));
        assert_eq!(r["facts"]["role"], json!("ui")); // regathered, not the stale JSON
        let (summary, hash): (Option<String>, String) = c.query_row(
            "SELECT summary_md, content_hash FROM code_descriptions WHERE path='src/components/Widget.jsx'", [], |row| Ok((row.get(0)?, row.get(1)?)),
        ).unwrap();
        assert_eq!(summary, None); // essay dropped on regather
        assert_eq!(hash, "hashW");
    }

    #[test]
    fn force_bypasses_a_fresh_cached_essay() {
        let c = seeded();
        c.execute(
            "INSERT INTO code_descriptions (origin, path, facts_json, content_hash, summary_md, model_id) VALUES ('atlas','src/components/Widget.jsx','{\"role\":\"ui\"}','hashW','It renders a widget.','low-model')",
            [],
        ).unwrap();
        let r = codeDescribe(&json!({ "path": "src/components/Widget.jsx", "force": true }), &c).unwrap();
        assert_eq!(r["cached"], json!(false));
        assert_eq!(r["summary_md"], Value::Null); // deferred aiCall swallowed → no new essay
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'code.describe.v1'", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 1); // prompt WAS ensured this time (force skipped the cache short-circuit)
    }

    // ── formatting helpers (deterministic prompt/context assembly) ────────────────────────
    #[test]
    fn fmt_syms_uses_summary_then_kind_then_bare_and_defaults_to_none() {
        assert_eq!(fmt_syms(None), "(none)");
        assert_eq!(fmt_syms(Some(&json!([]))), "(none)");
        let syms = json!([
            { "name": "Widget", "signature": "()", "summary": "renders" },
            { "name": "helper", "signature": "(x)", "kind": "function" },
            { "kind": "const" },
        ]);
        // verified via node repro of the real JS fmtSyms: the third symbol has no
        // name/signature/summary but DOES have kind:"const", which is truthy, so JS
        // appends " (const)" — the original expectation of a bare "?" was wrong.
        assert_eq!(fmt_syms(Some(&syms)), "Widget() — renders; helper(x) (function); ? (const)");
    }

    #[test]
    fn join_or_none_caps_at_30_and_defaults() {
        assert_eq!(join_or_none(None), "(none)");
        assert_eq!(join_or_none(Some(&json!([]))), "(none)");
        assert_eq!(join_or_none(Some(&json!(["a", "b"]))), "a, b");
    }
}

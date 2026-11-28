//! card-grounding-collect.rs — twin of skills/system/card-grounding-collect.mjs. NAMES ARE
//! CONTRACT: `cardGroundingCollect` (default).
//!
//! Assembles the grounding context for the AI card-body-enrichment call: the card's own facts
//! (title/why/kind/size/priority/risk/complexity/score/parent_slug), a capped git-evidence list,
//! and up to 3 "similar" already-rich cards (same kind+size, priority within +-1, most keys wins).
//!
//! BESPOKE-NATIVE (playbook's "not conditionally avoidable" rule, card-commits.rs /
//! code-git-init.rs precedent): the private `collectGitEvidence` helper is called
//! UNCONDITIONALLY on every success path (no input flag gates it) and shells out to `git log`
//! twice — so this whole unit is exercised only by the native tests below, real correct code, NO
//! conform-db fixture and NO frontier entry.
//!
//! `execSync(cmd, {...})` in JS builds shell-interpreted command strings. Probe 1
//! (`--grep="${safe}" -n <cap>`) is a single `git` invocation with its own `-n` cap, ported as
//! `std::process::Command` with an explicit args array (`--grep=<safe>` — same
//! post-shell-quote-stripping token as card-commits.rs). Probe 2
//! (`git log --all --oneline -- "*<safe>*" | head -<remaining>`) is a SHELL PIPE in JS — no Rust
//! shell is involved here, so this twin runs the UNLIMITED `git log` itself and reproduces
//! `head -N` by taking the first `remaining` RAW lines of stdout (before the blank-line filter
//! `run()` applies afterward) — exactly what reaches JS on the far side of the pipe.
//!
//! `String(slug).replace(/[^A-Z0-9-]/g, '')` — case-SENSITIVE, keeps only uppercase/digit/dash
//! (NO underscore, unlike card-commits.rs's sibling regex which also keeps `_`).
//!
//! Evidence accumulation mirrors the JS byte-for-byte: after each pushed line, recompute
//! `lines.join('\n').length` and stop once it exceeds `max_evidence_bytes` (the line that tips it
//! over stays in the list — not trimmed back out). Probe 2 additionally dedupes via `.includes`
//! (linear scan, not a Set) before pushing. `.length` here is JS UTF-16 code-unit count; this
//! twin uses byte length (git subject lines are effectively ASCII in this substrate's slugs, and
//! the value is only a loop-termination heuristic here — never fixture/byte compared, since this
//! unit carries no conform-db case).
//!
//! `max_evidence_bytes` default: `{ max_evidence_bytes = 5000 } = input ?? {}` — the JS default
//! applies only when the key is MISSING (`undefined`), not when it is explicitly `null`. Modeled
//! as: key absent -> 5000.0; key present -> `js_number(Some(v))` (so an explicit `null` becomes
//! `0.0`, matching JS's `ToNumber(null) === 0` in the `length > maxBytes` comparison, and any
//! non-coercible value becomes `NaN`, which Rust's `f64 >` also always evaluates false for, same
//! as JS `> NaN`).
//!
//! `card.body_json` parse: `JSON.parse` failure -> `null` (JS `catch {}` leaves the prior `null`);
//! a still-string result (double-encoded legacy rows) gets ONE more parse attempt, and on ITS
//! failure the string itself is kept (JS reassigns only on success); anything that isn't a plain
//! object (null/array/primitive/still-a-string) collapses to `{}`. `current_why` only takes
//! `body.why` when it is literally a JSON string (`typeof body.why === 'string'`). `kind`/`size` =
//! `body.kind ?? null` / `body.size ?? null` (JS `??`, so an explicit JSON `null` stays `null`
//! same as a missing key).
//!
//! Similar-cards query only runs when `kind && size` are both TRUTHY (an empty-string kind/size
//! also skips it, not just null/undefined) — `priority = card.priority ?? 3` (nullable INTEGER
//! column; disclosed narrowing: this twin reads it via `as_i64` directly rather than full
//! `js_number` ToNumber coercion, since `priority` is always a plain SQLite INTEGER in every real
//! row). Rows are scored by parsed-body key count (rows whose body doesn't parse to a plain
//! object score 0 and get filtered out by the `keyCount > 3` gate), ties broken by original row
//! order (stable sort — `sort_by` in Rust is stable, same as JS `Array.prototype.sort` since
//! ES2019).
//!
//! `slug` missing/falsy -> `err.required` -> `"card-grounding-collect: slug is required"`. Card
//! not found -> `err.notFound` -> `"card-grounding-collect: card '<slug>' not found"`. Modeled as
//! `Err(String)`. `context.db` missing is NOT modeled: `db: &Connection` is a required,
//! non-nullable reference here — same "guard-throws are not exercised" convention used by every
//! sibling twin.
// QUALITY: bespoke-native (real git subprocess on every success path) — no fixture/frontier
// entry; correctness proven by the native tests below against a real temp-dir git repo plus an
// in-memory cards table.

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand
fn js_string(v: &Value) -> String { match v { Value::Null=>"null".into(), Value::String(s)=>s.clone(), Value::Number(n)=>n.to_string(), Value::Bool(b)=>b.to_string(), Value::Array(a)=>a.iter().map(js_string).collect::<Vec<_>>().join(","), Value::Object(_)=>"[object Object]".into() } } // = JS String(v)
fn js_number(v: Option<&Value>) -> f64 { match v { None=>f64::NAN, Some(Value::Null)=>0.0, Some(Value::Bool(b))=>if *b {1.0} else {0.0}, Some(Value::Number(n))=>n.as_f64().unwrap_or(f64::NAN), Some(Value::String(s))=>{let t=s.trim(); if t.is_empty(){0.0}else{t.parse().unwrap_or(f64::NAN)}}, Some(_)=>f64::NAN } } // = JS Number(v)

const MAX_EVIDENCE_LINES: i64 = 60; // hard cap to prevent prompt blowout
const MAX_SIMILAR_CARDS: usize = 3;

// roots.mjs: SUBSTRATE_ROOT = resolve($ANTILOKI_SUBSTRATE_ROOT) if set+truthy, else
// resolve(ENGINE_ROOT, '..') anchored at roots.mjs's own file location (antiloki-core's parent).
// CARGO_MANIFEST_DIR here is antiloki-core-rust; its parent is that same SUBSTRATE_ROOT on this
// checkout layout (card-commits.rs / auth-init.rs precedent).
fn substrate_root() -> PathBuf {
    if let Ok(v) = std::env::var("ANTILOKI_SUBSTRATE_ROOT") {
        if !v.is_empty() {
            return PathBuf::from(v);
        }
    }
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("antiloki-core-rust has a parent dir")
        .to_path_buf()
}

/// Probe 1: `git log --all --oneline --grep="<safe>" -n <cap>` — a single git invocation with
/// its own `-n` cap, no shell pipe involved. Any spawn failure or non-zero exit collapses to [].
fn probe_grep(safe: &str, cap: i64, cwd: &Path) -> Vec<String> {
    let out = crate::proc::cmd("git")
        .args(["log", "--all", "--oneline", &format!("--grep={safe}"), "-n", &cap.to_string()])
        .current_dir(cwd)
        .output();
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .split('\n')
            .filter(|l| !l.trim().is_empty())
            .map(|s| s.to_string())
            .collect(),
        _ => Vec::new(),
    }
}

/// Probe 2: JS `git log --all --oneline -- "*<safe>*" | head -<remaining>` — the JS side runs
/// this as ONE shell-interpreted string so the shell performs the pipe. This twin instead runs
/// the UNLIMITED `git log` and reproduces `head -N` itself: take the first `remaining` RAW lines
/// of stdout, THEN drop blank ones (same order as JS: pipe truncates first, `run()`'s
/// `.filter(l=>l.trim())` runs second on whatever the pipe handed it).
fn probe_path_head(safe: &str, remaining: i64, cwd: &Path) -> Vec<String> {
    let out = crate::proc::cmd("git")
        .args(["log", "--all", "--oneline", "--", &format!("*{safe}*")])
        .current_dir(cwd)
        .output();
    match out {
        Ok(o) if o.status.success() => {
            let text = String::from_utf8_lossy(&o.stdout).into_owned();
            text.split('\n')
                .take(remaining.max(0) as usize)
                .filter(|l| !l.trim().is_empty())
                .map(|s| s.to_string())
                .collect()
        }
        _ => Vec::new(),
    }
}

/// `function collectGitEvidence(slug, maxBytes)`
fn collect_git_evidence(slug_val: &Value, max_bytes: f64) -> Vec<String> {
    let safe: String = js_string(slug_val)
        .chars()
        .filter(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == '-')
        .collect();
    if safe.is_empty() {
        return Vec::new();
    }
    let root = substrate_root();
    let mut lines: Vec<String> = Vec::new();

    for line in probe_grep(&safe, MAX_EVIDENCE_LINES, &root) {
        lines.push(line);
        if lines.join("\n").len() as f64 > max_bytes {
            break;
        }
    }

    let remaining = MAX_EVIDENCE_LINES - lines.len() as i64;
    if remaining > 0 {
        for line in probe_path_head(&safe, remaining, &root) {
            if !lines.contains(&line) {
                lines.push(line);
            }
            if lines.join("\n").len() as f64 > max_bytes {
                break;
            }
        }
    }

    lines.into_iter().take(MAX_EVIDENCE_LINES as usize).collect()
}

/// `let body = null; try { body = card.body_json ? JSON.parse(card.body_json) : null; } catch {}
/// if (typeof body === 'string') { try { body = JSON.parse(body); } catch {} }
/// if (!body || typeof body !== 'object' || Array.isArray(body)) body = {};`
fn parse_body(body_json: Option<&str>) -> Value {
    let mut body: Value = Value::Null;
    if let Some(s) = body_json {
        if !s.is_empty() {
            if let Ok(v) = serde_json::from_str::<Value>(s) {
                body = v;
            }
            // else body stays Value::Null — JS `catch {}` leaves the prior `null`.
        }
    }
    if let Value::String(inner) = &body {
        if let Ok(v2) = serde_json::from_str::<Value>(inner) {
            body = v2;
        }
        // else body stays the (still-a-string) value — JS reassigns only on success.
    }
    match &body {
        Value::Object(_) => body,
        _ => json!({}),
    }
}

/// `export default function cardGroundingCollect(input, context)`
pub fn cardGroundingCollect(input: &Value, db: &Connection) -> Result<Value, String> {
    let slug_val = match input.get("slug").filter(|v| truthy(Some(v))) {
        Some(v) => v.clone(),
        None => return Err("card-grounding-collect: slug is required".into()),
    };
    let max_evidence_bytes: f64 = match input.get("max_evidence_bytes") {
        None => 5000.0,
        Some(v) => js_number(Some(v)),
    };

    let rows = crate::db_json::query_rows(
        db,
        "SELECT slug, title, body_json, risk, complexity, priority, score, parent_slug FROM cards WHERE slug = ?",
        &[slug_val.clone()],
    )?;
    let card = match rows.as_array().and_then(|a| a.first()) {
        Some(r) => r.clone(),
        None => return Err(format!("card-grounding-collect: card '{}' not found", js_string(&slug_val))),
    };

    let body = parse_body(card.get("body_json").and_then(Value::as_str));
    let current_why = body.get("why").and_then(Value::as_str).unwrap_or("").to_string();
    let kind = nz(body.get("kind")).cloned().unwrap_or(Value::Null);
    let size = nz(body.get("size")).cloned().unwrap_or(Value::Null);

    let git_evidence = collect_git_evidence(&slug_val, max_evidence_bytes);

    let mut similar_cards: Vec<Value> = Vec::new();
    if truthy(Some(&kind)) && truthy(Some(&size)) {
        let priority: i64 = nz(card.get("priority")).and_then(Value::as_i64).unwrap_or(3);
        let sim_rows = crate::db_json::query_rows(
            db,
            "SELECT slug, title, body_json FROM cards WHERE slug != ? \
             AND json_extract(body_json, '$.kind') = ? AND json_extract(body_json, '$.size') = ? \
             AND priority BETWEEN ? AND ? LIMIT 30",
            &[slug_val.clone(), kind.clone(), size.clone(), json!(priority - 1), json!(priority + 1)],
        )?;

        let mut scored: Vec<(Value, Value, i64)> = Vec::new(); // (row, parsed, keyCount)
        if let Some(arr) = sim_rows.as_array() {
            for r in arr {
                let parsed: Value = r
                    .get("body_json")
                    .and_then(Value::as_str)
                    .and_then(|s| serde_json::from_str::<Value>(s).ok())
                    .unwrap_or(Value::Null);
                let key_count = match &parsed {
                    Value::Object(m) => m.len() as i64,
                    _ => 0,
                };
                scored.push((r.clone(), parsed, key_count));
            }
        }
        scored.retain(|(_, _, kc)| *kc > 3);
        scored.sort_by(|a, b| b.2.cmp(&a.2)); // stable: keyCount desc, ties keep insertion order
        scored.truncate(MAX_SIMILAR_CARDS);

        for (row, parsed, _) in scored {
            similar_cards.push(json!({
                "slug": row.get("slug").cloned().unwrap_or(Value::Null),
                "title": row.get("title").cloned().unwrap_or(Value::Null),
                "body": parsed,
            }));
        }
    }

    let mut out = Map::new();
    out.insert("slug".into(), card.get("slug").cloned().unwrap_or(Value::Null));
    out.insert("title".into(), card.get("title").cloned().unwrap_or(Value::Null));
    out.insert("current_why".into(), json!(current_why));
    out.insert("kind".into(), kind);
    out.insert("size".into(), size);
    out.insert("priority".into(), card.get("priority").cloned().unwrap_or(Value::Null));
    out.insert("risk".into(), card.get("risk").cloned().unwrap_or(Value::Null));
    out.insert("complexity".into(), card.get("complexity").cloned().unwrap_or(Value::Null));
    out.insert("score".into(), card.get("score").cloned().unwrap_or(Value::Null));
    out.insert("parent_slug".into(), card.get("parent_slug").cloned().unwrap_or(Value::Null));
    out.insert("git_evidence".into(), json!(git_evidence));
    out.insert("similar_cards".into(), json!(similar_cards));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    // SAFETY: ANTILOKI_SUBSTRATE_ROOT is a process-global env var read by many twins' tests
    // across the crate — shared crate-wide lock (test_env_lock.rs), a local mutex would not
    // prevent a race with OTHER files' tests over the same real env var.
    fn with_substrate_root<T>(root: &str, f: impl FnOnce() -> T) -> T {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("ANTILOKI_SUBSTRATE_ROOT", root);
        }
        let result = f();
        unsafe {
            std::env::remove_var("ANTILOKI_SUBSTRATE_ROOT");
        }
        result
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-card-grounding-collect-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn git(args: &[&str], cwd: &Path) {
        let status = crate::proc::cmd("git").args(args).current_dir(cwd).status().unwrap();
        assert!(status.success(), "git {args:?} failed in {cwd:?}");
    }

    fn init_repo(dir: &Path) {
        git(&["init", "-q"], dir);
        git(&["config", "user.email", "test@example.com"], dir);
        git(&["config", "user.name", "Test"], dir);
    }

    fn commit(dir: &Path, file: &str, contents: &str, message: &str) {
        if let Some(parent) = Path::new(file).parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(dir.join(parent)).unwrap();
            }
        }
        fs::write(dir.join(file), contents).unwrap();
        git(&["add", "-A"], dir);
        git(&["commit", "-m", message], dir);
    }

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE cards (slug TEXT PRIMARY KEY, title TEXT, body_json TEXT, \
             risk INTEGER, complexity INTEGER, priority INTEGER, score REAL, parent_slug TEXT)",
        )
        .unwrap();
        c
    }
    fn ins_card(c: &Connection, slug: &str, body_json: Option<&str>, priority: Option<i64>) {
        c.execute(
            "INSERT INTO cards (slug, title, body_json, priority) VALUES (?1, 't', ?2, ?3)",
            rusqlite::params![slug, body_json, priority],
        )
        .unwrap();
    }

    #[test]
    fn missing_slug_errors() {
        let r = cardGroundingCollect(&json!({}), &db());
        assert_eq!(r, Err("card-grounding-collect: slug is required".to_string()));
    }

    #[test]
    fn unknown_card_errors() {
        let r = cardGroundingCollect(&json!({ "slug": "NOPE-1" }), &db());
        assert_eq!(r, Err("card-grounding-collect: card 'NOPE-1' not found".to_string()));
    }

    #[test]
    fn basic_shape_defaults_and_key_order_no_git_repo() {
        let dir = temp_dir("no-repo"); // real dir, deliberately NOT a git repo
        let c = db();
        ins_card(&c, "FEAT-BASIC-1", Some(r#"{"why":"because"}"#), Some(2));
        let root = dir.to_string_lossy().into_owned();
        let r = with_substrate_root(&root, || cardGroundingCollect(&json!({ "slug": "FEAT-BASIC-1" }), &c).unwrap());
        assert_eq!(r["slug"], json!("FEAT-BASIC-1"));
        assert_eq!(r["current_why"], json!("because"));
        assert_eq!(r["kind"], Value::Null);
        assert_eq!(r["size"], Value::Null);
        assert_eq!(r["priority"], json!(2));
        assert_eq!(r["git_evidence"], json!([])); // not a git repo → probes degrade to []
        assert_eq!(r["similar_cards"], json!([]));
        let keys: Vec<&String> = r.as_object().unwrap().keys().collect();
        assert_eq!(
            keys,
            ["slug", "title", "current_why", "kind", "size", "priority", "risk", "complexity", "score", "parent_slug", "git_evidence", "similar_cards"]
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn double_encoded_body_json_recovered() {
        let dir = temp_dir("double-encoded");
        let c = db();
        let inner = serde_json::to_string(&json!({ "kind": "feature", "size": "m", "why": "deep" })).unwrap();
        let doubled = serde_json::to_string(&Value::String(inner)).unwrap();
        ins_card(&c, "FEAT-DBL-1", Some(&doubled), None);
        let root = dir.to_string_lossy().into_owned();
        let r = with_substrate_root(&root, || cardGroundingCollect(&json!({ "slug": "FEAT-DBL-1" }), &c).unwrap());
        assert_eq!(r["kind"], json!("feature"));
        assert_eq!(r["size"], json!("m"));
        assert_eq!(r["current_why"], json!("deep"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn malformed_and_null_body_json_collapse_to_empty_object() {
        let dir = temp_dir("malformed-null");
        let c = db();
        ins_card(&c, "FEAT-BAD-1", Some("{not json"), None);
        ins_card(&c, "FEAT-NULL-1", None, None);
        let root = dir.to_string_lossy().into_owned();
        let r1 = with_substrate_root(&root, || cardGroundingCollect(&json!({ "slug": "FEAT-BAD-1" }), &c).unwrap());
        assert_eq!(r1["current_why"], json!(""));
        assert_eq!(r1["kind"], Value::Null);
        let r2 = with_substrate_root(&root, || cardGroundingCollect(&json!({ "slug": "FEAT-NULL-1" }), &c).unwrap());
        assert_eq!(r2["current_why"], json!(""));
        assert_eq!(r2["kind"], Value::Null);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn similar_cards_scored_sorted_capped_and_self_excluded() {
        let dir = temp_dir("similar");
        let c = db();
        ins_card(&c, "FEAT-SIM-SELF", Some(r#"{"kind":"feature","size":"m","why":"x"}"#), Some(3));
        ins_card(&c, "FEAT-SIM-THIN", Some(r#"{"kind":"feature","size":"m"}"#), Some(3)); // 2 keys, filtered
        ins_card(&c, "FEAT-SIM-RICH", Some(r#"{"kind":"feature","size":"m","why":"a","notes":"b","extra":"c"}"#), Some(4)); // 5 keys
        ins_card(&c, "FEAT-SIM-MID", Some(r#"{"kind":"feature","size":"m","why":"a","notes":"b"}"#), Some(2)); // 4 keys
        ins_card(&c, "FEAT-SIM-TIE", Some(r#"{"kind":"feature","size":"m","why":"a","notes":"z"}"#), Some(3)); // 4 keys, tie, inserted after MID
        ins_card(&c, "FEAT-SIM-WRONGKIND", Some(r#"{"kind":"bug","size":"m","why":"a","notes":"b","x":"c"}"#), Some(3));
        ins_card(&c, "FEAT-SIM-FAR", Some(r#"{"kind":"feature","size":"m","why":"a","notes":"b","x":"c"}"#), Some(5)); // out of [2,4]
        ins_card(&c, "FEAT-SIM-EXTRA", Some(r#"{"kind":"feature","size":"m","why":"a","notes":"b","x":"c","y":"d"}"#), Some(2)); // 6 keys

        let root = dir.to_string_lossy().into_owned();
        let r = with_substrate_root(&root, || cardGroundingCollect(&json!({ "slug": "FEAT-SIM-SELF" }), &c).unwrap());
        let arr = r["similar_cards"].as_array().unwrap();
        assert_eq!(arr.len(), 3);
        let slugs: Vec<&str> = arr.iter().map(|s| s["slug"].as_str().unwrap()).collect();
        assert_eq!(slugs, vec!["FEAT-SIM-EXTRA", "FEAT-SIM-RICH", "FEAT-SIM-MID"]);
        assert_eq!(
            arr[2],
            json!({ "slug": "FEAT-SIM-MID", "title": "t", "body": { "kind": "feature", "size": "m", "why": "a", "notes": "b" } })
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn similar_cards_skipped_when_size_is_empty_string() {
        let dir = temp_dir("empty-size");
        let c = db();
        ins_card(&c, "FEAT-EMPTYSIZE", Some(r#"{"kind":"feature","size":""}"#), None);
        let root = dir.to_string_lossy().into_owned();
        let r = with_substrate_root(&root, || cardGroundingCollect(&json!({ "slug": "FEAT-EMPTYSIZE" }), &c).unwrap());
        assert_eq!(r["similar_cards"], json!([]));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn git_evidence_collected_from_real_repo_message_and_path_probes_deduped() {
        let dir = temp_dir("git-evidence");
        init_repo(&dir);
        commit(&dir, "a.txt", "1", "FEAT-GROUND-1: initial commit");
        // matches via PATH probe only (slug appears in a path component, not the message)
        commit(&dir, "src/FEAT-GROUND-1/module.txt", "2", "unrelated path-only commit");
        // matches both probes on message text mentioning the slug
        commit(&dir, "b.txt", "3", "touches FEAT-GROUND-1 in prose");
        // no match at all
        commit(&dir, "c.txt", "4", "OTHER-CARD unrelated");

        let c = db();
        ins_card(&c, "FEAT-GROUND-1", Some("{}"), None);
        let root = dir.to_string_lossy().into_owned();
        let r = with_substrate_root(&root, || cardGroundingCollect(&json!({ "slug": "FEAT-GROUND-1" }), &c).unwrap());
        let evidence = r["git_evidence"].as_array().unwrap();
        assert_eq!(evidence.len(), 3, "3 distinct matching commits, deduped across both probes: {r}");
        let joined: Vec<&str> = evidence.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(joined.iter().any(|l| l.contains("touches FEAT-GROUND-1 in prose")));
        assert!(joined.iter().any(|l| l.contains("unrelated path-only commit")));
        assert!(joined.iter().any(|l| l.contains("FEAT-GROUND-1: initial commit")));
        assert!(!joined.iter().any(|l| l.contains("OTHER-CARD unrelated")));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn slug_that_sanitizes_to_empty_returns_empty_evidence_without_touching_git() {
        let dir = temp_dir("empty-safe");
        let c = db();
        ins_card(&c, "abc", Some("{}"), None); // lowercase-only slug: [^A-Z0-9-] strips it all
        let root = dir.to_string_lossy().into_owned();
        let r = with_substrate_root(&root, || cardGroundingCollect(&json!({ "slug": "abc" }), &c).unwrap());
        assert_eq!(r["git_evidence"], json!([]));
        let _ = fs::remove_dir_all(&dir);
    }
}

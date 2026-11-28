//! qa-swarm-coverage.rs — twin of skills/agents/qa/qa-swarm-coverage.mjs (db lane; default writes a
//! verdict row on the swarm path). NAMES ARE CONTRACT: `qaSwarmCoverage` (default). Reuses the
//! deterministic coverage primitives from qa-plan-coverage (specCoversPattern / fileMatches / safeArr)
//! and the same hand-rolled GREP_RE matcher (regex crate has no backrefs).
// QUALITY: deterministic acceptance-coverage referee for the SWARM path — grep acceptances matched
// against authored swarm_blocks.content; file-exists + dynamic commands deferred downstream.
use rusqlite::Connection;
use serde_json::{json, Map, Value};

// ── standard coercion helpers ───────────────────────────────────────────────
fn str_qq(v: Option<&Value>) -> String {
    // JS String(v ?? '')
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
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
fn json_stringify(v: &Value) -> String { serde_json::to_string(v).unwrap_or_else(|_| "null".into()) }
fn is_arr(o: &Option<Value>) -> bool { matches!(o, Some(Value::Array(_))) }

// GREP_RE = /\bgrep\b[^\n]*?(['"])([\s\S]*?)\1\s+(\S+)/  — hand-rolled (regex crate has no backrefs).
// Leftmost-first, lazy quantifiers. Returns (pattern = g2, path = g3) of the first completing match.
fn is_word_byte(b: u8) -> bool { b.is_ascii_alphanumeric() || b == b'_' }
fn is_js_space(b: u8) -> bool { matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0C | 0x0B) }

fn match_ws_nonws(bytes: &[u8], start: usize) -> Option<(usize, usize)> {
    // \s+\S+ starting at `start`; returns the (start, end) span of the \S+ run.
    let len = bytes.len();
    let mut i = start;
    let ws0 = i;
    while i < len && is_js_space(bytes[i]) { i += 1; }
    if i == ws0 { return None; } // need at least one \s
    let ns0 = i;
    while i < len && !is_js_space(bytes[i]) { i += 1; }
    if i == ns0 { return None; } // need at least one \S
    Some((ns0, i))
}

fn grep_re_exec(cmd: &str) -> Option<(String, String)> {
    let bytes = cmd.as_bytes();
    let len = bytes.len();
    let mut start = 0usize;
    while start + 4 <= len {
        // \bgrep\b at `start`?
        let is_grep = &cmd[start..start + 4] == "grep"
            && (start == 0 || !is_word_byte(bytes[start - 1]))
            && (start + 4 == len || !is_word_byte(bytes[start + 4]));
        if is_grep {
            // [^\n]*? then opening quote — lazy: try smallest prefix first.
            let mut q = start + 4;
            loop {
                if q >= len { break; }
                let c = bytes[q];
                if c == b'\'' || c == b'"' {
                    // ([\s\S]*?)\1 — lazy content, closing quote == c, then \s+\S+.
                    let mut r = q + 1;
                    while r < len {
                        if bytes[r] == c {
                            if let Some((ns0, end)) = match_ws_nonws(bytes, r + 1) {
                                let group2 = cmd[q + 1..r].to_string();
                                let group3 = cmd[ns0..end].to_string();
                                return Some((group2, group3));
                            }
                        }
                        r += 1;
                    }
                }
                if c == b'\n' { break; } // [^\n]*? cannot cross a newline
                q += 1;
            }
        }
        start += 1;
    }
    None
}

// ── default: the swarm coverage referee ─────────────────────────────────────
pub fn qaSwarmCoverage(input: &Value, db: &Connection) -> Result<Value, String> {
    let contract_slug = input.get("contract_slug");

    let mut acceptance: Option<Value> = input.get("acceptance").cloned();
    let mut blocks: Option<Value> = input.get("blocks").cloned();

    if (!is_arr(&acceptance) || !is_arr(&blocks)) && truthy(contract_slug) {
        // db is always provided by the dispatcher/harness (the !db guard is a non-exercised throw).
        let rows = crate::db_json::query_rows(
            db,
            "SELECT acceptance_json FROM contracts WHERE slug = ?",
            &[contract_slug.cloned().unwrap_or(Value::Null)],
        )?;
        let c = rows.as_array().and_then(|a| a.first());
        let c = match c {
            Some(row) => row.clone(),
            None => return Err(format!("qa-swarm-coverage: contract '{}' not found", str_qq(contract_slug))),
        };
        if !is_arr(&acceptance) {
            let acc_col = c.get("acceptance_json").cloned().unwrap_or(Value::Null);
            acceptance = Some(crate::agents_qa_qa_plan_coverage::safeArr(&acc_col));
        }
        if !is_arr(&blocks) {
            let man_rows = crate::db_json::query_rows(
                db,
                "SELECT id FROM swarm_manifests WHERE contract_slug = ? AND superseded_by IS NULL ORDER BY version DESC, id DESC LIMIT 1",
                &[contract_slug.cloned().unwrap_or(Value::Null)],
            )?;
            let man = man_rows.as_array().and_then(|a| a.first());
            blocks = match man {
                Some(m) => {
                    let man_id = m.get("id").cloned().unwrap_or(Value::Null);
                    Some(crate::db_json::query_rows(
                        db,
                        "SELECT file, content FROM swarm_blocks WHERE manifest_id = ? ORDER BY id",
                        &[man_id],
                    )?)
                }
                None => Some(json!([])),
            };
        }
    }

    let acceptance: Vec<Value> = match acceptance { Some(Value::Array(a)) => a, _ => Vec::new() };
    let blocks: Vec<Value> = match blocks { Some(Value::Array(a)) => a, _ => Vec::new() };

    let mut issues: Vec<Value> = Vec::new();
    let mut checked: i64 = 0;
    let mut deferred: i64 = 0;

    for ac in &acceptance {
        let kind = str_qq(ac.get("kind")).to_lowercase();
        if kind == "test" {
            let cmd = str_qq(nz(ac.get("cmd")).or_else(|| nz(ac.get("command"))));
            match grep_re_exec(&cmd) {
                Some((pattern, p)) => {
                    checked += 1;
                    let p_val = Value::String(p.clone());
                    let pat_json = json_stringify(&Value::String(pattern.clone())); // JSON.stringify(pattern)
                    // blocks.find((a) => fileMatches({ file: a.file }, p))
                    let block = blocks.iter().find(|&a| {
                        let mut fm = Map::new();
                        fm.insert("file".into(), a.get("file").cloned().unwrap_or(Value::Null));
                        crate::agents_qa_qa_plan_coverage::fileMatches(&Value::Object(fm), &p_val)
                            .as_bool()
                            .unwrap_or(false)
                    });
                    match block {
                        None => {
                            let mut o = Map::new();
                            o.insert("kind".into(), json!("grep"));
                            o.insert("acceptance".into(), Value::String(cmd.clone()));
                            o.insert("file".into(), p_val);
                            o.insert("problem".into(), Value::String(format!(
                                "no swarm block authors '{}' (required by the grep acceptance for {}) \u{2014} the manifest must include a block for it",
                                p, pat_json
                            )));
                            issues.push(Value::Object(o));
                        }
                        Some(b) => {
                            let content = b.get("content").cloned().unwrap_or(Value::Null);
                            let covers = crate::agents_qa_qa_plan_coverage::specCoversPattern(
                                &content,
                                &Value::String(pattern.clone()),
                            )
                            .as_bool()
                            .unwrap_or(false);
                            if !covers {
                                let mut o = Map::new();
                                o.insert("kind".into(), json!("grep"));
                                o.insert("acceptance".into(), Value::String(cmd.clone()));
                                o.insert("file".into(), b.get("file").cloned().unwrap_or(Value::Null));
                                o.insert("problem".into(), Value::String(format!(
                                    "the authored content for '{}' does not contain {} (quote style normalized) \u{2014} the grep acceptance will fail; re-author the file to emit that literal",
                                    p, pat_json
                                )));
                                issues.push(Value::Object(o));
                            }
                        }
                    }
                }
                None => { deferred += 1; } // dynamic command (npm build/test/run) → verified downstream
            }
        } else {
            // file-exists → manifest + composition-check; other dynamic kinds → downstream.
            deferred += 1;
        }
    }

    let passed = issues.is_empty();

    // Driver step: record a verdict the state machine reads (review_mode='swarm-coverage').
    if truthy(contract_slug) {
        let det_issues: Vec<Value> = issues.iter().map(|i| {
            let mut o = Map::new();
            o.insert("severity".into(), json!("blocker"));
            o.insert("microtask_id".into(), json!(""));
            o.insert("file".into(), nz(i.get("file")).cloned().unwrap_or(json!("")));
            o.insert("problem".into(), i.get("problem").cloned().unwrap_or(Value::Null));
            o.insert("fix_hint".into(), json!(""));
            Value::Object(o)
        }).collect();

        let findings = if passed {
            format!("Swarm-coverage GREEN: all {} grep acceptance(s) present in the authored blocks ({} deferred to build+verify).", checked, deferred)
        } else {
            format!("Swarm-coverage RED: {} of {} grep acceptance(s) not present in the authored blocks ({} deferred). Re-author the named files.", det_issues.len(), checked, deferred)
        };

        let reviewer = nz(input.get("reviewer")).cloned().unwrap_or(json!("driver:auto"));
        let issues_json = serde_json::to_string(&det_issues).unwrap_or_else(|_| "[]".to_string()); // = JSON.stringify(detIssues)
        let (_changes, review_id) = crate::db_json::run_write(
            db,
            "INSERT INTO contract_qa_reviews (contract_slug, passed, verdict, findings_md, issues_json, reviewer, failure_target, review_mode)\n       VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            &[
                contract_slug.cloned().unwrap_or(Value::Null),
                json!(if passed { 1 } else { 0 }),
                json!(if passed { "pass" } else { "fail" }),
                Value::String(findings.clone()),
                Value::String(issues_json),
                reviewer,
                json!("microtasks"),
                json!("swarm-coverage"),
            ],
        )?;

        let mut out = Map::new();
        out.insert("contract_slug".into(), contract_slug.cloned().unwrap_or(Value::Null));
        out.insert("verdict".into(), json!(if passed { "pass" } else { "fail" }));
        out.insert("passed".into(), json!(passed));
        out.insert("review_mode".into(), json!("swarm-coverage"));
        out.insert("review_id".into(), json!(review_id));
        out.insert("issues".into(), Value::Array(det_issues));
        out.insert("findings_md".into(), Value::String(findings));
        out.insert("checked".into(), json!(checked));
        out.insert("deferred".into(), json!(deferred));
        return Ok(Value::Object(out));
    }

    let mut out = Map::new();
    out.insert("passed".into(), json!(passed));
    out.insert("issues".into(), Value::Array(issues));
    out.insert("checked".into(), json!(checked));
    out.insert("deferred".into(), json!(deferred));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> Connection { Connection::open_in_memory().unwrap() }

    #[test]
    fn compute_pass_and_quote_idiom() {
        let r = qaSwarmCoverage(&json!({
            "acceptance": [{ "kind": "test", "cmd": "grep '\"strict\": true' tsconfig.json" }],
            "blocks": [{ "file": "tsconfig.json", "content": "{ \"strict\": true }" }]
        }), &mem()).unwrap();
        assert_eq!(r["passed"], json!(true));
        assert_eq!(r["checked"], json!(1));
        assert_eq!(r["deferred"], json!(0));
        assert_eq!(r["issues"].as_array().unwrap().len(), 0);

        // JS-quoted content covers a JSON-quoted grep pattern (quote normalization).
        let r2 = qaSwarmCoverage(&json!({
            "acceptance": [{ "kind": "test", "cmd": "grep '\"noEmit\": true' tsconfig.json" }],
            "blocks": [{ "file": "tsconfig.json", "content": "{ 'noEmit': true }" }]
        }), &mem()).unwrap();
        assert_eq!(r2["passed"], json!(true));
    }

    #[test]
    fn compute_red_paths() {
        // pattern absent from the authored block content
        let absent = qaSwarmCoverage(&json!({
            "acceptance": [{ "kind": "test", "cmd": "grep 'EXPORT_TOKEN' src/api.ts" }],
            "blocks": [{ "file": "src/api.ts", "content": "export const other = 1;" }]
        }), &mem()).unwrap();
        assert_eq!(absent["passed"], json!(false));
        assert_eq!(absent["issues"].as_array().unwrap().len(), 1);
        assert!(absent["issues"][0]["problem"].as_str().unwrap().contains("does not contain"));
        assert_eq!(absent["issues"][0]["file"], json!("src/api.ts"));

        // no block authors the target file at all
        let miss = qaSwarmCoverage(&json!({
            "acceptance": [{ "kind": "test", "cmd": "grep 'X' src/missing.ts" }],
            "blocks": [{ "file": "src/other.ts", "content": "X" }]
        }), &mem()).unwrap();
        assert_eq!(miss["passed"], json!(false));
        assert!(miss["issues"][0]["problem"].as_str().unwrap().contains("no swarm block authors"));
        assert_eq!(miss["issues"][0]["file"], json!("src/missing.ts"));
    }

    #[test]
    fn compute_deferred() {
        let r = qaSwarmCoverage(&json!({
            "acceptance": [
                { "kind": "file-exists", "path": "src/x.ts" },
                { "kind": "test", "cmd": "npm run build" }
            ],
            "blocks": []
        }), &mem()).unwrap();
        assert_eq!(r["passed"], json!(true));
        assert_eq!(r["checked"], json!(0));
        assert_eq!(r["deferred"], json!(2));
    }

    #[test]
    fn recording_writes_verdict_row() {
        let db = mem();
        db.execute_batch(
            "CREATE TABLE contracts (slug TEXT, acceptance_json TEXT); \
             CREATE TABLE swarm_manifests (id INTEGER PRIMARY KEY, contract_slug TEXT, superseded_by INTEGER, version INTEGER); \
             CREATE TABLE swarm_blocks (id INTEGER PRIMARY KEY, manifest_id INTEGER, file TEXT, content TEXT); \
             CREATE TABLE contract_qa_reviews (id INTEGER PRIMARY KEY, contract_slug TEXT, passed INTEGER, verdict TEXT, findings_md TEXT, issues_json TEXT, reviewer TEXT, failure_target TEXT, review_mode TEXT); \
             INSERT INTO contracts (slug, acceptance_json) VALUES ('SC-1', '[{\"kind\":\"test\",\"cmd\":\"grep ''TOKEN'' src/api.ts\"}]'); \
             INSERT INTO swarm_manifests (id, contract_slug, superseded_by, version) VALUES (1, 'SC-1', NULL, 1); \
             INSERT INTO swarm_blocks (id, manifest_id, file, content) VALUES (1, 1, 'src/api.ts', 'no token here');",
        ).unwrap();

        let r = qaSwarmCoverage(&json!({ "contract_slug": "SC-1", "reviewer": "human:dani" }), &db).unwrap();
        assert_eq!(r["passed"], json!(false));
        assert_eq!(r["verdict"], json!("fail"));
        assert_eq!(r["review_mode"], json!("swarm-coverage"));
        assert_eq!(r["review_id"], json!(1));
        assert_eq!(r["checked"], json!(1));
        assert_eq!(r["issues"][0]["severity"], json!("blocker"));
        assert_eq!(r["issues"][0]["microtask_id"], json!(""));
        assert_eq!(r["issues"][0]["file"], json!("src/api.ts"));
        let (passed_col, reviewer_col): (i64, String) = db.query_row(
            "SELECT passed, reviewer FROM contract_qa_reviews WHERE contract_slug='SC-1'",
            [], |r| Ok((r.get(0)?, r.get(1)?)),
        ).unwrap();
        assert_eq!(passed_col, 0);
        assert_eq!(reviewer_col, "human:dani");

        // Re-author the block to emit the literal token → GREEN, default reviewer.
        db.execute("UPDATE swarm_blocks SET content='has TOKEN now' WHERE id=1", []).unwrap();
        let r2 = qaSwarmCoverage(&json!({ "contract_slug": "SC-1" }), &db).unwrap();
        assert_eq!(r2["passed"], json!(true));
        assert_eq!(r2["verdict"], json!("pass"));
        assert_eq!(r2["review_id"], json!(2));
        let n: i64 = db.query_row("SELECT COUNT(*) FROM contract_qa_reviews WHERE reviewer='driver:auto'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn recording_no_manifest_empty_blocks() {
        let db = mem();
        db.execute_batch(
            "CREATE TABLE contracts (slug TEXT, acceptance_json TEXT); \
             CREATE TABLE swarm_manifests (id INTEGER PRIMARY KEY, contract_slug TEXT, superseded_by INTEGER, version INTEGER); \
             CREATE TABLE swarm_blocks (id INTEGER PRIMARY KEY, manifest_id INTEGER, file TEXT, content TEXT); \
             CREATE TABLE contract_qa_reviews (id INTEGER PRIMARY KEY, contract_slug TEXT, passed INTEGER, verdict TEXT, findings_md TEXT, issues_json TEXT, reviewer TEXT, failure_target TEXT, review_mode TEXT); \
             INSERT INTO contracts (slug, acceptance_json) VALUES ('SC-3', '[{\"kind\":\"test\",\"cmd\":\"grep ''ZZZ'' src/z.ts\"}]');",
        ).unwrap();
        let r = qaSwarmCoverage(&json!({ "contract_slug": "SC-3" }), &db).unwrap();
        assert_eq!(r["passed"], json!(false));
        assert!(r["issues"][0]["problem"].as_str().unwrap().contains("no swarm block authors"));
    }

    #[test]
    fn grep_extraction() {
        let g = grep_re_exec("grep -q '\"strict\": true' apps/x/tsconfig.json").unwrap();
        assert_eq!(g.0, "\"strict\": true");
        assert_eq!(g.1, "apps/x/tsconfig.json");
        assert!(grep_re_exec("cd apps/x && npm run build").is_none());
    }
}

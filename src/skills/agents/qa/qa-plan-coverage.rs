//! qa-plan-coverage.rs — twin of skills/agents/qa/qa-plan-coverage.mjs (db lane; default writes a
//! verdict row, helpers are no-db). NAMES ARE CONTRACT: qaPlanCoverage (default) · normQuotes ·
//! specCoversPattern · norm · fileMatches · safeArr.
// QUALITY: deterministic plan-coverage referee — file-exists + grep acceptances pre-checked, dynamic deferred
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
fn js_string(v: &Value) -> String {
    // JS String(v) (no ??)
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
    // JS `${v}`
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(x) => str_qq(Some(x)),
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

// ── module-level primitives (mirror the exported helpers) ───────────────────

// normQuotes: String(s ?? '').replace(/['"]/g, '"')
fn norm_quotes_of(s: &str) -> String {
    s.chars().map(|c| if c == '\'' || c == '"' { '"' } else { c }).collect()
}
fn norm_quotes_str(v: Option<&Value>) -> String { norm_quotes_of(&str_qq(v)) }

// specCoversPattern: normQuotes(spec).includes(normQuotes(pattern))
fn spec_covers(spec: Option<&Value>, pattern: &str) -> bool {
    norm_quotes_str(spec).contains(&norm_quotes_of(pattern))
}

// norm: String(p ?? '').trim().replace(/^\.?\//, '')   (single, anchored → strips one leading "./" or "/")
fn norm_str(v: Option<&Value>) -> String {
    let raw = str_qq(v);
    let t = raw.trim();
    if let Some(rest) = t.strip_prefix("./") { rest.to_string() }
    else if let Some(rest) = t.strip_prefix('/') { rest.to_string() }
    else { t.to_string() }
}

// fileMatches(m, accPath): does this microtask author the acceptance's target path?
fn file_matches_bool(m: &Value, acc_path: Option<&Value>) -> bool {
    let ap = norm_str(acc_path);
    if ap.is_empty() { return false; }
    let mf = norm_str(m.get("file"));
    if !mf.is_empty() && (mf == ap || mf.ends_with(&format!("/{ap}")) || ap.ends_with(&format!("/{mf}"))) {
        return true;
    }
    let ft: Vec<String> = m.get("files_touched").and_then(|v| v.as_array())
        .map(|a| a.iter().map(|x| norm_str(Some(x))).collect())
        .unwrap_or_default();
    if ft.contains(&ap) { return true; }
    ft.iter().any(|f| !f.is_empty() && (f.ends_with(&format!("/{ap}")) || ap.ends_with(&format!("/{f}"))))
}

// safeArr(s): try { JSON.parse(String(s)) } → array | [] ; catch → []
fn safe_arr_val(v: Option<&Value>) -> Value {
    match v {
        None => json!([]),
        Some(x) => match serde_json::from_str::<Value>(&js_string(x)) {
            Ok(a) if a.is_array() => a,
            _ => json!([]),
        },
    }
}

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

// ── exported helper twins (no-db lane) ──────────────────────────────────────
pub fn normQuotes(s: &Value) -> Value { json!(norm_quotes_str(Some(s))) }
pub fn specCoversPattern(spec: &Value, pattern: &Value) -> Value { json!(spec_covers(Some(spec), &str_qq(Some(pattern)))) }
pub fn norm(p: &Value) -> Value { json!(norm_str(Some(p))) }
pub fn fileMatches(m: &Value, acc_path: &Value) -> Value { json!(file_matches_bool(m, Some(acc_path))) }
pub fn safeArr(s: &Value) -> Value { safe_arr_val(Some(s)) }

// ── default: the driver/compute referee ─────────────────────────────────────
pub fn qaPlanCoverage(input: &Value, db: &Connection) -> Result<Value, String> {
    let contract_slug = input.get("contract_slug");

    let mut acceptance: Option<Value> = input.get("acceptance").cloned();
    let mut microtasks: Option<Value> = input.get("microtasks").cloned();

    if (!is_arr(&acceptance) || !is_arr(&microtasks)) && truthy(contract_slug) {
        // db is always provided by the dispatcher/harness (the !db guard is a non-exercised throw).
        let rows = crate::db_json::query_rows(
            db,
            "SELECT acceptance_json, microtasks_json FROM contracts WHERE slug = ?",
            &[contract_slug.cloned().unwrap_or(Value::Null)],
        )?;
        let c = rows.as_array().and_then(|a| a.first());
        let c = match c {
            Some(row) => row,
            None => return Err(format!("qa-plan-coverage: contract '{}' not found", str_qq(contract_slug))),
        };
        if !is_arr(&acceptance) { acceptance = Some(safe_arr_val(c.get("acceptance_json"))); }
        if !is_arr(&microtasks) { microtasks = Some(safe_arr_val(c.get("microtasks_json"))); }
    }

    let acceptance: Vec<Value> = match acceptance { Some(Value::Array(a)) => a, _ => Vec::new() };
    let microtasks: Vec<Value> = match microtasks { Some(Value::Array(a)) => a, _ => Vec::new() };

    let mut issues: Vec<Value> = Vec::new();
    let mut checked: i64 = 0;
    let mut deferred: i64 = 0;

    for ac in &acceptance {
        let kind = str_qq(ac.get("kind")).to_lowercase();
        if kind == "file_exists" || kind == "file-exists" {
            checked += 1;
            let p = ac.get("path");
            if !microtasks.iter().any(|m| file_matches_bool(m, p)) {
                let mut o = Map::new();
                o.insert("kind".into(), json!("file-exists"));
                if let Some(v) = p { o.insert("acceptance".into(), v.clone()); } // JS omits undefined
                if let Some(v) = p { o.insert("file".into(), v.clone()); }
                o.insert("problem".into(), Value::String(format!(
                    "no microtask creates or targets '{}' (required by the file-exists acceptance) — add a create-microtask for it",
                    tmpl(p)
                )));
                issues.push(Value::Object(o));
            }
        } else if kind == "test" {
            let cmd = str_qq(nz(ac.get("cmd")).or_else(|| nz(ac.get("command"))));
            match grep_re_exec(&cmd) {
                Some((pattern, p)) => {
                    checked += 1;
                    let p_val = Value::String(p.clone());
                    let cmd_val = Value::String(cmd.clone());
                    let pat_json = json_stringify(&Value::String(pattern.clone())); // JSON.stringify(pattern)
                    let mt = microtasks.iter().find(|&m| file_matches_bool(m, Some(&p_val)));
                    match mt {
                        None => {
                            let mut o = Map::new();
                            o.insert("kind".into(), json!("grep"));
                            o.insert("acceptance".into(), cmd_val);
                            o.insert("file".into(), p_val);
                            o.insert("problem".into(), Value::String(format!(
                                "no microtask targets '{}' (required by the grep acceptance for {})",
                                p, pat_json
                            )));
                            issues.push(Value::Object(o));
                        }
                        Some(mt) => {
                            if !spec_covers(mt.get("file_spec_md"), &pattern) {
                                let mut o = Map::new();
                                o.insert("kind".into(), json!("grep"));
                                if let Some(v) = mt.get("microtask_id") { o.insert("microtask_id".into(), v.clone()); }
                                o.insert("acceptance".into(), cmd_val);
                                if let Some(v) = mt.get("file") { o.insert("file".into(), v.clone()); }
                                o.insert("problem".into(), Value::String(format!(
                                    "microtask '{}' ({}) file_spec_md must state {} (quote style is normalized) so the grep acceptance passes — name that exact string in the spec",
                                    tmpl(mt.get("microtask_id")), p, pat_json
                                )));
                                issues.push(Value::Object(o));
                            }
                        }
                    }
                }
                None => { deferred += 1; } // dynamic command → verified downstream
            }
        } else {
            deferred += 1; // non-static kind → not pre-checkable here
        }
    }

    let passed = issues.is_empty();

    if truthy(contract_slug) {
        // Driver step: record a verdict row the state machine reads.
        let det_issues: Vec<Value> = issues.iter().map(|i| {
            let mut o = Map::new();
            o.insert("severity".into(), json!("blocker"));
            o.insert("microtask_id".into(), nz(i.get("microtask_id")).cloned().unwrap_or(json!("")));
            o.insert("file".into(), nz(i.get("file")).cloned().unwrap_or(json!("")));
            o.insert("problem".into(), i.get("problem").cloned().unwrap_or(Value::Null));
            o.insert("fix_hint".into(), json!(""));
            Value::Object(o)
        }).collect();

        let findings = if passed {
            format!("Plan-coverage GREEN: all {} static acceptance(s) covered ({} dynamic deferred to build+verify).", checked, deferred)
        } else {
            format!("Plan-coverage RED: {} of {} static acceptance(s) not covered ({} dynamic deferred). Fix the named specs and re-plan.", det_issues.len(), checked, deferred)
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
                json!("plan-coverage"),
            ],
        )?;

        let mut out = Map::new();
        out.insert("contract_slug".into(), contract_slug.cloned().unwrap_or(Value::Null));
        out.insert("verdict".into(), json!(if passed { "pass" } else { "fail" }));
        out.insert("passed".into(), json!(passed));
        out.insert("failure_target".into(), json!("microtasks"));
        out.insert("review_mode".into(), json!("plan-coverage"));
        out.insert("review_id".into(), json!(review_id));
        out.insert("issues".into(), Value::Array(det_issues));
        out.insert("findings_md".into(), Value::String(findings));
        out.insert("checked_static".into(), json!(checked));
        out.insert("deferred".into(), json!(deferred));
        return Ok(Value::Object(out));
    }

    let mut out = Map::new();
    out.insert("passed".into(), json!(passed));
    out.insert("issues".into(), Value::Array(issues));
    out.insert("checked_static".into(), json!(checked));
    out.insert("deferred".into(), json!(deferred));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> Connection { Connection::open_in_memory().unwrap() }

    #[test]
    fn helpers() {
        assert_eq!(norm(&json!("./apps/x/pkg.json")), json!("apps/x/pkg.json"));
        assert_eq!(norm(&json!("/root")), json!("root"));
        assert_eq!(norm(&json!("  a/b  ")), json!("a/b"));
        assert_eq!(norm(&Value::Null), json!(""));
        assert_eq!(normQuotes(&json!("'strict': true")), json!("\"strict\": true"));
        assert_eq!(specCoversPattern(&json!("has 'strict': true here"), &json!("\"strict\": true")), json!(true));
        assert_eq!(specCoversPattern(&json!("no match"), &json!("\"strict\": true")), json!(false));
        assert_eq!(fileMatches(&json!({ "file": "apps/x/package.json" }), &json!("package.json")), json!(true));
        assert_eq!(fileMatches(&json!({ "files_touched": ["a/b", "apps/x/tsconfig.json"] }), &json!("tsconfig.json")), json!(true));
        assert_eq!(fileMatches(&json!({ "file": "apps/x/README.md" }), &json!("OTHER.md")), json!(false));
        assert_eq!(safeArr(&json!("[{\"a\":1}]")), json!([{ "a": 1 }]));
        assert_eq!(safeArr(&json!("{\"nope\":1}")), json!([]));
        assert_eq!(safeArr(&json!("not json")), json!([]));
        assert_eq!(safeArr(&Value::Null), json!([]));
    }

    #[test]
    fn grep_extraction() {
        let g = grep_re_exec("grep -q '\"strict\": true' apps/x/tsconfig.json").unwrap();
        assert_eq!(g.0, "\"strict\": true");
        assert_eq!(g.1, "apps/x/tsconfig.json");
        assert!(grep_re_exec("cd apps/x && npm run build").is_none());
    }

    #[test]
    fn compute_only_pass_and_fail() {
        let input = json!({
            "acceptance": [
                { "kind": "file-exists", "path": "apps/x/package.json" },
                { "kind": "test", "cmd": "grep -q '\"strict\": true' apps/x/tsconfig.json" },
                { "kind": "test", "cmd": "cd apps/x && npm run build", "expect_exit": 0 }
            ],
            "microtasks": [
                { "microtask_id": "package.json", "file": "apps/x/package.json", "file_spec_md": "package.json with name x" },
                { "microtask_id": "tsconfig.json", "file": "apps/x/tsconfig.json", "file_spec_md": "tsconfig including \"strict\": true exactly" }
            ]
        });
        let r = qaPlanCoverage(&input, &mem()).unwrap();
        assert_eq!(r["passed"], json!(true));
        assert_eq!(r["checked_static"], json!(2));
        assert_eq!(r["deferred"], json!(1));
        assert_eq!(r["issues"].as_array().unwrap().len(), 0);

        let miss = qaPlanCoverage(&json!({
            "acceptance": [{ "kind": "file-exists", "path": "apps/x/README.md" }],
            "microtasks": [{ "microtask_id": "package.json", "file": "apps/x/package.json", "file_spec_md": "pkg" }]
        }), &mem()).unwrap();
        assert_eq!(miss["passed"], json!(false));
        assert_eq!(miss["issues"][0]["kind"], json!("file-exists"));
    }

    #[test]
    fn recording_writes_verdict_row() {
        let db = mem();
        db.execute_batch("CREATE TABLE contracts (slug TEXT, acceptance_json TEXT, microtasks_json TEXT); \
          CREATE TABLE contract_qa_reviews (id INTEGER PRIMARY KEY, contract_slug TEXT, passed INTEGER, verdict TEXT, findings_md TEXT, issues_json TEXT, reviewer TEXT, failure_target TEXT, review_mode TEXT); \
          INSERT INTO contracts (slug, acceptance_json, microtasks_json) VALUES \
          ('C-RED', '[{\"kind\":\"file-exists\",\"path\":\"apps/x/OTHER.md\"},{\"kind\":\"test\",\"cmd\":\"grep -q ''npm run test'' apps/x/README.md\"}]', \
                    '[{\"microtask_id\":\"README.md\",\"file\":\"apps/x/README.md\",\"file_spec_md\":\"README docs\"}]');").unwrap();
        let r = qaPlanCoverage(&json!({ "contract_slug": "C-RED", "reviewer": "human:dani" }), &db).unwrap();
        assert_eq!(r["verdict"], json!("fail"));
        assert_eq!(r["review_mode"], json!("plan-coverage"));
        assert_eq!(r["failure_target"], json!("microtasks"));
        assert_eq!(r["review_id"], json!(1));
        assert_eq!(r["checked_static"], json!(2));
        assert_eq!(r["issues"][0]["severity"], json!("blocker"));
        assert_eq!(r["issues"][0]["microtask_id"], json!("")); // file-exists issue → empty
        assert_eq!(r["issues"][1]["microtask_id"], json!("README.md")); // grep-spec issue → set
        let n: i64 = db.query_row("SELECT COUNT(*) FROM contract_qa_reviews WHERE reviewer='human:dani'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }
}

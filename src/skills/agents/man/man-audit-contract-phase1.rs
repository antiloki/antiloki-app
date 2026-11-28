//! man-audit-contract-phase1.rs — twin of skills/agents/man/man-audit-contract-phase1.mjs (db
//! WRITE lane). NAMES ARE CONTRACT: `manAuditContractPhase1` (default). Phase 1 deterministic
//! audit of a just-signed contract: 7 criteria (c1-c5 critical, c6-c7 non-critical), aggregate
//! verdict, man_reports persistence, idempotent replay, and the FIX-MAN-BLOCKED-DEADEND
//! contract-reject-on-blocked side effect.
//!
//! GIT SUBPROCESS — REAL CODE, DISCLOSED (playbook lesson 5's flag-skippable pattern, adapted):
//! `readHeadSha()` in the JS is called UNCONDITIONALLY on every non-cached run (not gated by an
//! input flag), so this twin ports it as a real `git rev-parse HEAD` subprocess call
//! (`read_head_sha`, cwd = the same `REPO_ROOT` the JS computes via
//! `resolve(HERE,'..','..','..','..')` from `skills/agents/man/` — mirrored here via
//! `CARGO_MANIFEST_DIR`'s parent, exact precedent: man-audit-drift-spec-vs-impl.rs's
//! `repo_root()`). Its RESULT is masked from every fixture-compared output byte, though, so the
//! two-lane comparison can never diverge on it: `headSha` feeds ONLY criteria.c7's pass/detail,
//! and `pass = !headSha || !contract.base_sha || ...` / `detail = contract.base_sha && headSha ?
//! ... : 'sha comparison skipped (missing data)'` — as long as no fixture case sets
//! `contract.base_sha` to a value that happens to equal the real `git rev-parse HEAD` output at
//! conform-run time (this checkout's `REPO_ROOT`, `C:/dev/antiloki`, has no `.git` of its own —
//! each of antiloki-core/antiloki-core-rust/antiloki-crossing is its OWN separate repo — so
//! `git rev-parse HEAD` there fails identically in both lanes and `headSha` is reliably `None`),
//! c7 always takes the `!headSha` branch and its output is identical regardless of what the real
//! subprocess returns. Fixtures below never seed a matching `base_sha`, so this holds.
//!
//! `context.db` missing → JS throws `err.contextMissing('man-audit-contract-phase1', 'db')`. Not
//! modeled: `db: &Connection` is a required, non-nullable reference here (sibling man-audit twins'
//! convention, e.g. man-audit-drift-spec-vs-impl.rs). `contract_slug` required / contract not
//! found / status not tl_approved|human_approved are guard-throws — not fixture-exercised
//! (playbook gotcha 9, success-path-only), but real `Err(String)` messages are still produced,
//! matching AntilokiError's `"<skill>: <cause>"` format exactly.
//!
//! REALISTIC-DOMAIN SIMPLIFICATION (same convention as man-audit-drift-spec-vs-impl.rs's
//! `parse_json_field`): `files_touched_json`/`acceptance_json`/`microtasks_json`/
//! `must_obey_json`/`out_of_scope_json` are assumed to always decode to a JSON array (or the `[]`
//! fallback) — this twin's `load_json` reproduces the JS `loadJson` helper's exact `if (!s) return
//! fallback; try { JSON.parse(s) } catch { fallback }` semantics (including the "literal JSON
//! text `null` parses to `null`, NOT the fallback" edge case per playbook gotcha 6), but
//! `must_obey.includes(law)` / `touchesSensitive(files)`'s `for...of` would THROW in JS if the
//! parsed value were some other non-array truthy JSON (a bare object/number/string) — this twin
//! instead degrades to empty-array behavior for that shape rather than reproducing the crash,
//! matching the established simplification precedent. Not exercised by any fixture case (all seed
//! well-formed JSON arrays).
// QUALITY: 78/100 — 7-criterion deterministic audit + verdict aggregation + man_reports INSERT +
// conditional contracts UPDATE (frozen-clock) + idempotent replay; one disclosed-inert subprocess call

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } } // = JS String(v ?? '')
fn js_string(v: &Value) -> String { match v { Value::Null=>"null".into(), Value::String(s)=>s.clone(), Value::Number(n)=>n.to_string(), Value::Bool(b)=>b.to_string(), Value::Array(a)=>a.iter().map(js_string).collect::<Vec<_>>().join(","), Value::Object(_)=>"[object Object]".into() } } // = JS String(v)
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>str_qq(Some(x)) } } // = JS `${v}`

const REQUIRED_HARD_LAWS: [&str; 2] = ["NEVER_BYPASS_WITH_AUDIT", "HISTORY_IS_SACRED_NO_REVERT"];

// JS `x.length ?? 0` for our 5 array-or-null fields: arrays → their length, JSON strings → UTF-16
// code-unit count (JS .length semantics), everything else (object/number/bool/null) → undefined.
fn js_len(v: &Value) -> Option<i64> {
    match v {
        Value::Array(a) => Some(a.len() as i64),
        Value::String(s) => Some(s.encode_utf16().count() as i64),
        _ => None,
    }
}
fn len_or0(v: &Value) -> i64 { js_len(v).unwrap_or(0) } // `${x.length ?? 0}`
fn len_tmpl(v: &Value) -> String { match js_len(v) { Some(n) => n.to_string(), None => "undefined".into() } } // `${x.length}` (no ??)

// JS: `function loadJson(s, fallback) { if (!s) return fallback; try { return JSON.parse(s); }
// catch { return fallback; } }` — note a literal JSON `null` string parses successfully to
// `null`, which is NOT `!s`-falsy at the *pre*-parse check, so it is returned as-is, not the
// fallback (playbook gotcha 6). `s` is always a TEXT column here (String or Null).
fn load_json(v: Option<&Value>, fallback: Value) -> Value {
    let s = match v {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        _ => return fallback, // null/None/empty-string are all falsy → fallback
    };
    serde_json::from_str::<Value>(&s).unwrap_or(fallback)
}

fn sensitive_patterns() -> &'static [regex::Regex] {
    use std::sync::OnceLock;
    static R: OnceLock<Vec<regex::Regex>> = OnceLock::new();
    R.get_or_init(|| {
        vec![
            regex::Regex::new(r"^\.antiloki/migrations/").unwrap(),
            regex::Regex::new(r"^\.antiloki/skills/engine/").unwrap(),
            regex::Regex::new(r"^\.antiloki/db/").unwrap(),
            regex::Regex::new(r"^bin/antiloki\.mjs$").unwrap(),
        ]
    })
}

// JS: `function touchesSensitive(files) { for (const f of files ?? []) { if
// (SENSITIVE_PATTERNS.some((re) => re.test(f))) return true; } return false; }` — RegExp.test
// coerces its argument via ToString (js_string, not str_qq).
fn touches_sensitive(files: &Value) -> bool {
    let arr: &[Value] = match files { Value::Array(a) => a.as_slice(), _ => &[] };
    arr.iter().any(|f| {
        let s = js_string(f);
        sensitive_patterns().iter().any(|re| re.is_match(&s))
    })
}

// JS: `const HERE = dirname(fileURLToPath(import.meta.url)); const REPO_ROOT = resolve(HERE,
// '..','..','..','..');` from skills/agents/man/ — same 4-level-up target as
// man-audit-drift-spec-vs-impl.rs's `repo_root()` (copied verbatim, same precedent).
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("antiloki-core-rust has a parent dir")
        .to_path_buf()
}

/// `function readHeadSha() { try { return execSync('git rev-parse HEAD', { cwd: REPO_ROOT,
/// encoding: 'utf8' }).trim(); } catch { return null; } }` — real subprocess call; see module doc
/// for why its actual return value never leaks into any fixture-compared output.
fn read_head_sha() -> Option<String> {
    let out = crate::proc::cmd("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo_root())
        .output();
    match out {
        Ok(o) if o.status.success() => {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if s.is_empty() { None } else { Some(s) }
        }
        _ => None,
    }
}

/// `export default function manAuditContractPhase1(input, context)`.
pub fn manAuditContractPhase1(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const { db } = context ?? {}; if (!db) throw err.contextMissing(...)` — unreachable
    // here (db is always injected in the db lane).

    let contract_slug_v = input.get("contract_slug").cloned().unwrap_or(Value::Null);
    if !truthy(Some(&contract_slug_v)) {
        return Err("man-audit-contract-phase1: contract_slug is required".to_string()); // guard-throw, not fixture-exercised
    }

    let prior = crate::db_json::query_rows(
        db,
        "SELECT id, verdict, what_happened_md, lessons_md, metrics_json, criteria_json, ts \
         FROM man_reports WHERE contract_slug = ? AND phase = 'contract_signed' AND verdict != 'blocked' \
         ORDER BY id DESC LIMIT 1",
        &[contract_slug_v.clone()],
    )?;
    if let Some(p) = prior.as_array().and_then(|a| a.first()) {
        let mut out = Map::new();
        out.insert("contract_slug".into(), contract_slug_v.clone());
        out.insert("cached".into(), json!(true));
        out.insert("report_id".into(), p.get("id").cloned().unwrap_or(Value::Null));
        out.insert("verdict".into(), p.get("verdict").cloned().unwrap_or(Value::Null));
        out.insert("what_happened_md".into(), p.get("what_happened_md").cloned().unwrap_or(Value::Null));
        out.insert("lessons_md".into(), p.get("lessons_md").cloned().unwrap_or(Value::Null));
        out.insert("metrics".into(), load_json(p.get("metrics_json"), json!({})));
        out.insert("criteria".into(), load_json(p.get("criteria_json"), json!({})));
        return Ok(Value::Object(out));
    }

    let contracts = crate::db_json::query_rows(
        db,
        "SELECT slug, card_slug, status, tier, risk, complexity, size, base_sha, \
                files_touched_json, acceptance_json, microtasks_json, \
                must_obey_json, out_of_scope_json \
         FROM contracts WHERE slug = ?",
        &[contract_slug_v.clone()],
    )?;
    let contract = match contracts.as_array().and_then(|a| a.first()) {
        Some(c) => c.clone(),
        None => {
            return Err(format!(
                "man-audit-contract-phase1: contract '{}' not found", // guard-throw, not fixture-exercised
                tmpl(Some(&contract_slug_v))
            ));
        }
    };
    let status = contract.get("status").and_then(Value::as_str);
    if status != Some("tl_approved") && status != Some("human_approved") {
        return Err(format!( // guard-throw, not fixture-exercised
            "man-audit-contract-phase1: Phase 1 audit only valid for tl_approved/human_approved contracts; got '{}'",
            tmpl(contract.get("status"))
        ));
    }

    // ── Deterministic checks ────────────────────────────────────────────
    let files = load_json(contract.get("files_touched_json"), json!([]));
    let acceptance = load_json(contract.get("acceptance_json"), json!([]));
    let microtasks = load_json(contract.get("microtasks_json"), json!([]));
    let must_obey = load_json(contract.get("must_obey_json"), json!([]));
    let out_of_scope = load_json(contract.get("out_of_scope_json"), json!([]));
    let head_sha = read_head_sha();

    let mut criteria = Map::new();

    let c1_pass = matches!(&files, Value::Array(a) if !a.is_empty());
    criteria.insert(
        "c1_files_touched_non_empty".into(),
        json!({ "critical": true, "pass": c1_pass, "detail": format!("{} files declared", len_or0(&files)) }),
    );

    let c2_pass = matches!(&acceptance, Value::Array(a) if !a.is_empty());
    criteria.insert(
        "c2_acceptance_has_items".into(),
        json!({ "critical": true, "pass": c2_pass, "detail": format!("{} acceptance criteria", len_or0(&acceptance)) }),
    );

    // EPIC-SWARM: a swarm M/L contract carries its work as authored swarm_blocks instead of
    // microtasks_json — present blocks satisfy c3 the same way microtasks do.
    let contract_slug_str = contract_slug_v.as_str().unwrap_or("");
    let swarm_block_count = crate::swarm_swarm_block_presence::authoredBlockCount(db, contract_slug_str)
        .as_i64()
        .unwrap_or(0);
    let size_opt = contract.get("size").and_then(Value::as_str);
    let c3_pass = (size_opt != Some("M") && size_opt != Some("L"))
        || matches!(&microtasks, Value::Array(a) if !a.is_empty())
        || swarm_block_count > 0;
    let size_display = match contract.get("size") {
        None | Some(Value::Null) => "?".to_string(),
        Some(v) => tmpl(Some(v)),
    };
    criteria.insert(
        "c3_microtasks_present_if_ML".into(),
        json!({ "critical": true, "pass": c3_pass,
            "detail": format!("size={}, microtasks={}, swarm_blocks={}", size_display, len_or0(&microtasks), swarm_block_count) }),
    );

    let must_obey_arr: &[Value] = match &must_obey { Value::Array(a) => a.as_slice(), _ => &[] };
    let missing_laws: Vec<&str> = REQUIRED_HARD_LAWS
        .iter()
        .copied()
        .filter(|law| !must_obey_arr.iter().any(|v| v.as_str() == Some(*law)))
        .collect();
    let c4_pass = missing_laws.is_empty();
    let missing_str = if missing_laws.is_empty() { "(none)".to_string() } else { missing_laws.join(", ") };
    criteria.insert(
        "c4_must_obey_minimum".into(),
        json!({ "critical": true, "pass": c4_pass, "detail": format!("declared: {} laws; missing: {}", len_tmpl(&must_obey), missing_str) }),
    );

    let c5_pass = matches!(&out_of_scope, Value::Array(a) if !a.is_empty());
    criteria.insert(
        "c5_out_of_scope_non_empty".into(),
        json!({ "critical": true, "pass": c5_pass, "detail": format!("{} out-of-scope rules declared", len_or0(&out_of_scope)) }),
    );

    let sensitive = touches_sensitive(&files);
    let tier_disp = tmpl(contract.get("tier"));
    let tier_is_high_risk = contract.get("tier").and_then(Value::as_str) == Some("high-risk");
    let c6_pass = !sensitive || tier_is_high_risk;
    let c6_detail = if sensitive {
        format!("contract touches sensitive paths; tier={} (expected high-risk)", tier_disp)
    } else {
        format!("no sensitive paths touched; tier={} acceptable", tier_disp)
    };
    criteria.insert("c6_tier_consistency".into(), json!({ "critical": false, "pass": c6_pass, "detail": c6_detail }));

    let base_sha_v = contract.get("base_sha").cloned().unwrap_or(Value::Null);
    let base_sha_truthy = truthy(Some(&base_sha_v));
    let head_truthy = head_sha.is_some();
    let c7_pass = !head_truthy || !base_sha_truthy || base_sha_v.as_str() == head_sha.as_deref();
    let c7_detail = if base_sha_truthy && head_truthy {
        let base8: String = js_string(&base_sha_v).chars().take(8).collect();
        let head_ref = head_sha.as_deref().unwrap_or("");
        let head8: String = head_ref.chars().take(8).collect();
        let word = if base_sha_v.as_str() == Some(head_ref) { "match" } else { "STALE" };
        format!("base_sha={}, HEAD={}, {}", base8, head8, word)
    } else {
        "sha comparison skipped (missing data)".to_string()
    };
    criteria.insert("c7_base_sha_fresh".into(), json!({ "critical": false, "pass": c7_pass, "detail": c7_detail }));

    // Aggregate verdict (insertion order = c1..c7, matches Object.entries order).
    let mut critical_fails: Vec<(String, Value)> = Vec::new();
    let mut noncritical_fails: Vec<(String, Value)> = Vec::new();
    let mut passes: Vec<String> = Vec::new();
    for (k, v) in criteria.iter() {
        let critical = v.get("critical").and_then(Value::as_bool).unwrap_or(false);
        let pass = v.get("pass").and_then(Value::as_bool).unwrap_or(false);
        if pass {
            passes.push(k.clone());
        } else if critical {
            critical_fails.push((k.clone(), v.clone()));
        } else {
            noncritical_fails.push((k.clone(), v.clone()));
        }
    }
    let verdict = if !critical_fails.is_empty() {
        "blocked"
    } else if !noncritical_fails.is_empty() {
        "flagged"
    } else {
        "approved"
    };

    // Metrics — undefined-valued keys (a non-array/non-string field) are OMITTED, matching
    // JSON.stringify's `undefined` property behavior (playbook gotcha 3).
    let mut metrics = Map::new();
    if let Some(n) = js_len(&files) { metrics.insert("files_touched_count".into(), json!(n)); }
    if let Some(n) = js_len(&acceptance) { metrics.insert("acceptance_count".into(), json!(n)); }
    if let Some(n) = js_len(&microtasks) { metrics.insert("microtasks_count".into(), json!(n)); }
    if let Some(n) = js_len(&must_obey) { metrics.insert("must_obey_count".into(), json!(n)); }
    if let Some(n) = js_len(&out_of_scope) { metrics.insert("out_of_scope_count".into(), json!(n)); }
    metrics.insert("size".into(), contract.get("size").cloned().unwrap_or(Value::Null));
    metrics.insert("tier".into(), contract.get("tier").cloned().unwrap_or(Value::Null));
    metrics.insert("risk".into(), contract.get("risk").cloned().unwrap_or(Value::Null));
    metrics.insert("complexity".into(), contract.get("complexity").cloned().unwrap_or(Value::Null));

    // Narrative
    let mut lines: Vec<String> = Vec::new();
    lines.push(format!(
        "Phase 1 audit of '{}' (card: {}, size={}, tier={}, risk={}/5).",
        tmpl(contract.get("slug")), tmpl(contract.get("card_slug")), tmpl(contract.get("size")),
        tmpl(contract.get("tier")), tmpl(contract.get("risk"))
    ));
    lines.push(format!("Verdict: {}", verdict.to_uppercase()));
    if !critical_fails.is_empty() {
        lines.push(format!("Critical failures ({}):", critical_fails.len()));
        for (name, c) in &critical_fails {
            lines.push(format!("  \u{2717} {} \u{2014} {}", name, c.get("detail").and_then(Value::as_str).unwrap_or("")));
        }
    }
    if !noncritical_fails.is_empty() {
        lines.push(format!("Non-critical concerns ({}):", noncritical_fails.len()));
        for (name, c) in &noncritical_fails {
            lines.push(format!("  \u{26A0} {} \u{2014} {}", name, c.get("detail").and_then(Value::as_str).unwrap_or("")));
        }
    }
    if !passes.is_empty() {
        lines.push(format!("Passed: {}.", passes.join(", ")));
    }
    let what_happened_md = lines.join("\n");

    // Persist
    let (_changes, report_id) = crate::db_json::run_write(
        db,
        "INSERT INTO man_reports (contract_slug, phase, verdict, metrics_json, criteria_json, what_happened_md) \
         VALUES (?, 'contract_signed', ?, ?, ?, ?)",
        &[
            contract_slug_v.clone(),
            json!(verdict),
            json!(serde_json::to_string(&Value::Object(metrics.clone())).unwrap()),
            json!(serde_json::to_string(&Value::Object(criteria.clone())).unwrap()),
            json!(what_happened_md.clone()),
        ],
    )?;

    // FIX-MAN-BLOCKED-DEADEND: a blocked verdict also flips status to 'rejected' so
    // contract-rejected-redo routes it to tl.contract.redraft.
    if verdict == "blocked" {
        crate::db_json::run_write(
            db,
            &format!("UPDATE contracts SET status = 'rejected', \
                 failure_kind = COALESCE(failure_kind, 'man_revision_blocked'), \
                 rejection_reason_md = COALESCE(rejection_reason_md, ?), \
                 updated_at = {} \
             WHERE slug = ?", crate::clock::now_sql()),
            &[json!(what_happened_md.clone()), contract_slug_v.clone()],
        )?;
    }

    let mut out = Map::new();
    out.insert("contract_slug".into(), contract_slug_v);
    out.insert("cached".into(), json!(false));
    out.insert("report_id".into(), json!(report_id));
    out.insert("verdict".into(), json!(verdict));
    out.insert("what_happened_md".into(), json!(what_happened_md));
    out.insert("metrics".into(), Value::Object(metrics));
    out.insert("criteria".into(), Value::Object(criteria));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE contracts (id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT NOT NULL UNIQUE, \
             card_slug TEXT, status TEXT NOT NULL DEFAULT 'proposed', tier TEXT, risk INTEGER, \
             complexity INTEGER, size TEXT, base_sha TEXT, \
             files_touched_json TEXT NOT NULL DEFAULT '[]', acceptance_json TEXT NOT NULL DEFAULT '[]', \
             microtasks_json TEXT NOT NULL DEFAULT '[]', must_obey_json TEXT NOT NULL DEFAULT '[]', \
             out_of_scope_json TEXT NOT NULL DEFAULT '[]', failure_kind TEXT, rejection_reason_md TEXT, \
             updated_at TEXT); \
             CREATE TABLE man_reports (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT NOT NULL, \
             phase TEXT NOT NULL, ts TEXT, verdict TEXT, metrics_json TEXT, criteria_json TEXT, \
             what_happened_md TEXT, lessons_md TEXT); \
             CREATE TABLE swarm_manifests (id INTEGER, contract_slug TEXT, superseded_by TEXT, version INTEGER); \
             CREATE TABLE swarm_blocks (manifest_id INTEGER)",
        )
        .unwrap();
        c
    }

    fn seed(c: &Connection, slug: &str, status: &str, size: &str, tier: &str,
            files: &str, acceptance: &str, microtasks: &str, must_obey: &str, out_of_scope: &str) {
        c.execute(
            "INSERT INTO contracts (slug, status, tier, risk, complexity, size, card_slug, \
             files_touched_json, acceptance_json, microtasks_json, must_obey_json, out_of_scope_json) \
             VALUES (?1, ?2, ?3, 2, 2, ?4, 'CARD-A', ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![slug, status, tier, size, files, acceptance, microtasks, must_obey, out_of_scope],
        )
        .unwrap();
    }

    const MUST_OBEY_OK: &str = r#"["NEVER_BYPASS_WITH_AUDIT","HISTORY_IS_SACRED_NO_REVERT"]"#;

    #[test]
    fn clean_contract_is_approved_and_persists() {
        let c = db();
        seed(&c, "CT-1", "tl_approved", "S", "standard", r#"["src/foo.mjs"]"#, r#"["ac1"]"#, "[]", MUST_OBEY_OK, r#"["no ui changes"]"#);
        let r = manAuditContractPhase1(&json!({ "contract_slug": "CT-1" }), &c).unwrap();
        assert_eq!(r["verdict"], json!("approved"));
        assert_eq!(r["cached"], json!(false));
        assert!(r["report_id"].as_i64().unwrap() > 0);
        assert_eq!(r["criteria"]["c1_files_touched_non_empty"]["pass"], json!(true));
        let keys: Vec<&String> = r.as_object().unwrap().keys().collect();
        assert_eq!(keys, vec!["contract_slug", "cached", "report_id", "verdict", "what_happened_md", "metrics", "criteria"]);
        let row = crate::db_json::query_rows(&c, "SELECT verdict, phase FROM man_reports WHERE contract_slug='CT-1'", &[]).unwrap();
        assert_eq!(row[0]["verdict"], json!("approved"));
        assert_eq!(row[0]["phase"], json!("contract_signed"));
    }

    #[test]
    fn empty_files_touched_is_blocked_and_rejects_contract() {
        let c = db();
        seed(&c, "CT-2", "tl_approved", "S", "standard", "[]", r#"["ac1"]"#, "[]", MUST_OBEY_OK, r#"["x"]"#);
        let r = manAuditContractPhase1(&json!({ "contract_slug": "CT-2" }), &c).unwrap();
        assert_eq!(r["verdict"], json!("blocked"));
        assert_eq!(r["criteria"]["c1_files_touched_non_empty"]["pass"], json!(false));
        let row = crate::db_json::query_rows(&c, "SELECT status, failure_kind FROM contracts WHERE slug='CT-2'", &[]).unwrap();
        assert_eq!(row[0]["status"], json!("rejected"));
        assert_eq!(row[0]["failure_kind"], json!("man_revision_blocked"));
    }

    #[test]
    fn size_m_without_microtasks_or_swarm_blocks_is_blocked() {
        let c = db();
        seed(&c, "CT-3", "tl_approved", "M", "standard", r#"["a.mjs"]"#, r#"["ac1"]"#, "[]", MUST_OBEY_OK, r#"["x"]"#);
        let r = manAuditContractPhase1(&json!({ "contract_slug": "CT-3" }), &c).unwrap();
        assert_eq!(r["criteria"]["c3_microtasks_present_if_ML"]["pass"], json!(false));
        assert_eq!(r["verdict"], json!("blocked"));
    }

    #[test]
    fn size_m_with_authored_swarm_blocks_passes_c3() {
        let c = db();
        seed(&c, "CT-4", "tl_approved", "M", "standard", r#"["a.mjs"]"#, r#"["ac1"]"#, "[]", MUST_OBEY_OK, r#"["x"]"#);
        c.execute("INSERT INTO swarm_manifests (id, contract_slug, superseded_by, version) VALUES (1, 'CT-4', NULL, 1)", []).unwrap();
        c.execute("INSERT INTO swarm_blocks (manifest_id) VALUES (1), (1)", []).unwrap();
        let r = manAuditContractPhase1(&json!({ "contract_slug": "CT-4" }), &c).unwrap();
        assert_eq!(r["criteria"]["c3_microtasks_present_if_ML"]["pass"], json!(true));
        assert!(r["criteria"]["c3_microtasks_present_if_ML"]["detail"].as_str().unwrap().contains("swarm_blocks=2"));
        assert_eq!(r["verdict"], json!("approved"));
    }

    #[test]
    fn missing_must_obey_laws_is_blocked() {
        let c = db();
        seed(&c, "CT-5", "tl_approved", "S", "standard", r#"["a.mjs"]"#, r#"["ac1"]"#, "[]", "[]", r#"["x"]"#);
        let r = manAuditContractPhase1(&json!({ "contract_slug": "CT-5" }), &c).unwrap();
        assert_eq!(r["criteria"]["c4_must_obey_minimum"]["pass"], json!(false));
        assert!(r["criteria"]["c4_must_obey_minimum"]["detail"].as_str().unwrap().contains("NEVER_BYPASS_WITH_AUDIT"));
        assert_eq!(r["verdict"], json!("blocked"));
    }

    #[test]
    fn sensitive_path_with_standard_tier_is_flagged_not_blocked() {
        let c = db();
        seed(&c, "CT-6", "tl_approved", "S", "standard", r#"[".antiloki/migrations/x.mjs"]"#, r#"["ac1"]"#, "[]", MUST_OBEY_OK, r#"["x"]"#);
        let r = manAuditContractPhase1(&json!({ "contract_slug": "CT-6" }), &c).unwrap();
        assert_eq!(r["criteria"]["c6_tier_consistency"]["pass"], json!(false));
        assert_eq!(r["verdict"], json!("flagged"));
        let row = crate::db_json::query_rows(&c, "SELECT status FROM contracts WHERE slug='CT-6'", &[]).unwrap();
        assert_eq!(row[0]["status"], json!("tl_approved")); // flagged, not blocked → no reject side effect
    }

    #[test]
    fn sensitive_path_with_high_risk_tier_passes_c6() {
        let c = db();
        seed(&c, "CT-7", "tl_approved", "S", "high-risk", r#"[".antiloki/db/x.sql"]"#, r#"["ac1"]"#, "[]", MUST_OBEY_OK, r#"["x"]"#);
        let r = manAuditContractPhase1(&json!({ "contract_slug": "CT-7" }), &c).unwrap();
        assert_eq!(r["criteria"]["c6_tier_consistency"]["pass"], json!(true));
        assert_eq!(r["verdict"], json!("approved"));
    }

    #[test]
    fn idempotent_replay_returns_cached_existing_report() {
        let c = db();
        seed(&c, "CT-8", "tl_approved", "S", "standard", r#"["a.mjs"]"#, r#"["ac1"]"#, "[]", MUST_OBEY_OK, r#"["x"]"#);
        c.execute(
            "INSERT INTO man_reports (id, contract_slug, phase, verdict, metrics_json, criteria_json, what_happened_md, lessons_md) \
             VALUES (7, 'CT-8', 'contract_signed', 'approved', '{\"a\":1}', '{\"b\":2}', 'prior narrative', 'lesson text')",
            [],
        )
        .unwrap();
        let r = manAuditContractPhase1(&json!({ "contract_slug": "CT-8" }), &c).unwrap();
        assert_eq!(r["cached"], json!(true));
        assert_eq!(r["report_id"], json!(7));
        assert_eq!(r["verdict"], json!("approved"));
        assert_eq!(r["what_happened_md"], json!("prior narrative"));
        assert_eq!(r["lessons_md"], json!("lesson text"));
        assert_eq!(r["metrics"], json!({ "a": 1 }));
        assert_eq!(r["criteria"], json!({ "b": 2 }));
        let count = crate::db_json::query_rows(&c, "SELECT COUNT(*) AS n FROM man_reports WHERE contract_slug='CT-8'", &[]).unwrap();
        assert_eq!(count[0]["n"], json!(1)); // no new row inserted
    }

    #[test]
    fn blocked_prior_report_is_not_returned_as_cached() {
        let c = db();
        seed(&c, "CT-9", "tl_approved", "S", "standard", r#"["a.mjs"]"#, r#"["ac1"]"#, "[]", MUST_OBEY_OK, r#"["x"]"#);
        c.execute(
            "INSERT INTO man_reports (contract_slug, phase, verdict, what_happened_md) \
             VALUES ('CT-9', 'contract_signed', 'blocked', 'old blocked narrative')",
            [],
        )
        .unwrap();
        let r = manAuditContractPhase1(&json!({ "contract_slug": "CT-9" }), &c).unwrap();
        assert_eq!(r["cached"], json!(false)); // MAN re-audits the new draft
        assert_eq!(r["verdict"], json!("approved"));
    }

    #[test]
    fn missing_contract_slug_input_errs() {
        let c = db();
        let err = manAuditContractPhase1(&json!({}), &c).unwrap_err();
        assert!(err.contains("contract_slug is required"));
    }

    #[test]
    fn wrong_status_errs() {
        let c = db();
        seed(&c, "CT-10", "proposed", "S", "standard", r#"["a.mjs"]"#, r#"["ac1"]"#, "[]", MUST_OBEY_OK, r#"["x"]"#);
        let err = manAuditContractPhase1(&json!({ "contract_slug": "CT-10" }), &c).unwrap_err();
        assert!(err.contains("Phase 1 audit only valid"));
    }
}

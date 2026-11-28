//! man-audit-truth-cross-check.rs — twin of skills/agents/man/man-audit-truth-cross-check.mjs.
//! NAMES ARE CONTRACT: `manAuditTruthCrossCheck` (default, db lane, attached `memory` db).
//! For a given card_slug, pulls 5 independent "truth signals" (phase_events claims, git log,
//! schema_migrations, sacred_timeline, memory.memories) and runs 7 cross-checks (C1-C7) between
//! them, returning a finding list with severities.
//!
//! BESPOKE-NATIVE — NOT fixture-testable through the conform-db two-lane harness. Every success
//! path unconditionally shells out to `git log` (`gitLogForSlug` is called unconditionally right
//! after the card lookup, with no input flag to skip it — unlike constitution-estimate.rs's
//! `git_diff_paths`, which only fires when `input.base` is truthy). Compounding that: the JS
//! computes `REPO_ROOT` as `resolve(dirname(fileURLToPath(import.meta.url)), '..','..','..','..')`
//! — a HARDCODED constant derived from the module's own file location, not env-overridable and not
//! DB-derived (same precedent as man-audit-drift-spec-vs-impl.rs / qa-sandbox-reset.rs's
//! `REPO_ROOT`). This twin mirrors `repo_root()` verbatim from that sibling (via
//! `CARGO_MANIFEST_DIR`'s parent — the antiloki-core-rust checkout's parent dir, same physical
//! directory the JS's REPO_ROOT resolves to on this layout). On THIS checkout, that directory is
//! not itself a git repository (only `antiloki-core` nested inside it is), so `git log` there
//! always fails and `gitLogForSlug` degrades to `[]` — verified against the real JS source (both
//! the JS test suite's own comment "test environment isn't a real git repo" and a live repro run).
//! The `git_log_for_slug` helper below is written for real (a genuine `git log --all --grep=...`
//! subprocess + output parser) and is unit-tested directly against a scratch git repo with an
//! injected `root: &Path` parameter, proving the subprocess+parse logic works even though the
//! production call site's fixed root never exercises the non-empty branch in this checkout.
//!
//! `--grep="${slug}"` in the JS is ONE shell-interpreted string (execSync spawns a shell); this
//! twin passes `--grep=<slug>` as a single `Command` arg (no shell) — same disclosed divergence as
//! constitution-estimate.rs's `git_diff_paths` (irrelevant for realistic slugs, which never carry
//! shell metacharacters).
//!
//! DISCLOSED JS BUG, ported verbatim (NAMES ARE CONTRACT covers SQL text too): the memories query
//! selects `COALESCE(situation_md, '')` but `memory.memories` has no `situation_md` column (only
//! `situation` — confirmed against schemas.mjs and a live repro: the query always throws "no such
//! column: situation_md", swallowed by the JS `try/catch`, so `memories`/`memories_count` are
//! ALWAYS empty/0 in production regardless of input). This twin issues the identical malformed SQL
//! against the identical real schema, so it fails identically and is caught the same way — no
//! special-casing needed, the bug reproduces itself structurally.
//!
//! `context.db` missing → JS throws `err.contextMissing(...)`; unreachable here since `db:
//! &Connection` is a required, non-nullable reference (every sibling twin's convention).
//! `card_slug` missing/falsy → JS throws `err.required(...)`; card not found → JS throws
//! `AntilokiError({code:NOT_FOUND, skill, cause:"card '<slug>' not found"})`, whose `.message` is
//! `"<skill>: <cause>"` (error.mjs `AntilokiError` constructor) — ported as
//! `Err("man-audit-truth-cross-check: card '<slug>' not found")`. Realistic-domain simplification
//! (card-run-preflight.rs precedent): `card_slug` is narrowed to "non-empty JSON string" rather
//! than JS's raw-truthy check — real callers always pass a TEXT PRIMARY KEY value.

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

// JS: `const HERE = dirname(fileURLToPath(import.meta.url)); const REPO_ROOT = resolve(HERE,
// '..','..','..','..');` — see module doc. CARGO_MANIFEST_DIR's parent lands on the same physical
// directory on this checkout layout (man-audit-drift-spec-vs-impl.rs precedent). No env override.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("antiloki-core-rust has a parent dir")
        .to_path_buf()
}

fn finding(severity: &str, code: &str, message: String, evidence: Value) -> Value {
    let mut m = Map::new();
    m.insert("severity".into(), json!(severity));
    m.insert("code".into(), json!(code));
    m.insert("message".into(), json!(message));
    m.insert("evidence".into(), evidence);
    Value::Object(m)
}

/// `function gitLogForSlug(slug)` — real `git log --all --grep=<slug> --pretty=format:%H|%ci|%s`
/// subprocess (cwd=root), parsed into `{sha (12 chars), committed_at, subject}` rows. Any spawn
/// failure or non-zero exit collapses to `[]`, matching the JS `try { ... } catch { return []; }`.
fn git_log_for_slug(root: &Path, slug: &str) -> Vec<Value> {
    let output = crate::proc::cmd("git")
        .arg("log")
        .arg("--all")
        .arg(format!("--grep={slug}"))
        .arg("--pretty=format:%H|%ci|%s")
        .current_dir(root)
        .output();
    let out = match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
        _ => return Vec::new(),
    };
    let trimmed = out.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    trimmed
        .split('\n')
        .map(|line| {
            let parts: Vec<&str> = line.split('|').collect();
            let sha = parts.first().copied().unwrap_or("");
            let date = parts.get(1).copied().unwrap_or("");
            let subject = if parts.len() > 2 { parts[2..].join("|") } else { String::new() };
            let sha12: String = sha.chars().take(12).collect();
            json!({ "sha": sha12, "committed_at": date, "subject": subject })
        })
        .collect()
}

/// `export default function manAuditTruthCrossCheck(input, context)`.
pub fn manAuditTruthCrossCheck(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS `if (!db) throw err.contextMissing(...)` — unreachable (db lane always injects one).
    let card_slug = match input.get("card_slug").and_then(Value::as_str) {
        Some(s) if !s.is_empty() => s,
        _ => return Err("man-audit-truth-cross-check: card_slug is required".into()),
    };

    // 0. The card itself must exist.
    let card_rows = crate::db_json::query_rows(
        db,
        "SELECT slug, title, status, created_at FROM cards WHERE slug = ?",
        &[json!(card_slug)],
    )?;
    let card = match card_rows.as_array().and_then(|a| a.first()) {
        Some(c) => c.clone(),
        None => return Err(format!("man-audit-truth-cross-check: card '{card_slug}' not found")),
    };
    let card_status = card.get("status").cloned().unwrap_or(Value::Null);
    let created_at = card.get("created_at").cloned().unwrap_or(Value::Null);

    let mut findings: Vec<Value> = Vec::new();

    // 1. Phase events — the claims.
    let phase_events_v = crate::db_json::query_rows(
        db,
        "SELECT phase, actor, ts, evidence_json FROM card_phase_events WHERE card_slug = ? ORDER BY id",
        &[json!(card_slug)],
    )?;
    let phase_events: Vec<Value> = phase_events_v.as_array().cloned().unwrap_or_default();
    // `new Set(phase_events.map(e => e.phase))` — insertion order preserved (first-seen wins),
    // matches JS Set iteration order used later for `[...phases_claimed]` (C6 evidence).
    let mut phases_claimed: Vec<String> = Vec::new();
    let mut seen_phases: HashSet<String> = HashSet::new();
    for e in &phase_events {
        if let Some(p) = e.get("phase").and_then(Value::as_str) {
            if seen_phases.insert(p.to_string()) {
                phases_claimed.push(p.to_string());
            }
        }
    }
    let has_phase = |p: &str| phases_claimed.iter().any(|x| x == p);

    // 2. Git log.
    let commits = git_log_for_slug(&repo_root(), card_slug);

    // 3. Migrations since card opened.
    let migrations_since_v = crate::db_json::query_rows(
        db,
        "SELECT name, applied_at FROM schema_migrations WHERE applied_at >= ? ORDER BY applied_at",
        &[created_at.clone()],
    )?;
    let migrations_since: Vec<Value> = migrations_since_v.as_array().cloned().unwrap_or_default();

    // 4. Sacred timeline rows for this entity.
    let timeline_v = crate::db_json::query_rows(
        db,
        "SELECT operation, actor, ts, user_goal_id FROM sacred_timeline WHERE entity_type='card' AND entity_id = ? ORDER BY id",
        &[json!(card_slug)],
    )?;
    let timeline: Vec<Value> = timeline_v.as_array().cloned().unwrap_or_default();

    // 5. Memories tagged with this slug — try multiple shapes. Wrapped in a swallowed Result,
    // matching JS `try {...} catch { memories = []; }`. See module doc: the SQL text references a
    // nonexistent `situation_md` column verbatim (a real JS bug, ported byte-for-byte), so this
    // always errors and always resolves to `[]` against the real schema.
    let like_pattern = json!(format!("%{card_slug}%"));
    let memories: Vec<Value> = crate::db_json::query_rows(
        db,
        "SELECT id, type, title, created_at FROM memory.memories\n       WHERE COALESCE(tags_json, '') LIKE ?\n          OR COALESCE(title, '') LIKE ?\n          OR COALESCE(situation_md, '') LIKE ?",
        &[like_pattern.clone(), like_pattern.clone(), like_pattern],
    )
    .ok()
    .and_then(|v| v.as_array().cloned())
    .unwrap_or_default();

    // ── Cross-checks ──────────────────────────────────────────────────

    // C1: dev_impl claimed → there must be at least one git commit mentioning slug.
    if has_phase("dev_impl") && commits.is_empty() {
        let dev_impl_events: Vec<Value> = phase_events
            .iter()
            .filter(|e| e.get("phase").and_then(Value::as_str) == Some("dev_impl"))
            .cloned()
            .collect();
        findings.push(finding(
            "critical",
            "DEV_IMPL_NO_COMMIT",
            "card claims dev_impl phase but no git commit mentions the slug".to_string(),
            json!({ "phase_events": dev_impl_events, "commits": commits.clone() }),
        ));
    }

    // C2: dev_impl claimed → there should be sacred_timeline rows for entity_id.
    if has_phase("dev_impl") && timeline.is_empty() {
        findings.push(finding(
            "warning",
            "DEV_IMPL_NO_SACRED",
            format!("card claims dev_impl but sacred_timeline has no rows for entity_id='{card_slug}'"),
            json!({ "timeline_count": timeline.len() as i64 }),
        ));
    }

    // C3: pl_draft claimed → phase_events must not be empty (defensive; structurally unreachable
    // since phases_claimed is derived FROM phase_events — ported for fidelity regardless).
    if has_phase("pl_draft") && phase_events.is_empty() {
        findings.push(finding(
            "critical",
            "PHASE_INCONSISTENT",
            "pl_draft in set but phase_events array is empty — logic error".to_string(),
            json!({}),
        ));
    }

    // C4: tl_sign claimed → there must exist a corresponding contract row.
    if has_phase("tl_sign") {
        let contract_v = crate::db_json::query_rows(
            db,
            "SELECT slug, status FROM contracts WHERE card_slug = ? AND status IN ('tl_approved','human_approved','impl','qa','done')",
            &[json!(card_slug)],
        )?;
        if contract_v.as_array().map(|a| a.is_empty()).unwrap_or(true) {
            findings.push(finding(
                "critical",
                "TL_SIGN_NO_CONTRACT",
                "card claims tl_sign but no contract at status ≥ tl_approved is linked to this card_slug".to_string(),
                json!({ "searched": "contracts WHERE card_slug=? AND status >= tl_approved" }),
            ));
        }
    }

    // C5: qa_pass claimed → must be after dev_impl.
    if has_phase("qa_pass") {
        let dev_impl_evt = phase_events.iter().find(|e| e.get("phase").and_then(Value::as_str) == Some("dev_impl"));
        let qa_pass_evt = phase_events.iter().find(|e| e.get("phase").and_then(Value::as_str) == Some("qa_pass"));
        match dev_impl_evt {
            None => {
                findings.push(finding(
                    "critical",
                    "QA_PASS_BEFORE_DEV_IMPL",
                    "qa_pass claimed but no preceding dev_impl event".to_string(),
                    json!({ "qa_pass_evt": qa_pass_evt.cloned().unwrap_or(Value::Null) }),
                ));
            }
            Some(dev_evt) => {
                if let Some(qa_evt) = qa_pass_evt {
                    let qa_ts = qa_evt.get("ts").and_then(Value::as_str).unwrap_or("");
                    let dev_ts = dev_evt.get("ts").and_then(Value::as_str).unwrap_or("");
                    if qa_ts < dev_ts {
                        findings.push(finding(
                            "critical",
                            "QA_PASS_TIMESTAMP_BEFORE_DEV_IMPL",
                            "qa_pass timestamp precedes dev_impl — phase order violated".to_string(),
                            json!({ "qa_pass_ts": qa_evt.get("ts").cloned().unwrap_or(Value::Null), "dev_impl_ts": dev_evt.get("ts").cloned().unwrap_or(Value::Null) }),
                        ));
                    }
                }
            }
        }
    }

    // C6: card status=done → all 4 merge-gate phases should be present.
    if card_status.as_str() == Some("done") {
        const REQUIRED: [&str; 4] = ["pl_draft", "tl_sign", "dev_impl", "qa_pass"];
        let missing: Vec<&str> = REQUIRED.iter().copied().filter(|p| !has_phase(p)).collect();
        if !missing.is_empty() {
            findings.push(finding(
                "critical",
                "DONE_MISSING_PHASES",
                format!("card status='done' but missing phase events: {}", missing.join(", ")),
                json!({ "missing": missing, "claimed": phases_claimed.clone() }),
            ));
        }
    }

    // C7: commits without phase events.
    if !commits.is_empty() && phase_events.is_empty() {
        findings.push(finding(
            "warning",
            "COMMITS_WITHOUT_PHASE_EVENTS",
            format!("{} commit(s) reference this slug but no phase_events recorded", commits.len()),
            json!({ "commit_count": commits.len() as i64, "sample": commits.iter().take(3).cloned().collect::<Vec<_>>() }),
        ));
    }

    let critical_count = findings.iter().filter(|f| f["severity"] == json!("critical")).count() as i64;
    let warning_count = findings.iter().filter(|f| f["severity"] == json!("warning")).count() as i64;

    let mut summary = Map::new();
    summary.insert("phase_events_count".into(), json!(phase_events.len() as i64));
    summary.insert("commits_count".into(), json!(commits.len() as i64));
    summary.insert("sacred_timeline_count".into(), json!(timeline.len() as i64));
    summary.insert("memories_count".into(), json!(memories.len() as i64));
    summary.insert("migrations_since_open".into(), json!(migrations_since.len() as i64));

    let mut sources = Map::new();
    sources.insert("phase_events".into(), json!(phase_events));
    sources.insert("commits".into(), json!(commits));
    sources.insert("timeline".into(), json!(timeline));
    sources.insert("memories".into(), json!(memories));
    sources.insert("migrations_since".into(), json!(migrations_since));

    let mut out = Map::new();
    out.insert("card_slug".into(), json!(card_slug));
    out.insert("card_status".into(), card_status);
    out.insert("summary".into(), Value::Object(summary));
    out.insert("sources".into(), Value::Object(sources));
    out.insert("findings".into(), json!(findings));
    out.insert("critical_count".into(), json!(critical_count));
    out.insert("warning_count".into(), json!(warning_count));
    out.insert("ok".into(), json!(critical_count == 0));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const SCHEMA: &str = "
        CREATE TABLE cards (slug TEXT PRIMARY KEY, title TEXT, status TEXT NOT NULL DEFAULT 'backlog', created_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00');
        CREATE TABLE card_phase_events (id INTEGER PRIMARY KEY AUTOINCREMENT, card_slug TEXT NOT NULL, phase TEXT NOT NULL, actor TEXT NOT NULL, ts TEXT NOT NULL DEFAULT '2026-01-01 00:00:00', evidence_json TEXT);
        CREATE TABLE schema_migrations (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT, applied_at TEXT);
        CREATE TABLE sacred_timeline (id INTEGER PRIMARY KEY AUTOINCREMENT, entity_type TEXT, entity_id TEXT, operation TEXT, actor TEXT, ts TEXT, user_goal_id INTEGER);
        CREATE TABLE contracts (id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT, card_slug TEXT, status TEXT);
        CREATE TABLE memory.memories (
          id INTEGER PRIMARY KEY AUTOINCREMENT, type TEXT NOT NULL, title TEXT NOT NULL, situation TEXT NOT NULL,
          guidance TEXT NOT NULL, tags_json TEXT NOT NULL DEFAULT '[]', created_by TEXT NOT NULL,
          created_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00'
        );
    ";

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS memory;").unwrap();
        c.execute_batch(SCHEMA).unwrap();
        c
    }
    fn add_card(c: &Connection, slug: &str, status: &str, created_at: &str) {
        c.execute("INSERT INTO cards (slug, title, status, created_at) VALUES (?1, 't', ?2, ?3)", rusqlite::params![slug, status, created_at]).unwrap();
    }
    fn add_phase(c: &Connection, slug: &str, phase: &str, ts: &str) {
        c.execute("INSERT INTO card_phase_events (card_slug, phase, actor, ts) VALUES (?1, ?2, 'test', ?3)", rusqlite::params![slug, phase, ts]).unwrap();
    }

    #[test]
    fn not_found_when_card_does_not_exist() {
        let c = db();
        let e = manAuditTruthCrossCheck(&json!({ "card_slug": "GHOST" }), &c).unwrap_err();
        assert_eq!(e, "man-audit-truth-cross-check: card 'GHOST' not found");
    }

    #[test]
    fn required_card_slug() {
        let c = db();
        assert_eq!(manAuditTruthCrossCheck(&json!({}), &c).unwrap_err(), "man-audit-truth-cross-check: card_slug is required");
    }

    #[test]
    fn clean_card_with_no_claims_ok_true_zero_findings() {
        let c = db();
        add_card(&c, "CARD-CLEAN", "backlog", "2026-01-01 00:00:00");
        let r = manAuditTruthCrossCheck(&json!({ "card_slug": "CARD-CLEAN" }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["critical_count"], json!(0));
        assert_eq!(r["warning_count"], json!(0));
        assert_eq!(r["summary"]["phase_events_count"], json!(0));
        assert_eq!(r["summary"]["memories_count"], json!(0)); // situation_md bug — always 0
        assert_eq!(r["findings"], json!([]));
        let keys: Vec<&String> = r.as_object().unwrap().keys().collect();
        assert_eq!(keys, vec!["card_slug", "card_status", "summary", "sources", "findings", "critical_count", "warning_count", "ok"]);
    }

    #[test]
    fn dev_impl_claimed_but_no_git_or_sacred_critical_findings() {
        let c = db();
        add_card(&c, "CARD-FALSE-CLAIM-DEV", "backlog", "2026-01-01 00:00:00");
        add_phase(&c, "CARD-FALSE-CLAIM-DEV", "dev_impl", "2026-01-01 00:00:00");
        let r = manAuditTruthCrossCheck(&json!({ "card_slug": "CARD-FALSE-CLAIM-DEV" }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        let codes: Vec<&str> = r["findings"].as_array().unwrap().iter().map(|f| f["code"].as_str().unwrap()).collect();
        assert!(codes.contains(&"DEV_IMPL_NO_COMMIT"));
        assert!(codes.contains(&"DEV_IMPL_NO_SACRED"));
    }

    #[test]
    fn status_done_but_missing_required_phases_critical() {
        let c = db();
        add_card(&c, "CARD-DONE-EMPTY", "done", "2026-01-01 00:00:00");
        let r = manAuditTruthCrossCheck(&json!({ "card_slug": "CARD-DONE-EMPTY" }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        let f = r["findings"].as_array().unwrap().iter().find(|f| f["code"] == json!("DONE_MISSING_PHASES")).unwrap();
        let mut missing: Vec<String> = f["evidence"]["missing"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
        missing.sort();
        assert_eq!(missing, vec!["dev_impl", "pl_draft", "qa_pass", "tl_sign"]);
    }

    #[test]
    fn qa_pass_with_timestamp_before_dev_impl_critical() {
        let c = db();
        add_card(&c, "CARD-TIME-WARP", "backlog", "2026-01-01 00:00:00");
        add_phase(&c, "CARD-TIME-WARP", "qa_pass", "2020-01-01 00:00:00");
        add_phase(&c, "CARD-TIME-WARP", "dev_impl", "2026-01-01 00:00:00");
        let r = manAuditTruthCrossCheck(&json!({ "card_slug": "CARD-TIME-WARP" }), &c).unwrap();
        let codes: Vec<&str> = r["findings"].as_array().unwrap().iter().map(|f| f["code"].as_str().unwrap()).collect();
        assert!(codes.contains(&"QA_PASS_TIMESTAMP_BEFORE_DEV_IMPL"));
    }

    #[test]
    fn qa_pass_with_no_preceding_dev_impl_critical() {
        let c = db();
        add_card(&c, "CARD-QA-ALONE", "backlog", "2026-01-01 00:00:00");
        add_phase(&c, "CARD-QA-ALONE", "qa_pass", "2026-01-01 00:00:00");
        let r = manAuditTruthCrossCheck(&json!({ "card_slug": "CARD-QA-ALONE" }), &c).unwrap();
        let codes: Vec<&str> = r["findings"].as_array().unwrap().iter().map(|f| f["code"].as_str().unwrap()).collect();
        assert!(codes.contains(&"QA_PASS_BEFORE_DEV_IMPL"));
    }

    #[test]
    fn tl_sign_claimed_but_no_matching_contract_critical() {
        let c = db();
        add_card(&c, "CARD-NO-CON", "backlog", "2026-01-01 00:00:00");
        add_phase(&c, "CARD-NO-CON", "tl_sign", "2026-01-01 00:00:00");
        let r = manAuditTruthCrossCheck(&json!({ "card_slug": "CARD-NO-CON" }), &c).unwrap();
        let codes: Vec<&str> = r["findings"].as_array().unwrap().iter().map(|f| f["code"].as_str().unwrap()).collect();
        assert!(codes.contains(&"TL_SIGN_NO_CONTRACT"));
    }

    #[test]
    fn tl_sign_with_matching_contract_suppresses_finding() {
        let c = db();
        add_card(&c, "CARD-CON-OK", "backlog", "2026-01-01 00:00:00");
        add_phase(&c, "CARD-CON-OK", "tl_sign", "2026-01-01 00:00:00");
        c.execute("INSERT INTO contracts (slug, card_slug, status) VALUES ('CT-1', 'CARD-CON-OK', 'tl_approved')", []).unwrap();
        let r = manAuditTruthCrossCheck(&json!({ "card_slug": "CARD-CON-OK" }), &c).unwrap();
        let codes: Vec<&str> = r["findings"].as_array().unwrap().iter().map(|f| f["code"].as_str().unwrap()).collect();
        assert!(!codes.contains(&"TL_SIGN_NO_CONTRACT"));
    }

    #[test]
    fn dev_impl_with_sacred_timeline_rows_suppresses_dev_impl_no_sacred() {
        let c = db();
        add_card(&c, "CARD-CONSISTENT", "backlog", "2026-01-01 00:00:00");
        c.execute("INSERT INTO sacred_timeline (entity_type, entity_id, operation, actor, ts) VALUES ('card', 'CARD-CONSISTENT', 'card.update', 'test', '2026-01-01 00:00:00')", []).unwrap();
        add_phase(&c, "CARD-CONSISTENT", "dev_impl", "2026-01-01 00:00:00");
        let r = manAuditTruthCrossCheck(&json!({ "card_slug": "CARD-CONSISTENT" }), &c).unwrap();
        let codes: Vec<&str> = r["findings"].as_array().unwrap().iter().map(|f| f["code"].as_str().unwrap()).collect();
        assert!(!codes.contains(&"DEV_IMPL_NO_SACRED"));
        assert!(r["summary"]["sacred_timeline_count"].as_i64().unwrap() > 0);
    }

    // ── git_log_for_slug: proves the real subprocess + parser against a scratch git repo ──

    fn scratch_repo(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-truth-cross-check-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let run = |args: &[&str]| {
            let st = crate::proc::cmd("git").args(args).current_dir(&dir).status().unwrap();
            assert!(st.success(), "git {args:?} failed");
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "test@example.com"]);
        run(&["config", "user.name", "Test"]);
        fs::write(dir.join("f.txt"), "hello").unwrap();
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "implement FEAT-SLUG-123 happy path"]);
        fs::write(dir.join("f.txt"), "hello again").unwrap();
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "unrelated change"]);
        dir
    }

    #[test]
    fn git_log_for_slug_finds_matching_commit() {
        let dir = scratch_repo("hit");
        let commits = git_log_for_slug(&dir, "FEAT-SLUG-123");
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0]["subject"], json!("implement FEAT-SLUG-123 happy path"));
        assert_eq!(commits[0]["sha"].as_str().unwrap().len(), 12);
        assert!(commits[0]["committed_at"].as_str().unwrap().starts_with("20"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn git_log_for_slug_no_match_returns_empty() {
        let dir = scratch_repo("miss");
        let commits = git_log_for_slug(&dir, "NO-SUCH-SLUG-XYZ");
        assert_eq!(commits, Vec::<Value>::new());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn git_log_for_slug_non_git_dir_returns_empty() {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-truth-cross-check-rs-notgit-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let commits = git_log_for_slug(&dir, "ANYTHING");
        assert_eq!(commits, Vec::<Value>::new());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn full_pipeline_with_real_repo_root_has_no_commits_on_this_checkout() {
        // repo_root() (CARGO_MANIFEST_DIR's parent) is not itself a git repo on this checkout —
        // see module doc — so the full function's git branch always degrades to [] here, same as
        // the JS test suite observes ("test environment isn't a real git repo with commits").
        let c = db();
        add_card(&c, "CARD-REAL-ROOT", "backlog", "2026-01-01 00:00:00");
        add_phase(&c, "CARD-REAL-ROOT", "dev_impl", "2026-01-01 00:00:00");
        let r = manAuditTruthCrossCheck(&json!({ "card_slug": "CARD-REAL-ROOT" }), &c).unwrap();
        assert_eq!(r["summary"]["commits_count"], json!(0));
    }
}
